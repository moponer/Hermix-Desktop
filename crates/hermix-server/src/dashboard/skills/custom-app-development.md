---
name: custom-app-development
version: 16
description: >
  自定义应用开发助手。当用户要求创建/开发一个桌面应用、桌面工具、独立程序、窗口应用、软件、小软件，
  或"打包成 exe"时使用；也用于**用户报告已安装自定义应用报错/打不开/崩溃**的修复场景。
  通过 custom_app_manager 工具创建、更新、打包、运行(dev)、校验、冒烟、文档管理(read_doc/update_doc/log_iteration/search_iterations)用户自定义桌面应用。
  基于 hx-shell 运行时（wry/WebView2，单文件 exe），LLM 写前端（TS/React/纯 HTML），Hermix 打包成单文件 exe 分发。
  用户电脑零工具链。每个应用在 docs/ 下强制 4 个 md 作为源码的一部分。
tags:
  - custom-app
  - desktop
  - application
  - hx-shell
  - wry
  - webview2
  - standalone
  - exe
  - vue
  - react
  - typescript
  - javascript
  - 自定义应用
  - 桌面应用
  - 软件开发
  - docs-management
---

# Custom App Development（基于 hx-shell）

## When to Activate

- 用户说"做个/开发一个/写个 应用/工具/软件/小软件/桌面程序/独立程序"，或"打包成 exe"
- 用户描述一个需要独立窗口运行的功能需求
- **用户报告已安装应用出错**：打不开、加载失败 (os error 3)、崩溃、运行时报错
- 收到 `@app 创建新应用 ...` 形态文本（快捷入口）
- 一律判定为自定义应用走本 skill；**不要**调其他通用代码工具，也**不要**自己 `dir`/`ls` 仓库或猜 HERMIX_HOME 路径

## Core Concepts

- **运行时 = hx-shell**：`wry 0.55.1` + WebView2，单文件 exe ≈1MB，用户电脑零工具链（无 Node/Rust）。打包 = 把 HTML/JS 作为 PE 资源节(RT_RCDATA)嵌入 hx-shell.exe，WebView2 即跑，**无尾部注入、无结构型 Defender 误报**。
- **应用模型** `HERMIX_HOME/custom-apps/<id>/`：
  ```
  <id>/
  ├── manifest.json       # 应用清单
  ├── index.html          # 纯 HTML 模式入口
  │   (或)
  ├── src/main.tsx        # TS/React 模式入口
  │   src/components/...  # 额外组件
  ├── docs/               # ★ 4 个 md 强制 (源码一部分, 与代码同源)
  │   ├── 设计方案.md       # propose 输出
  │   ├── 开发计划进度.md   # agent 维护 checkbox
  │   ├── 更新迭代.md       # append-only, 每条【问题/根因/修复/验证】4 节
  │   └── README.md         # 用户视角
  └── dist/                # 构建产物, 禁止手改
      ├── <id>.dev.html    # run 测试版
      └── <id>.exe          # package 单文件
  ```
- **manifest 权限**：必须含 `permissions` 数组，validate L2 做"权限一致性"卡——HTML/TSX 用到的每个 `hx.*` API 都要有对应权限，否则 `ok=false`。有效值：`fs-read`/`fs-write`/`exec`/`dialog`/`app`/`clipboard`/`net`。
- **两种模式**：纯 HTML（`index_html`）/ Vue·React·TS（`app_source`+`index_html`）。
- **绑定 API**：`window.hx.*` 经 WebView2 原生桥 `postMessage`→Rust `ipc_handler`，**无 CORS 限制**。所有方法返回 `{ok,data?,err?}`。
- **三道强制闸门**：①设计阶段（propose→approve→create 硬约束）②复用检索（先搜 OSS）③代码审查与验证（validate→smoke + 对照 code-review.md）。

### ★ 设计闸门 = 工具级硬约束

新建应用必须先 `custom_app_manager` **`propose`** 输出结构化设计方案 → 展示给用户 → 用户确认后 **`approve`** → 才能 **`create`**。**没有 `approve` 过的方案，`create` 直接拒绝**（返回"缺少已批准的设计方案"）。已安装应用的 `update` 迭代不受此闸门限制。

