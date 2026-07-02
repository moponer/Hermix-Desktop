# Hermix Desktop 可移植方案:让 Agent 数据跟着安装包走

> **最终版时间**: 2026-06-08
> **状态**: ✅ 定稿,等主人开 Phase 10
> **核心定位**: 一个产品、一个发行物、一个目标用户

---

## 一、定位(锁死)

### 1.1 旧定位(取消)

```
❌ "通用 AI Agent,多平台,17 个 gateway"
❌ "Tauri 桌面应用 / CLI TUI / Dashboard HTTP API 三层"
❌ "macOS / Linux / Windows 三端支持"
```

### 1.2 新定位(锁定)

```
✅ 一个产品:Hermix Desktop(Windows 桌面 AI 助手)
✅ 一个发行物:zip 解压即用,不安装,不写注册表,不写 C 盘
✅ 一个目标用户:不想折腾环境、想 AI 直接能用的普通办公人
✅ 一个核心场景:把整个安装目录拷到 U 盘 / 另一台电脑,启动后所有数据完整保留
```

**这条产品哲学比任何技术细节都重要**。一旦锁死,所有"为兼容性而做"的妥协都不需要了。

---

## 二、核心需求

### 2.1 用户层(R1-R7)

| 编号 | 需求 | 验收 |
|------|------|------|
| **R1** | **零安装零环境** | 拿到 `Hermix-Desktop-Windows.zip` → 解压 → 双击 `Hermix.exe` → 立即可用 |
| **R2** | **数据可移植** | 整个 `Hermix Desktop/` 拷到 U 盘/另一台电脑,启动后所有数据 100% 保留 |
| **R3** | **C 盘零写入** | 运行期间**不**在 C 盘写任何数据(`%APPDATA%`/`%LOCALAPPDATA%`/`%USERPROFILE%`/`%TEMP%` 都不允许) |
| **R4** | **C 盘空间不涨** | 用半年/一年,`%USERPROFILE%` 下"程序产生数据" 0 字节 |
| **R5** | **单文件/单目录** | 一个 `Hermix Desktop/` 目录,子目录分明 |
| **R6** | **重装系统后还能用** | 删 C 盘重装 Windows → 重新解压 → 启动 → 数据回来 |
| **R7** | **卸载即删除** | 关程序 → 删 `Hermix Desktop/` 目录 → 干干净净,无残留 |

### 2.2 工程层(E1-E8)

| 编号 | 需求 | 验收 |
|------|------|------|
| **E1** | **HERMIX_HOME 强制重定向** | 默认值不再是 `~/.hermix`,而是 `<install_root>/data/` |
| **E2** | **沙箱路径全替换** | Tauri 启动 hook 把 `install_root/data` 写进 `HERMIX_HOME`,**先于**任何 crate 初始化 |
| **E3** | **取消 ~$HOME 兜底** | 桌面端启动时检测到 `~/.hermix` 存在 → 弹一次性"导入?"对话框,**绝不自动**复制 |
| **E4** | **sidecar 路径** | `hermix-server.exe` 与 `Hermix.exe` 在同一目录,Tauri 启动时 `current_exe().parent()` 直接拿 |
| **E5** | **webview2 用户数据** | 微软 WebView2 默认写 `%LOCALAPPDATA%`,**必须**改写到 `<install_root>/webview-cache/` |
| **E6** | **TEMP 临时文件** | Override 到 `<install_root>/cache/tmp/`(可选,P2 优先级) |
| **E7** | **环境变量沙箱** | sidecar 继承的 `HERMIX_HOME` / `TMP` / `TEMP` / `WEBVIEW2_USER_DATA_FOLDER` 全部强制重写 |
| **E8** | **hermix.exe 单一二进制** | 当前 ~16MB OK;但要确保 sidecar 不再依赖任何 C:\ 文件 |

---

## 三、关键工程改造清单(P0)

### 3.1 `hermix-config::paths::hermix_home()` 改造

**路径**:`crates/hermix-config/src/paths.rs:122`

**改造前**:
```rust
pub fn hermix_home() -> PathBuf {
    let resolved = if let Ok(home) = std::env::var("HERMIX_HOME") {
        if home.is_empty() {
            user_home_dir().join(".hermix")
        } else {
            PathBuf::from(home)
        }
    } else {
        user_home_dir().join(".hermix")
    };
    ...
}
```

