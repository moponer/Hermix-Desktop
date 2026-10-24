//! 工作台（Workbench）存储层 —— 目录骨架、manifest、文件读写与骨架契约校验。
//!
//! 与 custom app（分发型：独立进程 + 打包 exe）不同，工作台是**演进型**：
//! 在宿主内运行、靠对话迭代、无需打包。因此本模块只管「代码层」的落盘与
//! 契约校验；数据层（`data/`）对代码层是白名单禁区，由宿主网关独占。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hermix_config::paths::hermix_home;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::workbench_lint::{lint_files, Violation};

/// 宿主注入给工作台的 SDK（最小版）。放在共享 vendor 区，多工作台共用一份。
const WB_SDK_JS: &str = r#"// 宿主 SDK —— 工作台代码只能通过它访问数据与本机能力。
// 直接调用 fs / fetch / child_process 在架构上不可达：iframe 内没有这些 API。
// 全部调用都是异步请求/响应：本文件只负责发消息与兑现 Promise，
// 真正的权限判定与数据读写在宿主侧（Rust 网关）。
(function () {
  var seq = 0;
  var waiting = {};
  var listeners = {};

  function request(method, params) {
    var id = ++seq;
    return new Promise(function (resolve, reject) {
      waiting[id] = { resolve: resolve, reject: reject };
      window.parent.postMessage(
        { source: 'hermix-workbench', type: 'request', id: id, method: method, params: params },
        '*'
      );
    });
  }

  window.wb = {
    /** 受限数据访问：只读语句。数据与代码分离，代码回滚不碰数据库。 */
    db: {
      query: function (sql, params) { return request('db.query', { sql: sql, params: params || [] }); },
      exec: function (sql, params) { return request('db.exec', { sql: sql, params: params || [] }); },
    },
    /** 本机文件：直接用，无授权步骤。仅密钥/凭据类路径与工作台自己的 data/ 越权拒绝。 */
    fs: {
      read: function (path) { return request('fs.read', { path: path }); },
      write: function (path, content) { return request('fs.write', { path: path, content: content }); },
      list: function (dir) { return request('fs.list', { path: dir }); },
    },
    /** 向宿主发事件（右侧对话栏据此触发 agent）。 */
    emit: function (event, payload) {
      window.parent.postMessage(
        { source: 'hermix-workbench', type: 'event', event: event, payload: payload },
        '*'
      );
    },
    /** 订阅宿主下发事件。 */
    on: function (event, fn) {
      (listeners[event] = listeners[event] || []).push(fn);
    },
    /** 算法工具箱：虚拟滚动 / diff / CSV / 统计。只给算法，不给外观。 */
    util: window.__wbTools || {},
  };

  window.addEventListener('message', function (e) {
    var d = e.data;
    if (!d || d.source !== 'hermix-host') return;
    if (d.type === 'response') {
      var w = waiting[d.id];
      if (!w) return;
      delete waiting[d.id];
      if (d.ok) w.resolve(d.result);
      else w.reject(new Error(d.error || '宿主拒绝了该请求'));
      return;
    }
    if (d.type === 'event') {
      (listeners[d.event] || []).forEach(function (fn) { fn(d.payload); });
    }
  });

  // 未开发工作台的默认底：骨架文档没有任何背景规则，iframe 画布会露出父
  // 容器的底色。把 html 背景设为透明 → 父容器 .wb-canvas 的
  // var(--color-bg-base)（浅色 #f5f5f5 / 暗色 #1f1f1f）透出来 —— 画布与
  // 右侧对话的纸白拉开一档，读作"应用里的一块画布"而不是"没加载出来的
  // 网页"（2026-10-24 用户实机反馈：命名后还没开发完时画布一直是白色
  // 网页背景）。
  //
  // AI 之后想自定义整页底色：写 body 的 background 即可（html 透明时
  // body 的背景按 CSS 规范就是画布底），或给 html 加 !important ——
  // 宿主不阻拦。
  //
  // 宿主侧跨域读 contentDocument 的兜底（WorkbenchShell）在 asset 协议下
  // **永远静默失效**（iframe 是 asset:// 另一个源，contentDocument 为
  // null），这里是唯一真正生效的通道 —— SDK 跑在 iframe 自己的文档里。
  document.documentElement.style.setProperty('background', 'transparent');

  // 右键：不让浏览器弹"网页菜单"（重新加载/检查/后台打开），改由宿主弹
  // 应用菜单。跨源，宿主读不到本 iframe 的 DOM —— 只能把坐标、选区、
  // 链接转发过去，宿主用同一套菜单渲染（坐标按 iframe 位置换算）。
  document.addEventListener('contextmenu', function (e) {
    e.preventDefault();
    var sel = window.getSelection ? String(window.getSelection() || '') : '';
    var anchor = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    window.parent.postMessage({
      source: 'hermix-workbench',
      type: 'contextmenu',
      x: e.clientX,
      y: e.clientY,
      text: sel,
      href: anchor ? anchor.href : ''
    }, '*');
  });
})();
"#;

/// 宿主锁定文件 —— AI 不可写，写入即拒绝。
/// `main.js` 锁定让入口不是 AI 的画布；`tokens.css` 锁定保证主题变量不被绕过。
const LOCKED_FILES: [&str; 2] = ["src/main.js", "styles/tokens.css"];