### ★ 文档管理 = 源码的一部分（强制）

每个应用必须自带 4 个 md 作为源码的一部分，落盘到 `<custom-apps>/<id>/docs/`：

| 文件 | 角色 | 谁写 | 写入方式 |
|---|---|---|---|
| `设计方案.md` | propose 输出的结构化方案 + 用户确认状态 | agent | propose / approve / create 时自动渲染 |
| `开发计划进度.md` | 阶段任务 checkbox + 下一步行动 | agent | update 时 `plan_note` 自动 append, 或 update_doc |
| `更新迭代.md` | append-only 历史, 每条【问题/根因/修复/验证】4 节 | agent | log_iteration / update 自动 append |
| `README.md` | 用户视角快速上手 | agent | update_doc |

**强制流程**：
1. **新建**：`list` 查重 → `propose` → 用户确认 → `approve` → `create` (自动 init 4 个 md)
2. **迭代/修复**：
   - ① **`search_iterations`** query=<关键词> 查 历史 bug, **避免重复触发**
   - ② **`read_doc`** 设计方案.md + 开发计划进度.md 了解规划
   - ③ `update` 或 `patch` 改代码 (**改代码类 update 必须带 `iteration_note`** {问题/根因/修复/验证}, 工具自动 append 到 更新迭代.md)
   - ④ `validate` 复验 (含 L4 文档一致性: 4 个 md 齐全 + 更新迭代每条 4 节)
   - ⑤ `run` 测试版预览
   - ⑥ `update_doc` 把 plan_note/新状态 同步到 开发计划进度.md
3. **打包**：`package` (sidecar)
4. **交付**：`update_doc` 补全 README.md 给用户看

**注意**：
- `设计方案.md` 不允许直接 `update_doc`（强制走 propose/approve 流程）
- 改代码类 `update` 必须带 `iteration_note`（**工具拒绝**缺它的 update）
- 修 bug 前必须 `search_iterations`（避免重复触发老 fix）

### 设计提案模板（`propose` 的 `proposal` 必含以下章节，缺一不可）

- **① 名称**：app 名称、id（字母数字-_）、category、**icon（必填** —— 一个 lucide 图标名，如 `music`/`camera`/`calculator`/`notebook-pen`/`folder-tree`；缺省会让卡片左上角显示为无标识灰块，validate L0 直接判 error）
- **② 功能**：功能范围、核心用例、数据存哪（hx 写文件的路径/目录约定）、是否需要联网
- **③ 界面布局**：明暗两套配色 token（浅色优先、中性灰、禁蓝紫）、整体布局基线、V7 无边框壳必须自实现标题栏（拖动 + 最小化/最大化/关闭）
- **④ 交互效果**：主要交互流、涉及哪些 `hx.*` API（getInfo/readFile/writeFile/exec/openDialog/win.* 等）、关键反馈/状态文案
- **⑤ 自定义应用专属（必须拍板）**：
  - 运行模式：纯 HTML（`index_html`）还是 TS/React（`app_source`）
  - `manifest.permissions`：声明用到的权限（fs-read/fs-write/exec/dialog/app/clipboard/net），须与所用 hx API 一致（validate L2 校验）
  - 窗口配置：`window_config` 的 width/height/resizable/title
  - 图标三选一，validate L0 强制校验：
    - `icon`（必填）：lucide 图标名，见 `apps/desktop-tauri/src-tauri/resources/icons/` 清单（文件名去 `.svg`）。**不接受 data URI** —— 会被引擎忽略并回退到类目兜底
    - `icon_file`（可选）：放 app 目录下的自定义 PNG 文件名。声明了但文件不存在 → error（卡片 `<img>` 404，左上角直接空白）
    - `tile_color`（可选）：`#RRGGBB`/`#RRGGBBAA` 图标容器底色，格式非法只给 warning
  - 主题约束：浅色优先、禁高饱和蓝紫、禁 emoji（界面/按钮/manifest icon）
  - 禁止项自查：禁 `<input type=file>`（用 openDialog/saveDialog）、禁 CDN 外链、禁 Tauri/Node API、禁 `fetch('app://')`
  - 复用来源：是否复用 OSS（项目/版本/License）