**改造后**:
```rust
pub fn hermix_home() -> PathBuf {
    if let Ok(home) = std::env::var("HERMIX_HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);  // 1. 显式 HERMIX_HOME 永远胜出
        }
    }
    if let Ok(p) = std::env::var("HERMIX_PORTABLE_DATA_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);  // 2. 桌面端:Tauri 主进程在 fork 前 set
        }
    }
    user_home_dir().join(".hermix")  // 3. CLI 模式兜底
}
```

**改动量**: ~5 行 + 1 个测试。

### 3.2 Tauri 启动 hook 注入环境变量

**路径**:`apps/desktop-tauri/src-tauri/src/sidecar.rs:94`

**改造**:
```rust
let install_root = std::env::current_exe()
    .ok()
    .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    .expect("无法定位安装根目录");

let data_dir = install_root.join("data");
let cache_dir = install_root.join("cache");
let log_dir = install_root.join("logs");
let webview_dir = install_root.join("webview-cache");
for d in [&data_dir, &cache_dir, &log_dir, &webview_dir] {
    std::fs::create_dir_all(d).ok();
}

let child = Command::new(&bin)
    .args(["serve", "--port", &self.port.to_string()])
    .env("HERMIX_PORTABLE_DATA_DIR", &data_dir)
    .env("HERMIX_PORTABLE_CACHE_DIR", &cache_dir)
    .env("HERMIX_PORTABLE_LOG_DIR", &log_dir)
    .env("HERMIX_HOME", &data_dir)
    .env("WEBVIEW2_USER_DATA_FOLDER", &webview_dir)
    .env("TMP", &cache_dir.join("tmp"))
    .env("TEMP", &cache_dir.join("tmp"))
    .env("RUST_LOG", "info")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| format!("Failed to spawn hermix: {e}"))?;
```

**关键**:
- `WEBVIEW2_USER_DATA_FOLDER` 是微软 WebView2 官方的环境变量,可彻底重定向 webview2 用户数据
- `TMP` / `TEMP` 用 `cache/tmp/`,临时文件不需要持久化
- 子目录用 `create_dir_all`,失败用 `ok()` 不阻塞启动

### 3.3 Tauri conf.json: 关闭 installer

**路径**:`apps/desktop-tauri/src-tauri/tauri.conf.json`

```json
{
  "bundle": {
    "active": true,
    "targets": ["nsis"],
    "windows": {
      "nsis": {
        "installMode": "currentUser",
        "installDir": "Hermix Desktop"
      }
    }
  }
}
```

**最终选择**:
- `bundle.targets` 改为 `[]`(不出 installer,只出 raw exe + 资源目录)
- 编译产出: `<install>/Hermix.exe` + 资源目录
- 验证:解压 zip → 双击 `Hermix.exe` → 启动

### 3.4 WebView2 用户数据目录(微软的坑)