/// 单文件行数上限 —— 超出即拒绝，使「一个页面写完」物理上做不到。
fn max_lines(rel: &str) -> usize {
    if rel.ends_with(".css") {
        300
    } else if rel.ends_with(".html") {
        150
    } else {
        200
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchManifest {
    pub id: String,
    pub name: String,
    /// 绑定的持久会话；右侧对话栏靠它跨会话保留上下文。
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    /// 依赖白名单内的第三方库名，宿主据此注入 importmap。
    #[serde(default)]
    pub deps: Vec<String>,
    /// 主工作台标记。工作台页面默认显示主台，其余靠上方 tab 切换。
    /// 全局至多一个为 true；设新主台时旧标记由 `set_primary` 清掉。
    #[serde(default)]
    pub is_primary: bool,
}

pub fn workbenches_dir() -> PathBuf {
    hermix_home().join("workbenches")
}

pub fn workbench_dir(id: &str) -> PathBuf {
    workbenches_dir().join(id)
}

/// 模块索引 —— 迭代前注入给 agent 的「已有能力清单」。
/// 没有它，agent 每次迭代都重新写一个 `formatDate`；有了它才会 import 已有模块。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleEntry {
    pub file: String,
    pub exports: Vec<String>,
}

pub fn module_index(id: &str) -> Result<Vec<ModuleEntry>, String> {
    let re = regex_lite::Regex::new(
        r"export\s+(?:async\s+)?(?:function|const|class)\s+([A-Za-z_$][\w$]*)",
    )
    .map_err(|e| format!("bad regex: {e}"))?;
    let mut out = Vec::new();
    for rel in list_code_files(id)? {
        if !rel.ends_with(".js") {
            continue;
        }
        let text = fs::read_to_string(workbench_dir(id).join("code").join(&rel))
            .map_err(|e| format!("read {rel}: {e}"))?;
        let mut exports: Vec<String> = re
            .captures_iter(&text)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        if exports.is_empty() {
            continue;
        }
        exports.sort();
        exports.dedup();
        out.push(ModuleEntry { file: rel, exports });
    }
    Ok(out)
}

/// 渲染成注入 prompt 的紧凑文本。一行一个模块，控制 token 成本。
pub fn format_module_index(entries: &[ModuleEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    entries
        .iter()
        .map(|m| format!("{}: {}", m.file, m.exports.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 撤销栈记录。追加式 JSONL —— 撤销是「回退最近一次」，不是版本树，
/// 用不着 git 级别的结构；真需要版本历史时再上快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoEntry {
    ts: u64,
    rel: String,
    backup: String,
}

fn undo_index_path(id: &str) -> PathBuf {
    workbench_dir(id).join("data/_undo/index.jsonl")
}

fn backup_before_write(id: &str, rel: &str, path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let ts = now_secs();
    let safe = rel.replace(['/', '\\'], "__");
    let backup_rel = format!("_undo/{}.{}.bak", safe, ts);
    let backup_path = workbench_dir(id).join("data").join(&backup_rel);
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create undo dir: {e}"))?;
    }
    fs::copy(path, &backup_path).map_err(|e| format!("backup {}: {}", rel, e))?;
    let entry = UndoEntry {
        ts,
        rel: rel.to_string(),
        backup: backup_rel,
    };
    let line = serde_json::to_string(&entry).map_err(|e| format!("serialize undo: {e}"))?;
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(undo_index_path(id))
        .map_err(|e| format!("open undo index: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("append undo index: {e}"))
}

/// 撤销最近一次代码改动。数据层不受影响 —— 这正是分离的意义。
pub fn undo_last(id: &str) -> Result<Option<String>, String> {
    let path = undo_index_path(id);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("read undo index: {e}"))?;
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let Some(last) = lines.pop() else {
        return Ok(None);
    };
    let entry: UndoEntry = serde_json::from_str(last).map_err(|e| format!("parse undo: {e}"))?;
    let backup = workbench_dir(id).join("data").join(&entry.backup);
    if backup.exists() {
        let target = resolve_code_path(id, &entry.rel)?;
        fs::copy(&backup, &target).map_err(|e| format!("restore {}: {}", entry.rel, e))?;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    fs::write(&path, out).map_err(|e| format!("rewrite undo index: {e}"))?;
    Ok(Some(entry.rel))
}

/// 可撤销的改动列表（最近在前）。
pub fn undo_history(id: &str) -> Vec<String> {
    let path = undo_index_path(id);
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<UndoEntry>(l).ok())
        .map(|e| e.rel)
        .collect()
}

/// importmap 标记段 —— 宿主只在两行标记之间重写，AI 写的其余部分不动。
const MAP_START: &str = "<!-- wb:importmap:start -->";
const MAP_END: &str = "<!-- wb:importmap:end -->";

/// 共享 vendor 区 —— 多工作台共用同一份 SDK 与第三方库，不逐台拷贝。
pub fn vendor_dir() -> PathBuf {
    workbenches_dir().join("_vendor")
}

/// 算法工具箱源码的落点 —— 工作台可直接 `import { diffLines } from 'wb-tools'`。
pub fn vendor_tools_js() -> PathBuf {
    vendor_dir().join("wb-tools.js")
}

/// 把宿主 SDK 与算法工具箱落到共享 vendor 区。
///
/// SDK 里带 `__wbTools`（同步可用），工具箱另出一份 ESM（按需 import）。
/// 内容无变则跳过重写，避免无谓的文件变更与缓存失效。
fn ensure_vendor() -> Result<(), String> {
    let dir = vendor_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create _vendor: {}", e))?;

    let tools = crate::WB_TOOLS_TS;
    let sdk_path = dir.join("wb-sdk.js");
    let sdk = WB_SDK_JS.replace("window.__wbTools || {}", &tools_inline_iife(tools));
    if !fs::read_to_string(&sdk_path).map(|s| s == sdk).unwrap_or(false) {
        fs::write(&sdk_path, &sdk).map_err(|e| format!("write wb-sdk.js: {}", e))?;
    }
    let tools_path = vendor_tools_js();
    if !fs::read_to_string(&tools_path).map(|s| s == tools).unwrap_or(false) {
        fs::write(&tools_path, tools).map_err(|e| format!("write wb-tools.js: {}", e))?;
    }
    Ok(())
}

/// 工具箱以 TS 维护，注入 iframe 前必须剥掉类型 —— 浏览器不认 TS。
/// 只处理本项目用到的几种注解形态（type/interface 块、参数与返回注解、
/// `as` 断言、`export`/`import type`），不引 esbuild：几行正则就够，
/// 且工具箱是宿主代码，形态由我们自己控。
fn strip_typescript(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block_comment = false;
    // 声明丢弃用独立的「花括号深度」记账，不能跟注释状态共用一个标志位：
    // 注释靠 `*/` 退出、声明靠 `}` 退出，共用一个 bool 会互相污染。
    let mut decl_depth: Option<i32> = None;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(depth) = decl_depth {
            // 声明体按花括号计深；`};` / 单行 `type X = Y;` 同样在这里收尾。
            let next = depth + count_delta(line);
            if next <= 0 {
                decl_depth = None;
            } else {
                decl_depth = Some(next);
            }
            continue;
        }
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("/*") {
            in_block_comment = !trimmed.contains("*/");
            continue;
        }
        // interface / type 声明整块丢弃（含多行体）
        if trimmed.starts_with("export interface")
            || trimmed.starts_with("interface ")
            || trimmed.starts_with("export type ")
            || trimmed.starts_with("import type ")
        {
            let depth = count_delta(line);
            if depth > 0 {
                decl_depth = Some(depth);
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    strip_annotations(&out)
}

/// 一行里的花括号净增量。声明行末尾的 `{` 让它成为正数，字段里出现的泛型
/// 方括号与对象字面量都不含花括号，所以只看 `{}` 足够。
fn count_delta(line: &str) -> i32 {
    let mut delta = 0;
    for ch in line.chars() {
        match ch {
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// 剥参数/返回注解与 `as X` 断言。
///
/// 保留 `export` —— 两条投递路径对导出的需求正好相反：
/// `wb-tools.js` 供 `import { diffLines } from 'wb-tools'`，必须有 export；
/// `__wbTools` 对象字面量则靠 `tools_object_literal` 逐个取值。剥类型阶段
/// 统一保留，由调用方去留。
fn strip_annotations(src: &str) -> String {
    let mut s = src.to_string();
    // 返回类型注解：`) : Type {` / `) : Type =>`
    let re_ret = regex_lite::Regex::new(r"\)\s*:\s*[A-Za-z_][\w<>\[\]|, .]*(\s*[{=>])").unwrap();
    s = re_ret.replace_all(&s, ")${1}").to_string();
    // 参数注解必须在函数签名的括号内才成立。不能裸跑正则 ——
    // 对象字面量 `{ left: a[i], right: b[i] }` 会被误剥成 `{ left, right }`。
    s = strip_param_annotations(&s);
    // 局部变量注解：`const x: Type[] = ...`
    s = strip_var_annotations(&s);
    // as 断言
    let re_as = regex_lite::Regex::new(r"\s+as\s+[A-Za-z_][\w<>\[\]|.]*").unwrap();
    s = re_as.replace_all(&s, "").to_string();
    // 泛型调用实参：`new Array<number>(n)` / `foo<Type>(x)` —— TS 专有语法，
    // 留着会变成运行时 `number is not defined`（esbuild 的 js loader 也放行）。
    // 泛型名不一定是大驼峰（`number` / `string` 是内置原始类型），所以不按大小写
    // 过滤，改为靠「`<...>` 后紧跟 `(`」这个形态约束 —— JS 里比较运算不会贴着写。
    let re_generic = regex_lite::Regex::new(r"<[A-Za-z_][\w<>\[\]|, .]*>(\s*\()").unwrap();
    s = re_generic.replace_all(&s, "${1}").to_string();
    s
}

/// 剥函数签名括号内的参数注解。只在「函数声明的参数列表」里生效：
/// 找到 `function name(` 或 `=>` 之后的 `(`，在其配对括号内逐参剥注解。
/// 对象字面量、数组下标、调用实参都不进这个范围，天然免疫误伤。
fn strip_param_annotations(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        // 命中函数签名参数列表的起点
        if b[i] == '(' && is_signature_paren(&out) {
            out.push('(');
            i += 1;
            let mut depth = 1i32;
            while i < b.len() && depth > 0 {
                let c = b[i];
                if c == '(' {
                    depth += 1;
                    out.push(c);
                    i += 1;
                    continue;
                }
                if c == ')' {
                    depth -= 1;
                    out.push(c);
                    i += 1;
                    continue;
                }
                if depth == 1 && c == ':' {
                    let end = type_run_end(&b, i + 1);
                    if end > i + 1 && in_decl_position(&out) {
                        i = end;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// 已输出的末尾是不是「函数声明刚写到参数列表的 `(`」。
/// 判据：`(` 之前是标识符（函数名），再往前找关键字 `function` 或箭头 `=>`。
fn is_signature_paren(out: &str) -> bool {
    let tail: String = out.chars().rev().take(120).collect();
    let mut it = tail.chars().skip_while(|c| *c == ' ');
    match it.next() {
        Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => {}
        _ => return false,
    }
    // 越过函数名，剩下的部分是反序的关键字/箭头
    let rest: String = it
        .skip_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    let rest = rest.trim_start();
    rest.starts_with("noitcnuf")       // `function` 的反序
        || rest.starts_with('>')       // `=>` 的 `>` 端
        || rest.starts_with("suounoyna") // `anonymous` 之类的兜底
}

/// 剥局部变量类型注解。逐字符扫，跳过字符串字面量与注释 ——
/// 工具箱里 `if (ch === '"')` 这类代码含冒号与引号，直接跑正则必误伤。
///
/// 命中条件：冒号左侧是声明位置（`const`/`let`/`var`/参数），右侧是紧贴
/// `=` / `;` / `)` / `,` 的一段纯类型文本。靠「左侧上下文」而非「右侧像类型」
/// 来判定 —— 对象字面量 `{ a: b }` 的冒号左侧没有声明关键字，天然排除。
fn strip_var_annotations(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // 字符串字面量整段照抄，内部内容不参与判定
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                if b[i] == '\\' && i + 1 < b.len() {
                    out.push(b[i + 1]);
                    i += 2;
                    continue;
                }
                if b[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < b.len() && (b[i + 1] == '/' || b[i + 1] == '*') {
            let block = b[i + 1] == '*';
            while i < b.len() {
                out.push(b[i]);
                if block {
                    if b[i] == '*' && i + 1 < b.len() && b[i + 1] == '/' {
                        out.push('/');
                        i += 2;
                        break;
                    }
                } else if b[i] == '\n' {
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == ':' {
            let end = type_run_end(&b, i + 1);
            if end > i + 1 && in_decl_position(&out) {
                i = end;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 冒号左侧是不是声明位置。往回看最近一个非空白 token：
/// - 是 `const` / `let` / `var` → 变量声明，成立
/// - 是 `(` 或 `,`（且括号未闭合）→ 参数列表，成立
/// 其余一律不成立，避免误伤对象字面量与三元表达式。
fn in_decl_position(out: &str) -> bool {
    let tail: Vec<char> = out.chars().rev().take(200).collect();
    let mut i = 0;
    // 跳过刚输出的标识符本身
    while i < tail.len() && (tail[i].is_alphanumeric() || tail[i] == '_' || tail[i] == '$') {
        i += 1;
    }
    // 空白含换行 —— 多行参数列表的冒号前面是 `\n`，只跳空格会漏。
    while i < tail.len() && tail[i].is_whitespace() {
        i += 1;
    }
    if i >= tail.len() {
        return false;
    }
    match tail[i] {
        // 参数列表里：往回找未闭合的 `(`，且中途不越过 `=>` 之类的箭头体
        '(' | ',' => {
            let mut depth = 0i32;
            for c in tail[i..].iter() {
                match c {
                    ')' => depth -= 1,
                    '(' => {
                        if depth == 0 {
                            return true;
                        }
                        depth += 1;
                    }
                    ';' | '{' | '}' => return false,
                    _ => {}
                }
            }
            false
        }
        _ => {
            // tail 是反序，取出的词要再反回来才是原词
            let word: String = tail[i..]
                .iter()
                .take_while(|c| c.is_alphanumeric() || **c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            matches!(word.as_str(), "const" | "let" | "var")
        }
    }
}

/// 从 `start` 起吃掉一段类型文本，返回结束位置（不含）。
/// 类型文本形态：`Type` / `Type[]` / `A | B` / `Record<K, V>` / `{ a: number }`。
/// 只允许大写开头的标识符、点号、方括号、尖括号、竖线、逗号与空格。
fn type_run_end(b: &[char], start: usize) -> usize {
    let mut i = start;
    let mut depth = 0i32;
    let mut saw_type = false;
    while i < b.len() {
        let c = b[i];
        match c {
            '<' | '[' | '{' => {
                depth += 1;
                i += 1;
            }
            '>' | ']' | '}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                i += 1;
            }
            // `,` 与 `|` 只在泛型/联合类型内部合法。参数列表的分隔逗号在 depth 0，
            // 必须在这里停住，否则会把下一个参数一起吃掉。
            ',' | '|' if depth > 0 => {
                i += 1;
            }
            '|' if depth == 0 => {
                i += 1;
            }
            '.' | ' ' => {
                i += 1;
            }
            _ if c.is_alphanumeric() || c == '_' => {
                saw_type = true;
                i += 1;
            }
            _ => break,
        }
    }
    if !saw_type {
        return start;
    }
    // 收尾必须是 `=`、`;`、`)`、`,` 或换行（多行参数列表每个参数以 `,\n` 结尾）。
    let end = b[start..i]
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map(|p| start + p + 1)
        .unwrap_or(start);
    let mut probe = end;
    while probe < b.len() && b[probe] == ' ' {
        probe += 1;
    }
    match b.get(probe) {
        Some('=') | Some(';') | Some(')') | Some(',') | Some('\n') | None => end,
        _ => start,
    }
}

/// `out` 的末尾是不是「标识符刚结束」—— 即紧邻冒号左侧是一个变量名。

/// 工具箱剥类型并去掉 export 后的纯 JS —— 函数名与 TS 源一致。
fn stripped_tools_js(tools_ts: &str) -> String {
    // 对象字面量/IIFE 内不能有 export，这里逐处去掉；`wb-tools.js` 那条路径保留。
    strip_typescript(tools_ts)
        .replace("export function ", "function ")
        .replace("export const ", "const ")
}

/// 把剥完类型的工具箱包成一个 `{ virtualWindow, diffLines, parseDelimited, columnStats }`
/// 字面量，挂给 `__wbTools`。
fn tools_object_literal(tools_ts: &str) -> String {
    let js = stripped_tools_js(tools_ts);
    let names = [
        "virtualWindow",
        "diffLines",
        "parseDelimited",
        "columnStats",
    ];
    let mut fields = Vec::new();
    for name in names {
        if js.contains(&format!("function {name}")) || js.contains(&format!("const {name}")) {
            fields.push(format!("{name}: {name}"));
        }
    }
    format!("{{\n    {}\n  }}", fields.join(",\n    "))
}

/// SDK 内联工具箱：IIFE 里**先定义函数**再返回字面量。
/// 不能只注入 `{ name: name }` 字面量 —— 函数在 wb-sdk.js 里没有定义，
/// 生成文件一执行就 `ReferenceError: virtualWindow is not defined`，
/// window.wb 整个挂载失败、工作台全白屏（2026-10-15 用户实机踩中）。
fn tools_inline_iife(tools_ts: &str) -> String {
    format!(
        "(function () {{\n{}\n  return {};\n  }})()",
        stripped_tools_js(tools_ts),
        tools_object_literal(tools_ts)
    )
}

/// 已安装的第三方库清单。放在 `_vendor/index.json` 而非硬编码进二进制：
/// 「依赖申请」批准后往这里加一条即可生效，宿主代码不用改、不用重新编译。
/// 键为 import 名，值为 `{version, entry}`。
pub fn vendor_index() -> std::collections::HashMap<String, VendorLib> {
    let path = vendor_dir().join("index.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorLib {
    pub version: String,
    pub entry: String,
}

/// 渲染 importmap 片段。路径相对 `code/` 目录指向共享 vendor 区。
fn render_importmap(deps: &[String]) -> String {
    let index = vendor_index();
    // 宿主工具箱始终可用，不占用户依赖配额：工作台 import 'wb-tools' 即可。
    let mut entries = vec![
        "      \"wb-tools\": \"../../_vendor/wb-tools.js\"".to_string(),
        "      \"wb-sdk\": \"../../_vendor/wb-sdk.js\"".to_string(),
    ];
    for dep in deps {
        if let Some(lib) = index.get(dep) {
            entries.push(format!(
                "      \"{}\": \"../../_vendor/{}/{}\"",
                dep, dep, lib.entry
            ));
        }
    }
    format!(
        "{}\n    <script type=\"importmap\">\n{{\n  \"imports\": {{\n{}\n  }}\n}}\n    </script>\n{}\n",
        MAP_START,
        entries.join(",\n"),
        MAP_END
    )
}

/// 为工作台设定依赖并把 importmap 写回 `index.html`。
/// 未安装的库一律拒绝并点名 —— agent 收到后走「依赖申请」，而不是自己去抄 CDN。
pub fn set_deps(id: &str, deps: &[String]) -> Result<Vec<String>, String> {
    let index = vendor_index();
    let missing: Vec<String> = deps
        .iter()
        .filter(|d| !index.contains_key(*d))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "未安装依赖: {} — 请改用依赖申请流程，不要从 CDN 引入",
            missing.join(", ")
        ));
    }

    let html_path = workbench_dir(id).join("code/index.html");
    let html = fs::read_to_string(&html_path).map_err(|e| format!("read index.html: {}", e))?;
    let block = render_importmap(deps);
    let updated = replace_between(&html, block)?;
    write_atomic(&html_path, &updated)?;

    let mut m = read_manifest(id)?;
    m.deps = deps.to_vec();
    m.updated_at = now_secs();
    write_manifest(&m)?;
    Ok(deps.to_vec())
}

/// 替换两个标记之间的内容；标记缺失时视为骨架被破坏，直接报错而不是静默跳过。
fn replace_between(html: &str, block: String) -> Result<String, String> {
    let (Some(s), Some(e)) = (html.find(MAP_START), html.find(MAP_END)) else {
        return Err("index.html 缺少 importmap 标记段（骨架被破坏）".to_string());
    };
    if e < s {
        return Err("index.html importmap 标记顺序异常".to_string());
    }
    Ok(format!(
        "{}{}{}",
        &html[..s],
        block.trim_end(),
        &html[e + MAP_END.len()..]
    ))
}

/// 原子写：tmp + rename。并发读者（iframe 刷新）不会读到半截文件。
fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, content).map_err(|e| format!("write tmp: {}", e))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 名称转 id：非字母数字归一为 `-`，并附毫秒时间戳避免同名冲突。
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let base = if slug.is_empty() {
        "workbench".to_string()
    } else {
        slug
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}-{}", base, millis)
}

/// 解析相对路径并拒绝逃逸（`..` / 绝对路径 / 盘符），保证写入必在工作台内。
fn resolve_code_path(id: &str, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_start_matches("./").replace('\\', "/");
    if rel.is_empty() {
        return Err("empty path".to_string());
    }
    if rel.starts_with('/') || rel.contains("..") || Path::new(&rel).is_absolute() {
        return Err(format!("illegal code path: {}", rel));
    }
    Ok(workbench_dir(id).join("code").join(&rel))
}

pub fn create_workbench(name: &str) -> Result<WorkbenchManifest, String> {
    let id = slugify(name);
    let dir = workbench_dir(&id);
    if dir.exists() {
        return Err(format!("workbench dir already exists: {}", id));
    }
    let code = dir.join("code");
    for sub in ["styles", "src/views", "src/components", "src/utils"] {
        fs::create_dir_all(code.join(sub))
            .map_err(|e| format!("create {}: {}", sub, e))?;
    }
    // 数据层与代码层物理分离：data/ 由宿主网关独占，代码层无权直写。
    fs::create_dir_all(dir.join("data/files")).map_err(|e| format!("create data: {}", e))?;
    fs::create_dir_all(dir.join("data/_undo")).map_err(|e| format!("create undo: {}", e))?;

    for (rel, content) in skeleton_files(name) {
        let path = code.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create parent: {}", e))?;
        }
        fs::write(&path, content).map_err(|e| format!("write {}: {}", rel, e))?;
    }

    ensure_vendor()?;

    let now = now_secs();
    let manifest = WorkbenchManifest {
        id: id.clone(),
        name: name.to_string(),
        session_id: None,
        created_at: now,
        updated_at: now,
        deps: Vec::new(),
        // 第一个工作台自动成为主台 —— 否则建完仍停在空态，
        // 用户得再手动指定一次才有东西可看。
        is_primary: list_workbenches().map(|v| v.is_empty()).unwrap_or(false),
    };
    write_manifest(&manifest)?;
    Ok(manifest)
}

/// 宿主预生成的骨架 —— AI 只在格子里填，不在空白页里自由发挥。
fn skeleton_files(name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("index.html", skeleton_html(name)),
        ("styles/tokens.css", skeleton_tokens()),
        ("styles/layout.css", "/* 布局：栅格与分区 */\n".to_string()),
        ("styles/components.css", "/* 组件样式：只能用 var(--token)，不许用优先级覆盖 */\n".to_string()),
        ("src/main.js", SKELETON_MAIN.to_string()),
        ("src/app.js", SKELETON_APP.to_string()),
        ("src/store.js", SKELETON_STORE.to_string()),
        ("src/api.js", SKELETON_API.to_string()),
        ("src/views/overview.js", SKELETON_VIEW.to_string()),
        ("src/components/card.js", SKELETON_CARD.to_string()),
        ("src/utils/format.js", SKELETON_FORMAT.to_string()),
    ]
}

fn skeleton_html(name: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>{name}</title>
    <link rel="stylesheet" href="./styles/tokens.css" />
    <link rel="stylesheet" href="./styles/layout.css" />
    <link rel="stylesheet" href="./styles/components.css" />
    <!-- importmap 由宿主注入与维护，标记段之间由宿主重写，AI 不可改 -->
    <!-- wb:importmap:start -->
    <!-- wb:importmap:end -->
  </head>
  <body>
    <div id="wb-root"></div>
    <!-- 宿主 SDK 必须在业务代码之前加载：window.wb 是工作台唯一的本机能力通道 -->
    <script src="../../_vendor/wb-sdk.js"></script>
    <script type="module" src="./src/main.js"></script>
  </body>
</html>
"#
    )
}

fn skeleton_tokens() -> String {
    r#"/* 主题变量：宿主生成并维护，AI 只能覆写右侧的值，不得新增结构。 */
:root {
  --wb-bg: #1c1c20;
  --wb-surface: #232328;
  --wb-border: #2e2e34;
  --wb-text: #e8e8e5;
  --wb-text-muted: #9a9a95;
  --wb-accent: #c96442;
  --wb-radius: 8px;
  --wb-gap: 12px;
}
"#
    .to_string()
}

/// 入口锁定：只做装配，不含业务逻辑，使 AI 无法把入口当画布。
const SKELETON_MAIN: &str = r#"// 宿主生成，AI 不可修改。职责：装配应用并注入宿主 SDK。
import { mountApp } from './app.js';

const root = document.getElementById('wb-root');
mountApp(root, window.wb);
"#;

const SKELETON_APP: &str = r#"// 视图装配入口：挂载各 view 模块。业务拆分请放 views/ 与 components/。
import { mountOverview, unmountOverview } from './views/overview.js';

/** 挂载应用。root 为本工作台根节点，wb 为宿主注入的 SDK。 */
export function mountApp(root, wb) {
  root.innerHTML = '';
  mountOverview(root, wb);
}

/** 卸载应用：必须释放本工作台绑定的全部监听。 */
export function unmountApp() {
  unmountOverview();
}
"#;

const SKELETON_STORE: &str = r#"// 唯一状态源。各模块不得自行保存状态，一律经此订阅与派发。
const listeners = new Set();
let state = {};

/** 订阅状态变更，返回取消订阅函数。 */
export function subscribe(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** 读取当前状态。 */
export function getState() {
  return state;
}

/** 合并更新状态并通知订阅者。 */
export function setState(patch) {
  state = { ...state, ...patch };
  for (const fn of listeners) fn(state);
}
"#;

const SKELETON_API: &str = r#"// 全仓唯一允许调用 wb.* 的文件。其他模块只能 import 本文件导出的函数。

/** 示例：读取业务数据（参数化查询，禁字符串拼接 SQL）。 */
export function listItems() {
  return window.wb.db.query('SELECT * FROM items ORDER BY updated_at DESC');
}

/** 示例：写入一条业务数据。 */
export function addItem(title) {
  return window.wb.db.exec('INSERT INTO items (title) VALUES (?)', [title]);
}

/** 示例：读本机文件。首次访问某目录会被静默授予并记录审计。 */
export function readLocalFile(path) {
  return window.wb.fs.read(path);
}
"#;

const SKELETON_VIEW: &str = r#"// 视图模块契约：导出 mount(root, wb) 与 unmount()，事件必须在 unmount 内解绑。
import { renderCard } from '../components/card.js';

/** 挂载概览视图。 */
export function mountOverview(root, wb) {
  const el = document.createElement('section');
  el.className = 'wb-view';
  el.innerHTML = renderCard({ title: '概览', body: '在右侧对话栏描述你想要的界面。' });
  root.appendChild(el);
}

/** 卸载概览视图。 */
export function unmountOverview() {}
"#;

const SKELETON_CARD: &str = r#"// 纯函数组件：输入 props 输出 HTML 字符串，不碰状态、不做 DOM 查询。
import { escapeHtml } from '../utils/format.js';

/** 渲染一张卡片。 */
export function renderCard({ title = '', body = '' }) {
  return `<div class="wb-card"><h3>${escapeHtml(title)}</h3><p>${escapeHtml(body)}</p></div>`;
}
"#;

const SKELETON_FORMAT: &str = r#"// 纯函数工具集。

/** HTML 转义：所有拼进模板的数据必须先过这里。 */
export function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}
"#;

pub fn read_manifest(id: &str) -> Result<WorkbenchManifest, String> {
    let path = workbench_dir(id).join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("read manifest: {}", e))?;
    serde_json::from_str(&text).map_err(|e| format!("parse manifest: {}", e))
}

pub fn write_manifest(m: &WorkbenchManifest) -> Result<(), String> {
    let dir = workbench_dir(&m.id);
    fs::create_dir_all(&dir).map_err(|e| format!("create dir: {}", e))?;
    let text = serde_json::to_string_pretty(m).map_err(|e| format!("serialize: {}", e))?;
    // 原子写：避免并发读者读到半截 manifest（与 config.yaml 同样的教训）。
    let target = dir.join("manifest.json");
    let tmp = dir.join(format!("manifest.json.{}.tmp", std::process::id()));
    fs::write(&tmp, text).map_err(|e| format!("write tmp: {}", e))?;
    fs::rename(&tmp, &target).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

pub fn list_workbenches() -> Result<Vec<WorkbenchManifest>, String> {
    // SDK / 工具箱随二进制更新：宿主升级后，已存在工作台引用的共享 vendor
    // 资产也要跟上，否则新 SDK 能力（如画布底色跟随主题）永远到不了老工作台。
    // 幂等：内容无变则跳过重写。放在 list —— 前端每次进工作台页必调，
    // 是启动后最早的 workbench 触点（create 只在建新台时才走）。
    ensure_vendor()?;
    let root = workbenches_dir();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| format!("read_dir: {}", e))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let mf = path.join("manifest.json");
        if !mf.exists() {
            continue;
        }
        match fs::read_to_string(&mf).ok().and_then(|t| serde_json::from_str::<WorkbenchManifest>(&t).ok()) {
            Some(m) => out.push(m),
            None => continue,
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

/// 设 `id` 为主工作台，清掉其余工作台的标记。
///
/// 一次扫全表而不是只改目标 —— 标记是「全局至多一个」的互斥约束，
/// 增量维护迟早会因为手工改 manifest 或删目录而留下多个 true，
/// 那时工作台页面就得靠排序猜该显示谁。全清全写把不变式钉死在这。
pub fn set_primary(id: &str) -> Result<(), String> {
    let all = list_workbenches()?;
    if !all.iter().any(|m| m.id == id) {
        return Err(format!("workbench not found: {}", id));
    }
    for mut m in all {
        let want = m.id == id;
        if m.is_primary != want {
            m.is_primary = want;
            write_manifest(&m)?;
        }
    }
    Ok(())
}

/// 软删工作台：整个目录移入 `workbenches/_trash/<id>-<时间戳>/`。
///
/// data/ 与 sessions/ 跟代码在同一目录树下，一起进回收站 —— 「回滚只动
/// code/」是版本回退的纪律，删除是另一个操作：整台撤下，可找回（用户
/// 误删还有救），不做硬删。会话归属戳（sessions.workbench_id）由调用方
/// 经 HTTP PATCH 清理 —— sessions.db 归 sidecar 独占写，这里不直接碰。
/// 被删的是主台时，主台标记随目录一起消失，`primary_workbench` 的回落
/// 逻辑会自动落到最近更新的那个，无需在这里补选。
pub fn delete_workbench(id: &str) -> Result<PathBuf, String> {
    // id 拼进路径，先挡路径穿越：合法 id 是 "workbench-<毫秒>" 纯 ASCII。
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(format!("invalid workbench id: {}", id));
    }
    let src = workbench_dir(id);
    if !src.exists() {
        return Err(format!("workbench not found: {}", id));
    }
    let trash = workbenches_dir().join("_trash");
    fs::create_dir_all(&trash).map_err(|e| format!("create trash dir: {}", e))?;
    let dest = trash.join(format!(
        "{}-{}",
        id,
        Utc::now().timestamp_millis()
    ));
    fs::rename(&src, &dest).map_err(|e| format!("move to trash: {}", e))?;
    Ok(dest)
}

/// 主工作台；没有显式标记时回落到最近更新的那个。
///
/// 回落保证「删掉主台」不会让页面变成空态 —— 用户还有别的工作台时
/// 直接看最近用的那个，比让他重新指定更少一步。
pub fn primary_workbench(list: &[WorkbenchManifest]) -> Option<&WorkbenchManifest> {
    list.iter()
        .find(|m| m.is_primary)
        .or_else(|| list.first())
}

pub fn read_code_file(id: &str, rel: &str) -> Result<String, String> {
    let path = resolve_code_path(id, rel)?;
    fs::read_to_string(&path).map_err(|e| format!("read {}: {}", rel, e))
}

/// 写入代码文件 —— 落盘前强制骨架契约：锁定文件、路径合法、单行上限。
/// 完整 lint 规则表（W001–W021）属 P1，此处只落地「让违规物理上做不到」的三条硬约束。
pub fn write_code_file(id: &str, rel: &str, content: &str) -> Result<(), String> {
    let rel_norm = rel.trim().trim_start_matches("./").replace('\\', "/");
    if LOCKED_FILES.contains(&rel_norm.as_str()) {
        return Err(format!("locked file, AI must not write: {}", rel_norm));
    }
    let path = resolve_code_path(id, &rel_norm)?;
    let limit = max_lines(&rel_norm);
    let lines = content.lines().count();
    if lines > limit {
        return Err(format!(
            "{}: {} lines exceeds limit {} — split into modules",
            rel_norm, lines, limit
        ));
    }
    // 落盘前 lint 门禁：用待写入内容预检，违规直接拒写并回传「文件:行号 + 规则ID + 修法」。
    // 只拦本次文件自身的违规 —— 历史文件或骨架的旧账不该堵住新改动，
    // 否则 agent 一次违规后这个工作台就再也写不进去了。
    let mine: Vec<Violation> = lint_workbench(id, Some((&rel_norm, content)))?
        .into_iter()
        .filter(|v| v.file == rel_norm)
        .collect();
    if !mine.is_empty() {
        return Err(format_violations(&mine));
    }
    // 写盘前留后路：代码层的改动必须可撤销，否则 agent 一次误改就把用户的
    // 工作台写死。备份落在 data/ 而不是 code/ —— 回滚代码不该牵连数据层。
    backup_before_write(id, &rel_norm, &path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create parent: {}", e))?;
    }
    fs::write(&path, content).map_err(|e| format!("write {}: {}", rel_norm, e))?;
    Ok(())
}

/// 列出代码目录下的全部文件相对路径，供模块索引注入使用。
pub fn list_code_files(id: &str) -> Result<Vec<String>, String> {
    let root = workbench_dir(id).join("code");
    let mut out = Vec::new();
    collect_rel(&root, &root, &mut out)?;
    out.sort();
    Ok(out)
}

/// 对某个工作台的全部代码跑 lint 规则表，返回违规清单（空 = 通过）。
/// `pending` 用于落盘前预检：把待写入内容按路径替换后再检查，
/// 这样 agent 一次调用即可知道"写完会不会违规"，不必先写脏文件再回滚。
pub fn lint_workbench(id: &str, pending: Option<(&str, &str)>) -> Result<Vec<Violation>, String> {
    let rels = list_code_files(id)?;
    let mut files: Vec<(String, String)> = Vec::with_capacity(rels.len() + 1);
    for rel in rels {
        let content = fs::read_to_string(workbench_dir(id).join("code").join(&rel))
            .map_err(|e| format!("read {}: {}", rel, e))?;
        files.push((rel, content));
    }
    Ok(lint_files(&files, pending))
}

/// 把违规清单渲染成回传给 agent 的文本。格式必须带文件:行号 + 规则 ID + 修法，
/// 否则 agent 只能靠猜，会陷入"整篇重写"的死循环。
pub fn format_violations(v: &[Violation]) -> String {
    if v.is_empty() {
        return "OK".to_string();
    }
    let mut out = format!("{} violation(s):\n", v.len());
    for item in v {
        out.push_str(&item.format());
        out.push('\n');
    }
    out
}

fn collect_rel(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| format!("read_dir: {}", e))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.is_dir() {
            collect_rel(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !rel.is_empty() {
                out.push(rel);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径逃逸必须被拒，否则 AI 能写到工作台之外。
    #[test]
    fn rejects_path_escape() {
        assert!(resolve_code_path("wb-1", "../outside.js").is_err());
        assert!(resolve_code_path("wb-1", "/abs/path.js").is_err());
        assert!(resolve_code_path("wb-1", "src/../../etc/passwd").is_err());
        assert!(resolve_code_path("wb-1", "").is_err());
    }

    #[test]
    fn resolves_normal_path_inside_code_dir() {
        let path = resolve_code_path("wb-1", "./src/app.js").unwrap();
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with("workbenches/wb-1/code/src/app.js"), "{}", text);
    }

    /// 锁定文件让入口与主题变量不被 AI 改写，是骨架契约的硬底线。
    #[test]
    fn entry_and_tokens_are_locked() {
        assert!(LOCKED_FILES.contains(&"src/main.js"));
        assert!(LOCKED_FILES.contains(&"styles/tokens.css"));
    }

    #[test]
    fn line_limits_by_extension() {
        assert_eq!(max_lines("a.css"), 300);
        assert_eq!(max_lines("index.html"), 150);
        assert_eq!(max_lines("views/a.js"), 200);
    }

    /// 骨架必须先于一切 AI 产出通过 lint —— 骨架自带违规的话，门禁永远红，
    /// agent 写任何文件都会被拒，整个工作台直接废掉。
    #[test]
    fn skeleton_passes_its_own_lint() {
        let files: Vec<(String, String)> = skeleton_files("测试工作台")
            .into_iter()
            .map(|(rel, body)| (rel.to_string(), body))
            .collect();
        let v = lint_files(&files, None);
        assert!(
            v.is_empty(),
            "骨架自带违规:\n{}",
            v.iter().map(|x| x.format()).collect::<Vec<_>>().join("\n")
        );
    }

    /// 模块索引渲染要够紧凑：它是每轮迭代都注入 prompt 的固定成本。
    #[test]
    fn module_index_renders_one_line_per_module() {
        let entries = vec![ModuleEntry {
            file: "src/utils/format.js".to_string(),
            exports: vec!["escapeHtml".to_string(), "formatDate".to_string()],
        }];
        assert_eq!(
            format_module_index(&entries),
            "src/utils/format.js: escapeHtml, formatDate"
        );
        assert_eq!(format_module_index(&[]), "");
    }

    /// 中文名会全部归一为分隔符，退化后必须仍有可读 id，不能变成空串。
    #[test]
    fn slugify_degrades_to_default_id() {
        let id = slugify("安全管理工作台");
        assert!(id.starts_with("workbench"), "{}", id);
        let id2 = slugify("Ledger 台账");
        assert!(id2.starts_with("ledger-"), "{}", id2);
    }

    /// 无用户依赖时 importmap 仍须给出宿主工具箱 —— 它不占用户依赖配额。
    #[test]
    fn empty_deps_still_map_host_tools() {
        let block = render_importmap(&[]);
        assert!(block.contains(MAP_START) && block.contains(MAP_END));
        assert!(block.contains("\"wb-tools\""), "{}", block);
        assert!(block.contains("\"wb-sdk\""), "{}", block);
    }

    /// 工具箱有三条投递路径，任何一条断了 AI 都用不上它（白做）：
    /// ① importmap 的 `wb-tools` 映射 ② SDK 内联的 `wb.util` ③ 独立 ESM 文件。
    #[test]
    fn toolbox_reaches_workbench_through_all_three_paths() {
        // ① importmap 指向 wb-tools.js
        let block = render_importmap(&[]);
        assert!(
            block.contains("../../_vendor/wb-tools.js"),
            "importmap 未指向工具箱文件：{}",
            block
        );

        // ② SDK 里 wb.util 必须挂上真实现，不能是空对象兜底
        assert!(
            WB_SDK_JS.contains("util: window.__wbTools || {}"),
            "SDK 缺少 wb.util 挂载点"
        );
        let lit = tools_object_literal(crate::WB_TOOLS_TS);
        for name in ["virtualWindow", "diffLines", "parseDelimited", "columnStats"] {
            assert!(lit.contains(&format!("{name}: {name}")), "SDK 内联缺 {name}: {}", lit);
        }

        // ③ 独立 ESM 的导出名必须与 importmap 约定一致（AI 按 SKILL.md 的名字 import）
        let esm = strip_typescript(crate::WB_TOOLS_TS);
        for name in ["virtualWindow", "diffLines", "parseDelimited", "columnStats"] {
            assert!(
                esm.contains(&format!("export function {name}(")),
                "wb-tools.js 未导出 {name}"
            );
        }
    }

    /// 工具箱以 TS 维护、以 JS 注入，剥类型必须真的剥干净：
    /// 漏一个冒号就是一个 SyntaxError，整个 SDK 挂掉、工作台全白屏。
    #[test]
    fn toolbox_strips_types_into_valid_js() {
        let js = strip_typescript(crate::WB_TOOLS_TS);
        // 落一份给 esbuild 真解析（.tmp-probe/syntax-check.mjs）——
        // 字符串断言只能证明「我以为没漏」，解析器才能证明浏览器不会炸。
        if std::env::var_os("WB_TOOLS_DUMP").is_some() {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf());
            if let Some(root) = root {
                let dir = root.join(".tmp-probe");
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join("stripped.js"), &js);
            }
        }
        assert!(!js.contains("interface "), "{}", js);
        assert!(!js.contains(" as "), "{}", js);
        // `wb-tools.js` 供 `import { x } from 'wb-tools'`，导出必须保留
        for name in ["virtualWindow", "diffLines", "parseDelimited", "columnStats"] {
            assert!(js.contains(&format!("export function {name}(")), "缺 export {name}");
        }
        // 函数体必须完整保留
        for name in ["virtualWindow", "diffLines", "parseDelimited", "columnStats"] {
            assert!(js.contains(&format!("function {name}(")), "缺 {name}\nJS tail: {}",
                &js[js.len().saturating_sub(1200)..]);
        }
        // 残留的类型注解特征：参数位置出现 `x: Type` 形态
        let bad = regex_lite::Regex::new(r"\([\w, ]*[A-Za-z_]\w*\s*:\s*[A-Z]").unwrap();
        assert!(!bad.is_match(&js), "残留参数注解：{}", js);
        // 局部变量注解也不能漏
        let bad_var = regex_lite::Regex::new(r"\b(?:const|let|var)\s+\w+\s*:").unwrap();
        assert!(!bad_var.is_match(&js), "残留局部变量注解：{}", js);

        // 对象字面量路径反过来不能有 export
        let lit = tools_object_literal(crate::WB_TOOLS_TS);
        assert!(!lit.contains("export "), "LIT 残留 export: {}", lit);
        for name in ["virtualWindow", "diffLines", "parseDelimited", "columnStats"] {
            assert!(lit.contains(&format!("{name}: {name}")), "LIT: {}", lit);
        }
    }

    /// SDK 内联必须是「先定义函数再返回字面量」的 IIFE。
    /// 回归钉子：曾生成过只有 `{ name: name }` 引用、没有函数定义的 SDK，
    /// 测试全绿（断言只看字面量内容），运行时 ReferenceError、工作台全白屏。
    #[test]
    fn sdk_inline_defines_functions_before_referencing() {
        let sdk = WB_SDK_JS.replace(
            "window.__wbTools || {}",
            &tools_inline_iife(crate::WB_TOOLS_TS),
        );
        // 每个函数的定义必须出现在 SDK 文件里（且在挂载点之前）
        for name in ["virtualWindow", "diffLines", "parseDelimited", "columnStats"] {
            let defined = sdk.find(&format!("function {name}("));
            let mounted = sdk.find(&format!("{name}: {name}"));
            assert!(defined.is_some(), "SDK 内联缺 {name} 定义");
            assert!(
                defined.zip(mounted).map_or(false, |(d, m)| d < m),
                "{name} 的定义必须先于引用"
            );
        }
        // IIFE 立即执行后返回真对象，不是裸引用
        assert!(sdk.contains("return {"), "{}", &sdk[..sdk.len().min(2000)]);
    }

    /// 剥类型的每一处误伤都对应一个已踩过的坑，逐个钉住。
    /// 这些形态一旦回归，症状是「测试绿、运行时白屏」—— 最难查的一类。
    #[test]
    fn strip_types_handles_every_known_shape() {
        // 1. 多行参数列表：每个参数以 `,\n` 结尾，收尾判定必须认换行
        let js = strip_typescript("function f(\n  a: number,\n  b: string,\n) {\n}");
        assert!(js.contains("a,") && js.contains("b,"), "{}", js);
        assert!(!js.contains(": number") && !js.contains(": string"), "{}", js);

        // 2. 局部变量注解结尾是 `=` 而非 `,`/`)`
        let js = strip_typescript("function f() {\n  const out: DiffHunk[] = [];\n}");
        assert!(js.contains("const out = []"), "{}", js);

        // 3. 内置原始类型泛型实参：`<number>` 小写开头，不能按大小写过滤
        let js = strip_typescript("const t = new Array<number>(n);");
        assert!(js.contains("new Array(n)"), "{}", js);

        // 4. 对象字面量简写不能被当成参数注解剥掉 —— 这是最凶的一处，
        //    剥完 `{ left: a[i], right: b[i] }` 会变成 `{ left, right }`，
        //    语法合法但运行时 `left is not defined`，esbuild 根本查不出来。
        let js = strip_typescript("out.push({ left: a[i], right: b[i] });");
        assert!(js.contains("left: a[i]"), "对象字面量被误剥：{}", js);

        // 5. 字符串里的冒号不能被碰
        let js = strip_typescript("if (ch === \":\") { x(); }");
        assert!(js.contains("\":\""), "{}", js);

        // 6. interface 块紧邻 JSDoc 时，注释块与声明块不能互相吞掉后续代码
        let js = strip_typescript("interface A {\n  x: number;\n}\n\n/** 说明 */\nfunction g() {}\n");
        assert!(js.contains("function g()"), "声明块吞掉了后续代码：{}", js);

        // 7. export 必须保留（wb-tools.js 供 import 用）
        let js = strip_typescript("export function h() {}");
        assert!(js.contains("export function h()"), "{}", js);
    }

    /// 标记段重写必须保留 AI 写的其余内容，且标记缺失时报错而非静默跳过。
    #[test]
    fn importmap_block_replaces_only_between_markers() {
        let html = format!(
            "<head>\n{}\n<!-- 旧 -->\n{}\n</head>\n<body>AI 的东西</body>",
            MAP_START, MAP_END
        );
        let out = replace_between(&html, render_importmap(&[])).unwrap();
        assert!(!out.contains("旧"));
        assert!(out.contains("AI 的东西"), "{}", out);
        assert!(out.contains("<head>") && out.contains("</head>"));

        assert!(replace_between("<head></head>", render_importmap(&[])).is_err());
    }

    /// 主台标记是「全局至多一个」的互斥约束 —— 增量维护迟早会留下多个
    /// true，那时工作台页面只能靠排序猜该显示谁。set_primary 必须一次
    /// 扫全表把不变式钉死。
    /// `HERMIX_HOME` is process-global and `cargo test` runs tests in this
    /// binary on parallel threads. Any other test that sets it (or a
    /// user-level `HERMIX_HOME` inherited from the developer's shell)
    /// can change the value *between* this test's `set_var` and its
    /// `create_workbench`, so the manifests land in one directory and
    /// `list_workbenches` scans another — hence the intermittent
    /// `workbench not found: workbench-<id>`.
    ///
    /// A unique directory name only fixes directory-name collisions; it
    /// does nothing about the shared env var. Serialize against a
    /// process-wide lock for the whole test, and restore the previous
    /// value on the way out so we do not leak into later tests.
    ///
    /// 2026-10-01：锁原先是本函数内的 `static`，与 `validate.rs` 里的
    /// 另一把**不是同一把** —— 两者互不排斥，于是 validate 的 esbuild
    /// 测试会偶发拿不到本测试设的 home。现已提到 crate 级
    /// `crate::hermix_home_guard()`。
    #[test]
    fn set_primary_clears_previous_marker() {
        let _env_guard = crate::hermix_home_guard();
        let tmp = std::env::temp_dir().join(format!(
            "hx-wb-primary-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::env::set_var("HERMIX_HOME", &tmp);

        let a = create_workbench("甲工作台").unwrap();
        assert!(a.is_primary, "第一个工作台应自动成为主台");
        let b = create_workbench("乙工作台").unwrap();
        assert!(!b.is_primary, "后续工作台不应自动抢主台");

        set_primary(&b.id).unwrap();
        let list = list_workbenches().unwrap();
        assert_eq!(
            list.iter().filter(|m| m.is_primary).count(),
            1,
            "主台必须至多一个"
        );
        assert_eq!(primary_workbench(&list).unwrap().id, b.id);

        // 没有显式标记时回落到最近的，而不是返回 None —— 否则删掉主台
        // 会让页面变空态，而用户明明还有工作台可用。
        let stripped: Vec<WorkbenchManifest> = list
            .into_iter()
            .map(|m| WorkbenchManifest { is_primary: false, ..m })
            .collect();
        assert!(primary_workbench(&stripped).is_some());
        assert!(primary_workbench(&[]).is_none());

        assert!(set_primary("不存在的工作台").is_err());
        // 环境变量由 `_env_guard` 的 Drop 还原（crate::hermix_home_guard）。
        // 刻意不删目录：%TEMP% 由 OS 回收，运行中删除会与还在往里写的
        // 其它测试相撞。
    }
}