## Practical Guidance

### agent 开发流（一次性到位交付）

1. 识别为自定义应用 → 想清数据存哪/用哪些 `hx.*` API（先 `getInfo()` 取可用清单）/功能气质定配色
2. **`list`** 查重：已有同类应用就 `update` 迭代，否则走下面新建流
3. **设计闸门（工具强制）**：调 `custom_app_manager` **`propose`** 输出 ① 名称 ② 功能 ③ 界面布局 ④ 交互效果 ⑤ 自定义应用专属 → 把方案展示给用户 → 等用户确认 → 调 **`approve`** 标记 approved → 之后 **`create`** 才放行（缺 approved 方案 create 会拒）。create 后 docs/ 4 个 md 自动生成。
4. **复用检索闸门**：能力点先搜 GitHub（MIT/Apache/BSD），不可达切 Gitee/GitCode；记录 项目/版本/License/嵌入方式；无合适才自研
5. **写码**（`index_html` 或 `app_source` TSX），严格按设计 token + 前端默认标准（浅色/中性灰/禁蓝紫/禁 emoji/禁 CDN/必带标题栏/必含主题切换）
6. `validate` 循环到 `ok=true`（L0 manifest/L1 HTML/L2 权限/L3 前端质量/L4 文档一致性），不得带 error 交付
7. **`run`** 测试版即时预览（`hx-shell --app-dir <app目录> --dev --app-id <id>`，走 `app://` 协议直接加载磁盘最新源码，不重新打 exe）；要确认无报错调 `smoke`
   - 测试版窗口**按 F12 打开 WebView2 开发者工具**（Console/Elements/Network 齐全），查运行时报错、DOM、请求就靠它；打包后的正式 exe 不响应 F12
   - 页面自身的 JS 错误还会自动落盘到应用目录的 `.runtime-errors.log`
8. **更新文档**：每次改动后 → `update_doc` 把状态写进 开发计划进度.md；bug 修复后 → `log_iteration` 把【问题/根因/修复/验证】追加到 更新迭代.md；交付前 → `update_doc` README.md 给最终用户看
9. **代码审查闸门**：对照 code-review.md + frontend-architecture.md，跑 smoke 确认无运行时/控制台报错，提示友好
10. 交付说明：去哪运行（run 测试版 / package 出 exe）、如何打包、如何回来 `update` 继续

### 修复/迭代流程（强制顺序）

1. **`action=search_iterations` query=<关键词>** 查 更新迭代.md 历史 → 命中则看清楚上次怎么修的, 避免重复触发
2. **`action=read_doc` doc=设计方案 / doc=开发计划进度** 了解当前规划
3. **`action=update`** 改代码, 必须带 `iteration_note={问题, 根因, 修复, 验证}` → 工具自动 append 到 更新迭代.md
   - 或 **`action=patch`** 做手术式小修 → 改完再调 log_iteration 补记录
4. **`action=update_doc` doc=开发计划进度** 用 checkbox 推进度; 或 `update` 时带 `plan_note=...` 让工具自动 append
5. **`action=validate`** 复验 (含 L4 文档一致性)
6. **`action=run`** 测试版预览

### 关键 `hx.*` API（常用）

| API | 权限 | 说明 |
|---|---|---|
| `getInfo()` | app | 取运行时可用 API 清单（强制第一步） |
| `readFile`/`writeFile`/`appendFile`/`removeFile` | fs-read/fs-write | 文本读写 |
| `readDirDetailed`/`stat`/`createDir` | fs-read | 目录/元信息 |
| `readFileBytes`/`writeFileBytes` | fs-write | 二进制 base64 |
| `fileUrl(path)` | — | 本地路径→`hxfile://` URL，直接喂 `<audio>/<img>/<video>` |
| `exec(cmd,args)` | exec | 外部命令，返回 {stdout,stderr,exitCode} |
| `openDialog`/`saveDialog` | dialog | 原生文件/目录选择（**禁 `<input type=file>`**） |
| `showMessage` | dialog | 原生消息框 |
| `win.minimize/maximize/close/startDrag/setTitle` | app | 无装饰窗口控制（V7 壳必带标题栏） |