**问题**: 微软 WebView2 默认把缓存、cookie、本地存储等写到 `%LOCALAPPDATA%\<bundle_id>.Embedded\`。

**解决(双保险)**:
1. Tauri 启动时设 `WEBVIEW2_USER_DATA_FOLDER` 环境变量([Microsoft 官方文档](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/user-data-folder))
2. 或者用 `WebviewWindowBuilder.data_directory()`(Tauri v2 API):
   ```rust
   // Tauri v2: 在 lib.rs 的 setup hook 里
   let webview_dir = install_root.join("webview-cache");
   std::fs::create_dir_all(&webview_dir).ok();
   // WebviewWindowBuilder::new(...).data_directory(&webview_dir)  // 查 Tauri v2 当前 API
   ```

**WebView2 缺失**:
- Windows 10 1809+ 默认自带 WebView2 runtime,覆盖率 ~95%
- 启动时检测,缺失则弹窗"请安装 WebView2,Microsoft 官方下载链接" + 一次点击直达页面
- **不**自动装(避免 UAC,违背"零环境"原则)

### 3.5 取消 `get_settings_dir()`,改用 `get_data_dir()`

**路径**:`apps/desktop-tauri/src-tauri/src/commands.rs:64`

**改造**:
```rust
#[tauri::command]
pub fn get_data_dir() -> Result<String, String> {
    let install_root = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .ok_or_else(|| "无法定位安装根目录".to_string())?;
    let data_dir = install_root.join("data");
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    Ok(data_dir.to_string_lossy().to_string())
}
```

前端 `App.tsx` 调用点同步改。

### 3.6 启动前确保 4 个子目录

**路径**:`sidecar.rs::start()` 启动前

```rust
for sub in ["data", "cache", "logs", "webview-cache"] {
    let p = install_root.join(sub);
    if let Err(e) = std::fs::create_dir_all(&p) {
        eprintln!("[Hermix] 无法创建 {}: {}", p.display(), e);
    }
}
```

### 3.7 旧 `~/.hermix` 检测(后续 Phase 11 实现)

启动时检测 `~/.hermix` 存在 → 弹"导入旧数据?"对话框 → 3 个按钮(导入/不要/稍后) → 绝不动手。

---

## 四、安装目录结构(锁死)

```
Hermix Desktop/
├── Hermix.exe              # Tauri 主程序,~25MB
├── hermix-server.exe       # Sidecar,~16MB
├── hermix.exe              # 备用 CLI(无 GUI 场景),~16MB
├── webview-cache/          # 微软 WebView2 用户数据(自动生成)
│   ├── EBWebView/
│   └── LocalCache/
├── data/                   # 核心数据(本程序"持久态")
│   ├── config.yaml         # 用户配置
│   ├── USER.md             # 用户画像
│   ├── MEMORY.md           # 长程记忆
│   ├── skills/             # 用户安装的 skills
│   ├── sessions/           # 聊天会话历史
│   ├── cron/               # 定时任务
│   ├── plugins/            # 插件目录
│   ├── .env                # API key 等敏感信息
│   ├── auth.json
│   └── approval_allowlist.json
├── cache/                  # 非持久数据(可随时清)
│   ├── tmp/                # 临时文件
│   ├── logs/               # 日志(滚动 7 天)
│   └── embeddings/         # 未来向量嵌入缓存(Phase L1 用)
├── README.md               # 用户文档
├── LICENSE
└── .portable               # 空文件,标记"这是便携版"
```

**关键约定**:
- `data/` 是**唯一**用户数据来源,删除即恢复出厂
- `cache/` 可清,清掉不影响功能
- `webview-cache/` 由 Tauri/WebView2 自动管理,通常几百 MB,可清

---

## 五、新增 Tauri 命令

### 5.1 `get_data_dir()` — 替代 `get_settings_dir()`

见 §3.5。

### 5.2 `export_portable()` — 导出便携包(P1)

```rust
#[tauri::command]
pub async fn export_portable(app: tauri::AppHandle) -> Result<String, String> {
    // 让用户选目标目录(默认桌面)
    // 把整个 <install_root> 拷到目标目录
    // 进度条:每 100 个文件上报一次
}
```

### 5.3 `check_portable_integrity()` — 自检(P1)

```rust
#[tauri::command]
pub fn check_portable_integrity() -> Result<IntegrityReport, String> {
    // 检查:
    // 1. data/ 存在
    // 2. hermix-server.exe 存在
    // 3. .portable 标记存在
    // 4. WebView2 加载正常
}
```

---

## 六、改造优先级与工作量

### P0 - **核心约束**(1-2 周,所有其他的前置)

| Task | 改动 | 估时 | 阻塞 |
|------|------|------|------|
| **10.1** | `hermix-config::paths::hermix_home()` 加 `HERMIX_PORTABLE_DATA_DIR` 优先级 | 0.5 天 | — |
| **10.2** | Tauri 启动 hook 注入 6 个环境变量 | 1 天 | 10.1 |
| **10.3** | `tauri.conf.json` 关闭 NSIS installer,只出 zip | 0.5 天 | — |
| **10.4** | WebView2 用户数据强制重定向(`data_directory()` API + 环境变量双保险) | 1 天 | 10.2 |
| **10.5** | 删除 `get_settings_dir()`,改用 `get_data_dir()` 走 `install_root` | 0.5 天 | 10.2 |
| **10.6** | `sidecar.rs::start()` 启动前确保 4 个子目录都存在 | 0.5 天 | — |
| **10.7** | 单元测试 + E2E(解压到 D:\,启动,检查所有数据写在 D:\) | 2 天 | 10.1-10.6 |
| **10.8** | `docs/PORTABLE_BUILD.md` 写明构建流程 | 1 天 | 10.1-10.6 |

### P1 - **便携体验**(1-2 周)

| Task | 改动 | 估时 | 阻塞 |
|------|------|------|------|
| **11.1** | `export_portable()` 命令 + 前端"导出便携包"按钮 | 2 天 | 10.5 |
| **11.2** | `check_portable_integrity()` 命令 + 前端"自检"按钮 | 1 天 | 10.5 |
| **11.3** | 启动时显示"便携模式已激活"绿色提示(一次性,可关) | 1 天 | 10.2 |
| **11.4** | 旧 `~/.hermix` 检测 + 一次性"导入?"对话框(3 按钮:导入/不要/稍后) | 1 天 | 10.2 |
| **11.5** | README.md(中文)重写:产品定位、安装方式、便携特性、限制 | 1 天 | 10.1-10.6 |

### P2 - **生态兼容**(2-3 周,可选)

| Task | 改动 | 估时 | 阻塞 |
|------|------|------|------|
| **12.1** | `cli-config.yaml` 强制走 `install_root`(允许 CLI 模式手动覆盖) | 0.5 天 | 10.1 |
| **12.2** | 第三方插件(plugin)加载路径强制扫描 `<install_root>/data/plugins/` | 0.5 天 | 10.1 |
| **12.3** | Skill 路径同上 | 0.5 天 | 10.1 |
| **12.4** | 启动失败时友好提示(检查 install_root 路径,给具体原因) | 1 天 | 10.2 |

### 取消项(明确不做的)

| 原本计划 | 状态 | 理由 |
|---------|------|------|
| macOS / Linux 支持 | ❌ 取消 | 主人明确"暂不考虑其他环境" |
| 17 个 platform gateway | ❌ 取消 | 纯本地工具,违反"便携+本地"定位 |
| CLI TUI(ratatui) | ❌ 取消 | 桌面端,GUI 即可 |
| Dashboard HTTP API 外部端口 | ⚠️ 保留(localhost only) | 给未来调试用 |
| 多用户(2+ 用户同一电脑) | ❌ 取消 | 便携场景,1 用户 |
| 云同步/云备份 | ❌ 取消 | 便携就是"自带",不要云 |
| 安装到系统目录(perMachine) | ❌ 取消 | UAC 恶心,违背便携 |
| 自动更新 | ❌ 取消 | 重新下 zip 即可 |

---

## 七、风险与对策

| 风险 | 概率 | 影响 | 对策 |
|------|------|------|------|
| **WebView2 缺失** | 中(Win10 旧版 ~5%) | 启动失败 | 启动检测,弹窗"去 Microsoft 装",给官方下载链接 |
| **Tauri 沙箱不允许设 `WEBVIEW2_USER_DATA_FOLDER`** | 低 | 重定向失效,数据落 C 盘 | 验证:打包后跑一次,看 `%LOCALAPPDATA%` 是否产生新文件;若失效,改用 `data_directory()` API |
| **用户手贱把 data/ 删了** | 中 | 数据全失 | 启动时检测 `data/config.yaml` 是否存在,缺失则弹"恢复出厂?"对话框 |
| **路径含中文/空格** | 中 | 启动失败 | (1) Tauri sidecar 用 `Command::new(&bin)`(直接传 PathBuf);(2) 启动检测,若含中文/空格弹"建议重命名";(3) GBK 解码兜底(已在 `python_alignment` 有了) |
| **多开(同目录多实例)** | 低 | 端口冲突 | `hermix-server.exe serve` 启动时检测 3000 端口,占用则自动 +1 |
| **hermix-server.exe 路径含空格/中文** | 低 | Tauri 找不到 | 验证:中文路径 `D:\我的工具\Hermix Desktop\` 下完整 E2E |
| **data/ 体积膨胀**(技能装多了) | 中 | U 盘拷不动 | 启动时统计 `data/` 体积,>500MB 时提醒"考虑分卷" |
| **跨平台用户在 macOS/Linux 跑** | 0 | — | 不支持,直接 `#[cfg(windows)]` 锁死,Linux/macOS 编译失败提示"暂只支持 Windows" |
| **新机器没装 WebView2** | 中 | 启动失败 | 启动检测 + 友好提示 + 官方下载链接 |

---

## 八、启动自检(主人纠正后)

**主人原话**: "启动自检应该只是检查后端启动情况,链接情况,各技能、记忆、插件等等的必要的工具加载情况,相当从大脑检查、心跳检查、手脚检查,包括内置环境情况,确保应用启动后是真正可用的。"

**重新设计**(从主人框架"大脑/心跳/手脚/内置环境"四维):

| 类别 | 检查项 | 检查内容 | 失败处理 |
|------|------|---------|----------|
| **🧠 大脑** | 配置完整性 | `data/config.yaml` 存在,API key 已填 | ❌ 阻止启动,引导去设置 |
| **🧠 大脑** | 数据完整性 | `data/USER.md`、`data/MEMORY.md`、`data/sessions/` 存在且可读 | ⚠️ 警告,自动重建空模板 |
| **🧠 大脑** | 记忆系统 | SQLite 能打开、向量索引(若有)能加载 | ❌ 阻止启动,给修复指引 |
| **🫀 心跳** | 后端进程 | `hermix-server.exe` 进程在跑,`/health` 返回 200 | ❌ 自动重启,3 次失败报错 |
| **🫀 心跳** | API 通道 | LLM provider 连接通(ping 一下) | ⚠️ 标记 offline,用户能手动重试 |
| **🫀 心跳** | Tauri IPC | Rust ↔ React 通道正常 | ❌ 阻止启动 |
| **🦵 手脚** | Skills 加载 | 注册的 skills 全部能加载(语法/依赖/权限) | ⚠️ 列出失败,禁用 |
| **🦵 手脚** | Tools 加载 | 30+ 内置工具全部注册成功 | ⚠️ 列出失败,降级到剩余 |
| **🦵 手脚** | Plugins 加载 | 用户安装的 plugins 全部能加载 | ⚠️ 列出失败,降级 |
| **⚙️ 内置环境** | WebView2 | 微软 WebView2 runtime 已安装 | ❌ 弹窗引导去装 |
| **⚙️ 内置环境** | 磁盘空间 | `install_root` 所在盘剩余 > 1GB | ⚠️ 警告 |
| **⚙️ 内置环境** | 写权限 | `data/`、`cache/`、`logs/` 都能写 | ❌ 阻止启动,给原因 |
| **⚙️ 内置环境** | 网络出站 | DNS 通(给 LLM API 域名) | ⚠️ 提示,LLM 调用会失败 |