```html
<!-- 纯 HTML 工具骨架（浅色 + 自定义标题栏 + SVG 图标） -->
<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8"><title>工具</title><style>
 *{box-sizing:border-box;margin:0;padding:0}
 body{font:14px/1.6 system-ui,"PingFang SC","Microsoft YaHei",sans-serif;background:#fafafa;color:#18181b;display:flex;flex-direction:column;height:100vh}
 .hx-titlebar{display:flex;align-items:center;justify-content:space-between;height:32px;padding:0 0 0 12px;background:#f0f0f0;border-bottom:1px solid #e4e4e7;user-select:none}
 .hx-titlebar__title{font-size:12px;color:#71717a}
 .hx-titlebar__controls{display:flex;height:100%}
 .hx-titlebar__btn{width:46px;height:100%;border:none;background:transparent;color:#52525b;cursor:pointer;display:flex;align-items:center;justify-content:center}
 .hx-titlebar__btn:hover{background:#e4e4e7}
 .hx-titlebar__btn--close:hover{background:#c62828;color:#fff}
 .content{padding:24px;flex:1;overflow-y:auto}.wrap{max-width:880px;margin:0 auto}
 .card{background:#fff;border:1px solid #e4e4e7;border-radius:10px;padding:16px;margin-bottom:16px}
 button{display:inline-flex;align-items:center;gap:6px;height:36px;padding:0 14px;background:#f4f4f5;color:#18181b;border:1px solid #e4e4e7;border-radius:8px;cursor:pointer;font-size:13px}
 button.primary{background:#4b7a5e;color:#fff;border:none}
 .out{margin-top:12px;background:#f4f4f5;border-radius:8px;padding:10px;font:12px/1.5 Consolas,monospace;white-space:pre-wrap}
</style></head><body>
<div class="hx-titlebar" onmousedown="window.hx.win.startDrag()">
 <span class="hx-titlebar__title">工具</span>
 <div class="hx-titlebar__controls" onmousedown="event.stopPropagation()">
  <button class="hx-titlebar__btn" onclick="window.hx.win.minimize()" title="最小化">▁</button>
  <button class="hx-titlebar__btn" onclick="window.hx.win.toggleMaximize()" title="最大化">▢</button>
  <button class="hx-titlebar__btn hx-titlebar__btn--close" onclick="window.hx.win.close()" title="关闭">✕</button>
 </div>
</div>
<div class="content"><div class="wrap"><div class="card">
 <textarea id="in" style="width:100%;height:140px;background:#f4f4f5;border:1px solid #e4e4e7;border-radius:8px;padding:10px"></textarea>
 <div style="margin-top:10px"><button class="primary" id="save">保存文件</button></div>
 <div class="out" id="out">—</div>
</div></div></div>
<script>
const $=id=>document.getElementById(id);const out=(s,e)=>{$('out').textContent=s;$('out').style.color=e?'#c62828':'#2e7d32'}
$('save').onclick=async()=>{const r=await window.hx.writeFile('output.txt',$('in').value);out(r.ok?'已保存':'失败: '+r.err,!r.ok)}
</script></body></html>
```
> 音乐/本地文件类用 `readDirDetailed`+`fileUrl`+`<audio>`；Vue/React 用 Vite 单文件内联（Vue 运行时内嵌，禁 unpkg CDN）。核心即上述骨架 + `hx.*` 调用，按需扩展。

## Gotchas