**3 类失败处理**:
- **致命** (❌ Block): 后端起不来 / data/ 写不了 / WebView2 缺失 / 配置不全 → 红色错误,阻止启动
- **降级** (⚠️ Warn): 部分 skill/tool 加载失败 / API key 没填 / 网络出站失败 → 黄色警告,让用户决定
- **可选** (ℹ️ Info): 磁盘空间低 / 部分 data 缺失 → 蓝色提示,功能受限但能跑

**实现位置**:
- `hermix-server::startup_checks` 模块(P1 阶段实现)
- 启动时阻塞跑 5-10 秒,跑完才 emit `server-ready` 事件给前端
- 前端在 `<TitleBar>` 显示自检状态条(全绿/有黄/有红)

---

## 九、TICK 主循环: 拆解后按组件引入

**主人原话**: "TICK 主循环(持续存在)这个是什么作用给我解释下"

### TICK 是什么

让 Agent 从**"被动响应"**变成**"主动存在"**:

```
[普通 Agent]
  用户发消息 → 处理 → 返回 → 等下次
                          ↑ 永远在等

[TICK Agent]
  TICK ← → [Agent 主循环] ← → [响应]
    ↑           ↓
    │      [后端任务]
    │           ↓
  TICK ←  [TickScheduler]
    │
    ├── 20:00 夜深: 每 30 分钟一次(几乎不做事)
    ├── 09:00 早晨: 主动"出早报"
    └── 15:00 下午: 检查项目进度
```

**TICK 的核心能力**:
1. **时间感知**: 知道"现在几点",根据时间切换行为
2. **主动唤醒**: 即使没人发消息,自己跑任务
3. **自适应节奏**: 忙时秒级 tick,闲时 30 分钟 tick
4. **持续状态**: 不重启就不丢失"我是谁"的状态

### TICK 能力拆解 + 桌面端适用性

| TICK 能力 | 桌面端是否需要 | 桌面端实现 |
|---------|--------------|----------|
| **时间感知** | ⭐ 需要(P2) | "早上好,出今日早报" 确实好使 |
| **主动唤醒** | ⚠️ 可要可不要 | 有 cron 替代,但 TICK 更"自主" |
| **持续状态(不重启不丢)** | ⭐⭐ 很重要 | FocusStack 已在解决(P0) |
| **后台任务调度** | ✅ 已有 | hermix-cron 是 tick 模式 |
| **自适应节奏** | ❌ 不需要 | 桌面端 GUI 用户在场,不需要自适应 |
| **心电图可视化** | ⭐⭐⭐ 桌面端特有 | 大屏可视化,值得做(P1) |

### 结论: TICK 不是"取消",是"拆解后按优先级引入各组件"

| 组件 | 优先级 | 位置 |
|------|--------|------|
| **时间感知** | P2 | 单独 feature: `hermix-agent::time_aware` |
| **心电图可视化** | P1 | desktop-tauri 意识流 UI |
| **持续状态** | P0 | 已被 FocusStack 覆盖 |
| **后台调度** | P1 | hermix-cron 已实现 |
| **TICK 主循环(整体)** | ❌ 取消 | GUI 已足够,不需要"持续存在" |
| **自适应节奏** | ❌ 取消 | 桌面端不需要 |

---

## 十、BaiLongma 启发方案: 不砍,按优先级引入

**主人原话**: "我需要你能够将刚开始的方案的能力慎重思考下,我觉得都可以的,为啥有那么多不行了"

**反思**: 之前我犯的逻辑错误 —— 把"延后"和"取消"混淆,把"产品定位"和"技术能力"混淆。**桌面端是产品定位,工具路由/记忆层/心电图是技术能力,后者对任何形态 Agent 都有价值,只是优先级不同**。

### 完整能力清单(12 项,无遗漏)

| # | 能力 | 真实价值 | 优先级 | Phase |
|---|------|---------|--------|-------|
| **H1** | **工具路由**(省 92% token) | 💰💰💰💰 LLM 成本直降 | **P0** | Phase 7 |
| **H5** | **统一 MemoryLayer + FocusStack 5 状态** | 🧠🧠🧠🧠 跨会话记忆有结构 | **P0** | Phase 8 |
| **H2** | **意图预取**(写早报并行调天气+新闻+日程) | ⚡⚡⚡⚡ 桌面端大屏可视化最佳 | **P1** | Phase 13 |
| **H3** | **意识流 UI 全屏心电图** | 🎨🎨🎨🎨 桌面端特有 | **P1** | Phase 14 |
| **M4** | **预热缓存层**(moka + TTL) | ⚡⚡⚡ 启动加速 | **P2** | Phase 15 |
| **M5** | **ACUI**(AI 主动推结构化 UI,改 React 组件版) | 🎨🎨🎨 代替纯文本 | **P2** | Phase 16 |
| **L1** | **本地向量嵌入**(ONNX + usearch) | 🔒🔒🔒🔒 离线+隐私+速度 | **P2** | Phase 17 |
| **H4** | **时间词召回**(昨天/上周) | 🧠🧠🧠 跟 FocusStack 配合 | **P2** | Phase 18 |
| **觉醒期** | **启动自检(13 项,主人框架)** | 🛡️🛡️🛡️🛡️ 确保真可用 | **P1** | Phase 19 |
| **Watchdog** | **单轮 180s 强 abort** | 🛡️🛡️🛡️ 防卡死 | **P1** | Phase 19 |
| **时间感知** | **"早上好,出今日早报"** | ⭐⭐ 小功能 | **P2** | Phase 20 |
| **桌面端工具瘦身** | **30+ 工具 → 15** | ⚡⚡⚡ 体积 16MB → 10MB | **P2** | Phase 12 |

### 完整 Phase 排序(P0 → P1 → P2)