- **【致命】禁止 `<input type=file>`/`webkitdirectory`/手输路径选文件**：触发 WebView2"上传"确认弹窗。永远用 `openDialog`/`saveDialog`。
- **【致命】禁止 `fetch('app://...')`**：CORS 拒绝。一律 `window.hx.*`。
- **【致命】不要用 Tauri API**（`convertFileSrc`/`shell_*`）**或 Node API**（`path`/`fs`/`child_process`）：hx-shell 里不存在。
- **【致命】禁止 CDN 外链**（unpkg/jsdelivr）：Vue/React/图标全部内联或 Vite 打包内联，离线可用。
- **【致命】禁止 emoji**（界面/按钮/状态/manifest icon）：图标用内联 SVG（lucide 风格 24×24 `stroke=currentColor`）。
- **【致命】查/改应用只走 `custom_app_manager`**：绝不自己拼 `C:/Users/*/hermix/custom-apps` 等路径。
- **【致命】create 前必须 `list` 查重**：同类应用已存在就 `update`，绝不再建第二个。
- **【致命】V7 壳无原生标题栏**：`decorations:false`，HTML 顶部必须自实现标题栏，否则用户无法移动/关闭。
- **【致命】改代码类 update 必须带 `iteration_note`**：工具拒绝缺它的 update。
- **【致命】修 bug 前必须 `search_iterations`**：避免重复触发历史 fix。
- **【致命】设计方案.md 不允许 update_doc**：改方案必须重新 propose + approve。
- **禁止手改 `dist/`**：`dist/<id>.exe` 与 `<id>.dev.html` 每次 `run`/`package` 从源码重建覆盖。
- **默认浅色**：不要上来写深色；墨绿 `#4b7a5e` 仅作未做功能化配色时的回退，禁止所有应用统一用它。
- **二进制用 `fileUrl`**：`readFile` 只读文本，MP3/图片/视频用 `hx.fileUrl(path)` 生成 URL。

## Integration

- **唯一入口工具 `custom_app_manager`**：所有查/改/打包/运行/文档管理都走它。落盘到 `hermix_home()/custom-apps/<id>/`。
- **完整 action 列表**：`propose`(出方案) / `approve`(确认) / `create`(落地) / `update`(改代码, 强制带 iteration_note) / `list` / `get` / `delete` / `package` / `run`(测试版) / `validate`(含 L4 文档一致性) / `smoke`(运行时) / `patch`(手术式替换) / `read_doc` / `update_doc`(方案除外) / `log_iteration` / `search_iterations`。
- **`@app` 快捷入口**：聊天 `@app 创建新应用` → 输入框预填 `@app 创建新应用 应用要求：<需求>`。
- **三闸门协同**：设计阶段 → 复用检索 → 代码审查；本文档已升级为文档管理强制机制（4 个 md 与代码同源）。
- **与对话流关系**：本 skill 在用户提"应用/工具/软件"诉求或报告已装应用报错时自动加载；运行时若 `getInfo()` 返回 API 与本文不符，以 `getInfo()` 为准（本文档可能滞后）。

## References

- 同目录 `code-review.md`：交付前审查清单（无低级错误/死代码/权限一致/离线可用/设计落实）
- 同目录 `frontend-architecture.md`：前端质量基线（布局层级/控件尺寸/响应式）
- 同目录 `code-craftsmanship.md`：通用代码精炼标准（YAGNI/标准库/原生平台/现有依赖/单行/最小实现 + 不重复造轮子），适用于 hermix 与 workbuddy
- 图标库：内置 2007 个 Lucide SVG，清单见 `apps/desktop-tauri/src-tauri/resources/icons/`（文件名去 `.svg` 即 `manifest.icon` 名，如 `music`/`settings`/`folder`）
- **`tile_color`（可选）**：`#RRGGBB` 或 `#RRGGBBAA`，给列表卡片/详情头部/窗口标题栏/打包 exe 任务栏图标加一个**纯色圆角背景块 + 白色字形**（无渐变）。规则：`icon_file`/`icon_data` 或 `icon` 为 emoji 时**跳过**背景；非法值被 `validate_tile_color` 拒绝。
- hx-shell API 真相源：`window.hx.getInfo()`（运行时返回完整可用 API 清单，本文档可能滞后）