| Phase | 内容 | 估时 | 优先级 |
|------|------|------|--------|
| **10** | 零环境 + 可移植(7 项工程改造) | 2-3 周 | P0 |
| **7** | 工具路由(省 92% token) | 2-3 周 | P0 |
| **8** | 统一 MemoryLayer + FocusStack | 3-4 周 | P0 |
| **11** | 旧 ~/.hermix 兼容 | 1 周 | P1 |
| **13** | 意图预取(写早报) | 2 周 | P1 |
| **14** | 意识流 UI 全屏心电图 | 1-2 周 | P1 |
| **19** | 启动自检 + Watchdog | 1-2 周 | P1 |
| **12** | 30+ 工具瘦身到 15 | 2-3 周 | P2 |
| **15** | 预热缓存层 | 1 周 | P2 |
| **16** | ACUI React 组件 | 1-2 周 | P2 |
| **17** | 本地向量嵌入 | 2 周 | P2 |
| **18** | 时间词召回 | 1 周 | P2 |
| **20** | 时间感知小功能 | 1 周 | P2 |

**总计**: ~20-26 周(约 5-6.5 个月),P0 7-10 周完成,后面 P1-P2 逐步堆。

### 仍取消的(明确不做的)

| 项 | 理由 |
|---|------|
| **TICK 主循环整体** | GUI 已足够,不需要"持续存在" |
| **TICK 自适应节奏** | 桌面端不需要 |
| **扫描 SSH/Git/桌面/其他 AI Agent** | 跟启动自检是两码事,启动自检是健康检查不探测外部 |

---

## 十一、附录:核心改造点速查(给"动手做"用)

```
Phase 10 改造点:
1. crates/hermix-config/src/paths.rs:122
   → hermix_home() 加 HERMIX_PORTABLE_DATA_DIR 优先级
2. apps/desktop-tauri/src-tauri/src/sidecar.rs:94
   → Command::new() 之前 .env(...) 注入 6 个变量
3. apps/desktop-tauri/src-tauri/tauri.conf.json
   → bundle.targets 改 [] 或保留 nsis 但加 zip
4. apps/desktop-tauri/src-tauri/src/lib.rs
   → WebviewWindowBuilder 加 data_directory()
5. apps/desktop-tauri/src-tauri/src/commands.rs:64
   → get_settings_dir() → get_data_dir() 走 install_root
6. apps/desktop-tauri/src/App.tsx
   → useStreamChat 改用 get_data_dir()
7. docs/PORTABLE_BUILD.md (新建)
   → 写明构建流程 + 验证步骤
8. README.md (重写)
   → "可移植桌面端"定位
```

---

## 十二、决策点(等主人拍板)

| # | 决策点 | 选项 | 我的推荐 |
|---|--------|------|----------|
| **1** | installer 出局? | (A) 只出 zip,完全零安装 / (B) 保留 NSIS,首次启动检测 WebView2 | **A** |
| **2** | WebView2 缺失怎么办? | (A) 启动弹窗,引导去 Microsoft 装 / (B) 自动下载静默装(违背零环境) | **A** |
| **3** | 旧 ~/.hermix 数据? | (A) 检测到就弹"导入?"对话框 / (B) 完全无视,用户手动迁移 | **A** 绝不动手 |
| **4** | 多用户? | (A) 1 用户 1 目录 / (B) 1 目录多 profile | **A** |
| **5** | CLI 模式还要不要? | (A) 保留 hermix.exe(纯命令行) / (B) 砍掉,只 GUI | **A** 备用 |
| **6** | data/ 体积警告阈值? | (A) 500MB 提醒 / (B) 1GB 提醒 / (C) 不提醒 | **A** |
| **7** | 桌面端工具瘦身范围? | (A) 大刀阔斧,只留 15 个核心 / (B) 微调,留 25 个 | **B** |
| **8** | Phase 7 工具路由实施起点? | (A) L0 关键词+LRU(快,1 周) / (B) L1 + usearch 向量(2 周) | **A** 先行 |
| **9** | Phase 8 记忆层抽象? | (A) 8 plugin 适配统一 trait / (B) 1 个本地 embedded + 7 plugin 适配 | **A** |

---

**状态**: ✅ 完整方案定稿(无遗漏)
**作者**: Senior Developer (hermix-agent-rs 项目组)
**下一步**: 根据 §十二 决策点 → 进入 Phase 10 实施
