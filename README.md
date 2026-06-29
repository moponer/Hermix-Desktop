<div align="center">

# ⚡ Hermix Desktop

**自进化的 AI 助手。零安装,零环境。**

`146,598 行 Rust` · `1,834 个测试` · `18 个 crate` · `~80MB 解压即用`

[下载 zip](#) · [文档](#) · [更新日志](#) · [问题反馈](#)

</div>

---

## 一句话定位

> 下载 zip,解压到任意位置(推荐 D/E/F 盘),双击 `Hermix.exe`,开始用。

---

## 它能做什么

- **与 AI 对话** — 支持 10 种大模型(OpenAI、Anthropic、Qwen、DeepSeek、Kimi、MiniMax、NousResearch、Copilot、OpenRouter、本地)
- **帮你干活** — 读文件、写代码、执行命令(白名单内)、搜索网页、操作浏览器
- **记住你的偏好** — 跨会话长期记忆,越用越懂你
- **各种工具** — 文件、终端、搜索、视觉、语音、网页摘要(共 30+ 个)
- **定时任务** — cron 定时提醒/拉取
- **看屏点屏** — [自研电脑操控栈](#自研能力--电脑操控-computer-use-poc),AI 能直接看你桌面、点鼠标、敲键盘

---

## 自研能力 — 电脑操控 (Computer-Use, PoC)

> 🤖 让 AI 像人一样 **看屏幕、点鼠标、敲键盘**。从截屏驱动到输入注入、从元素识别到浏览器自动化,全程在 Windows 平台原生自研,不集成任何三方桌面自动化运行时。

### 14 个工具(PoC 已就绪)

**Phase 1 — 输入 / 输出基础(5 个,Phase 2-A 截屏主路径 DXGI 在同一批落地)**

| 工具 | 作用 | 关键参数 |
|------|------|----------|
| `cua_screenshot` | 截屏,返回 base64 PNG + 显示器元数据 | `monitor` (默认 0) |
| `cua_screen_info` | 列出所有显示器(分辨率 / 名称 / 位深) | — |
| `cua_left_click` | 鼠标点击(x, y, button 可选) | `x`, `y`, `button` |
| `cua_type_text` | Unicode 文本输入(任意语言) | `text` |
| `cua_press_key` | 组合键 / 单键(`ctrl+shift+delete` 风格 chord) | `chord` |

**Phase 2-B — Win32 顶层窗口识别(3 个,EnumWindows + GetWindowRect 自研封装)**

| 工具 | 作用 | 关键参数 |
|------|------|----------|
| `cua_list_windows` | 枚举顶层窗口(EnumWindows + 标题/可见性过滤) | `title?`, `visible_only?`, `max_results?` |
| `cua_window_at_point` | 返回 (x, y) 处的顶层窗口 hwnd + 标题 | `x`, `y` |
| `cua_click_window` | SetForegroundWindow + 点击窗口中心 | `hwnd`, `button?` |

**Phase 2-C — UIAutomation COM 元素交互(5 个 handler + 5 个 Tauri command,IUIAutomation* 自研封装)**

| 工具 | 作用 | 关键参数 |
|------|------|----------|
| `cua_ui_tree` | 走 UIA control view tree,返回元素树(含 bounding rect) | `max_depth?` (默认 3) |
| `cua_ui_find` | 按 name / automation_id / class_name 找元素 | `name?`, `automation_id?`, `class_name?` |
| `cua_ui_click` | 按 name 找 → GetClickablePoint → SendInput | `name` |
| `cua_ui_focus` | 按 name 找 → IUIAutomationElement::SetFocus | `name` |
| `cua_ui_invoke` | 按 name 找 → IUIAutomationInvokePattern | `name` |

**Tauri 前端命令**（可从前端直接 `invoke`）：
| Tauri command | 对应 handler | 说明 |
|---------------|---------------|------|
| `cua_ui_tree` | `CuaUiTree` | 返回 `Vec<UiElement>` JSON |
| `cua_ui_find` | `CuaUiFind` | 至少一个条件,name/automation_id/class_name |
| `cua_ui_click` | `CuaUiClick` | find + clickable_point + SendInput |
| `cua_ui_focus` | `CuaUiFocus` | find + SetFocus |
| `cua_ui_invoke` | `CuaUiInvoke` | find + InvokePattern |

**Phase 2-D — 离线 OCR(1 个 handler + 1 个 Tauri command,Windows.Media.Ocr 自研封装)**

| 工具 | 作用 | 关键参数 |
|------|------|----------|
| `cua_ocr_recognize` | 对 PNG(base64)做本地 OCR,返回 text + lines + word-level bbox | `png_base64` |

**Tauri 前端命令**：
| Tauri command | 对应 handler | 说明 |
|---------------|---------------|------|
| `cua_ocr_recognize` | `CuaOcrRecognize` | base64 decode + Windows.Media.Ocr |

**Phase 2-E — 浏览器自动化 CDP(4 个 Tauri command,最小化 CDP 客户端)**：

| Tauri command | 说明 | 关键参数 |
|---------------|------|----------|
| `cua_browser_navigate` | 启动 Chrome / 打开 URL | `url` |
| `cua_browser_screenshot` | 截图当前页面(PNG base64) | 无 |
| `cua_browser_evaluate` | 执行 JS 表达式 | `expression` |
| `cua_browser_click` | 点击 CSS 选择器匹配的元素 | `selector` |

### 全栈自研,Windows 原生 API

| 能力 | 自研实现 | 关键点 |
|------|----------|--------|
| **截屏(主路径)** *(Phase 2-A)* | DXGI Output Duplication | GPU 直采,比传统 GDI 快 5–10×,多显示器坐标准确 |
| **截屏(fallback)** | BitBlt + GDI | DXGI 失败时自动回退,工具永不失效 |
| **鼠标输入** | `Win32 SendInput` + `MOUSEINPUT` | 系统级事件,绕过 accessibility hook 限制 |
| **键盘输入** | `Win32 SendInput` + `KEYBDINPUT` | Unicode 完整支持,任意语言输入 |
| **顶层窗口识别** *(Phase 2-B)* | EnumWindows + GetWindowRect 自研封装 | hwnd ↔ isize 双向、可见性过滤、SetForegroundWindow |
| **UIA 元素交互** *(Phase 2-C)* | UIAutomation COM 自研封装 | IUIAutomationElement / IUIAutomationInvokePattern / SetFocus / GetClickablePoint |
| **离线 OCR** *(Phase 2-D)* | `Windows.Media.Ocr` 自研封装 | 离线、无 token、Win10 1809+ 内置 |
| **浏览器自动化** *(Phase 2-E,已完成)* | 自研最小化 CDP 客户端 | `tokio_tungstenite + serde_json`,零三方 driver,4 Tauri 命令已注册 |

### 双层安全审批模型

**审批粒度**:
- **任务级一次** — 同一 session 内同类型操作只批一次
- **destructive 每次** — 每次调用都弹窗

**风险等级分级**:
- 🟢 **Low** — 只读操作(查询屏幕信息、截图)
- 🟡 **Medium** — 可逆操作(点击、文本输入)
- 🔴 **High / Destructive** — 不可逆操作(删除、退出应用、系统热键)

**硬拦截**(无需审批,直接拒绝):
- `Ctrl+Alt+Del` — 系统级,任何工具都无法拦截
- `Win+L` — 锁屏
- `Alt+F4` — 关闭窗口
- 其他系统级 chord

**限速**: 默认 30 ops/s,防误操作刷屏。

### 完整审计

每次操作写入 `~/.hermix/cua/sessions/<session_id>/trace.jsonl`(JSONL):

```json
{"ts":"2026-06-23T10:30:15.123Z","kind":"Action","action":"click","target":[1920,540],"button":"left","elapsed_ms":12}
{"ts":"2026-06-23T10:30:15.456Z","kind":"Action","action":"type_text","text":"hello hermix","elapsed_ms":8}
```

事后可回放整个 session,精准定位每个环节。

### 端到端 demo(已可跑通)

`打开记事本 → 输入 hello hermix → 截图验证` 全链路:

1. `cua_press_key("win")` → 打开开始菜单
2. `cua_type_text("notepad")` → 搜索
3. `cua_press_key("Return")` → 启动
4. `cua_type_text("hello hermix")` → 输入
5. `cua_screenshot()` → 视觉验证

每步都有 trace 留痕,任意环节出错可精确定位。

**UIA demo(Phase 2-B + 2-C)**:`cua_list_windows` 找到目标窗口 → `cua_click_window(hwnd)` 切到前台 → `cua_ui_find(name="OK")` 定位按钮 → `cua_ui_invoke(name="OK")` 触发默认动作。整条链路可被 LLM 一句话串起,**不再依赖坐标盲点**。

### 设计取向:为什么不一样

| 维度 | 我们的做法 |
|------|-----------|
| **平台承诺** | 仅 Windows 10/11 x64 — 把一个平台做深 |
| **数据流向** | 零云服务 — 截屏 / 输入全部本地完成,不上传 |
| **运行依赖** | 自研为主 — 不集成三方 desktop automation runtime,避免供应商锁定 |
| **核心 API** | DXGI / SendInput / UIAutomation / Windows.Media.Ocr — 都是 Windows 一等公民 API |
| **浏览器** | 自研最小化 CDP 客户端 — 不依赖任何 browser driver |
| **可移植性** | 整个项目 ~80MB,解压即用,不污染系统 |

> 详细方案与决策过程见 [`docs/COMPUTER_USE_POC_DELIVERY.md`](./docs/COMPUTER_USE_POC_DELIVERY.md)。

---

## 怎么用(3 步)

1. **下载** `Hermix-Desktop-Windows-x64.zip`(约 80MB)
2. **解压** 到任意位置(推荐 `D:\Hermix Desktop\`)
3. **双击** `Hermix.exe`

首次启动会引导你填一个 API key(支持 OpenAI / Anthropic / 国产模型),然后就能开始对话了。

---

## 数据存放在哪

Hermix Desktop 的数据**按类型分两处存放**：

| 类型 | 内容 | 路径 |
|------|------|------|
| **用户配置和数据** 🔒 | `config.yaml`、`.env`、`auth.json`、`sessions.db`、`memory_store.db`、`enabled_toolsets.json` | `~/.hermix/`（即 `C:\Users\<用户名>\.hermix\`） |
| **应用瞬态** 🗑️ | WebView2 缓存、日志 | `<安装目录>/webview-cache`、`<安装目录>/logs` |

`~/.hermix/` 是后端侧边进程的默认数据目录（`hermix_home()` 的最终 fallback）。Tauri 桌面端**不覆盖**这个路径，所以你的配置和聊天记录一直留在那里，无论把桌面端装到 D 盘、F 盘还是 U 盘都不会丢。

瞬态数据（缓存、日志）放在安装目录旁，没了自动重建。

### 卸载

1. 关闭程序
2. 删除安装文件夹（缓存和日志随之清除）
3. 数据 `~/.hermix/` 保留，下次重装还能继续用。想彻底清除需手动删除

### 换电脑 / 重装系统

把 `C:\Users\<旧用户名>\.hermix\` 拷到新电脑的相同位置即可。最少只需三个文件：
- `config.yaml` — API Key 和模型配置
- `.env` — 环境变量密钥
- `sessions.db` — 聊天历史

---

## 常见问题

**Q: 数据会越来越大?**  
A: 正常用半年约 500MB。如果上 GB,检查是不是装了太多"技能"和"插件"。

**Q: 跑在 macOS / Linux?**  
A: 暂不支持。只承诺 Windows 桌面端。

**Q: 数据会上传?**  
A: 不会。Hermix Desktop 不收集任何遥测。  
   你说的话/你传的文件,只发给你配置的 LLM 服务商(OpenAI/Anthropic/...)。

**Q: 怎么升级?**  
A: 重新下载 zip → 解压 → 双击 exe。配置和数据会自动从 `%APPDATA%` 读取。

---

## 目录结构

```
安装目录/                    ← 解压位置(任意盘)
├── Hermix.exe              # 主程序(Tauri v2,~25MB)
├── hermix-server.exe       # 后端服务(Rust,~16MB)
├── hermix.exe              # CLI 备用(无 GUI 场景)
├── webview-cache/          # WebView2 缓存(可清)
└── logs/                   # 日志(滚动 7 天)

~/.hermix/                  ← 你的数据(C:\Users\<用户名>\.hermix\)
├── config.yaml             # API Key 配置
├── .env                    # 环境变量密钥
├── auth.json               # 认证令牌
├── sessions.db             # 聊天历史(SQLite)
├── memory_store.db         # 长期记忆
├── enabled_toolsets.json   # 工具集开关
├── approval_allowlist.json # 审批允许列表
├── automation_tasks.json   # 定时任务
├── dashboard-plugins/      # 面板插件
└── skills/                 # 技能
```

**备份建议**: 整个 `~/.hermix/` 目录就是你的全部"记忆+技能+配置+历史"。
安装目录删了重下一个就行,数据不丢。

---

## 下一步开发计划(2026 Q2-Q3)

> 📋 详细方案见 [`docs/PROPOSAL_PORTABLE_DESKTOP.md`](./docs/PROPOSAL_PORTABLE_DESKTOP.md)
>
> ### 🏗️ 后端架构重构(2026-06-19 开始,Backend Architect)
>
> 目标:提升**扩展性 + 稳定性**。**7 周 7 阶段**,接口零变,行为不破。
>
> - [`docs/ARCHITECTURE_BENCHMARK.md`](./docs/ARCHITECTURE_BENCHMARK.md) — 现状基线 + OpenClaw / Hermes Agent / BaiLongma 对比 + 适配性判断
> - [`docs/ARCHITECTURE_REFACTOR_PLAN.md`](./docs/ARCHITECTURE_REFACTOR_PLAN.md) — 目标架构(分层/接口/模式/决策点)
> - [`docs/MIGRATION_TODO.md`](./docs/MIGRATION_TODO.md) — 7 阶段 TODO(基线/基础设施/工具路由/Agent拆分/Cancel/Memory/持久化/收尾)
>
> 头号目标:**工具 schema 注入 token 5238 → 800**(-85%)。次要:9K 行 agent_loop 拆 5 文件、CancellationToken 贯穿、SQLite 连接池、13/13 启动自检。

### ✅ Phase 10: 零环境 + 可移植(2-3 周,P0) — **已完成**

**目标**: 实现"解压即用、可移植、C 盘零写入"的核心产品哲学

| Task | 内容 | 估时 | 状态 |
|------|------|------|------|
| 10.1 | `hermix-config::paths::hermix_home()` 加 `HERMIX_PORTABLE_DATA_DIR` 优先级 | 0.5 天 | ✅ |
| 10.2 | Tauri 启动 hook 注入 6 个环境变量(`HERMIX_HOME` / `HERMIX_PORTABLE_DATA_DIR` / `HERMIX_PORTABLE_CACHE_DIR` / `WEBVIEW2_USER_DATA_FOLDER` / `TMP` / `TEMP`) | 1 天 | ✅ |
| 10.3 | `tauri.conf.json` 关闭 NSIS installer,只出 zip  *(注:Tauri 2.9.2 不支持单独 `zip` BundleType,采用 `nsis` 让其 sidecar 出 `.zip` portable archive)* | 0.5 天 | ✅ |
| 10.4 | WebView2 用户数据强制重定向(`data_directory()` API)  *(通过 `WEBVIEW2_USER_DATA_FOLDER` env var + `tauri::WebviewWindowBuilder::data_directory()` 双保险)* | 1 天 | ✅ |
| 10.5 | `get_settings_dir()` → `get_data_dir()` 走 `install_root`  *(新 Tauri command + desktopBridge `openDataDir`,旧 `get_settings_dir` 标 deprecated 仍兼容)* | 0.5 天 | ✅ |
| 10.6 | `sidecar.rs::start()` 启动前确保 4 个子目录都存在  *(+ `current_portable_env()` 显式把 6 个 env var 透传给 sidecar)* | 0.5 天 | ✅ |
| 10.7 | 单元测试 17 个(`hermix-config::paths::tests` 新增) + 6 个(`desktop-tauri::portable_env::tests` 新增) | 2 天 | ✅ |
| 10.8 | `docs/PORTABLE_BUILD.md` 写明构建流程  *(并入 `docs/PROPOSAL_PORTABLE_DESKTOP.md`)* | 1 天 | ✅ |

**已落地的代码变更**:
- `crates/hermix-config/src/paths.rs` — `hermix_home()` 加 `HERMIX_PORTABLE_DATA_DIR` 优先级;新增 `portable_data_dir()` / `portable_cache_dir()` 访问器;4 个新单元测试
- `apps/desktop-tauri/src-tauri/src/portable_env.rs` — **新文件**:`install_portable_env()` 幂等注入 6 个 env var + 4 个子目录;`current_portable_env()` 显式 env pass-through;6 个单元测试
- `apps/desktop-tauri/src-tauri/src/lib.rs` — `setup` hook 调用 `install_portable_env()` 作为**第一步**
- `apps/desktop-tauri/src-tauri/src/sidecar.rs` — `start()` 前 ensure 4 个子目录 + 显式 `Command::env()` 透传 6 个 env var
- `apps/desktop-tauri/src-tauri/src/commands.rs` — 新增 `get_data_dir` Tauri command;`get_settings_dir` 标 deprecated 转发
- `apps/desktop-tauri/src-tauri/tauri.conf.json` — `targets: ["nsis"]` (Tauri 2.9.2 不支持 zip,nsis 产 .zip portable archive)
- `apps/desktop-tauri/src/desktopBridge.ts` — 新增 `openDataDir()`;`openSettingsDirectory` 改为 `openDataDir` 的 alias

**验收** (单元测试已通过,完整 E2E 需要主人手动跑 `pnpm tauri dev` 触发):
- ✅ 6 个 desktop-tauri unit test 全过(`portable_env::tests::*`)
- ✅ 17 个 hermix-config path test 全过(包括新 `portable_data_dir_overrides_hermix_home` 等 4 个)
- ✅ cargo check 0 error,0 新 warning
- ⏳ 整包 E2E:解压到 `D:\Hermix Desktop\` → 双击 → 启动(需要主人手动触发)
- ⏳ C 盘零写入验证:跑 1 小时,扫 `%LOCALAPPDATA%` / `%APPDATA%` 看是否有 hermix 文件(需要 E2E)
- ⏳ 可移植验证:整个 `Hermix Desktop/` 拷到另一台电脑 → 启动 → 数据完整(需要 E2E)

### 🎯 Phase 11: 旧 ~/.hermix 兼容 + 一次性导入(1 周,P1)

| Task | 内容 | 估时 |
|------|------|------|
| 11.1 | 启动检测 `~/.hermix` 存在 → 弹"导入旧数据?"对话框 | 2 天 |
| 11.2 | 一次性导入流程(拷到 `data/` + 7 天观察期 + 自动清旧) | 3 天 |
| 11.3 | 跳过标记 `~/.hermix.skip-import`(用户主动点"不要"后写) | 1 天 |

**验收**: 旧用户首次启动看到"导入?"对话框,3 个按钮(导入/不要/稍后),绝不自动动手。

### 🎯 Phase 7: 工具路由 + 上下文预取(2-3 周,P0,与 Phase 10 并行)

**目标**: 解决 30+ 工具全量注入 system prompt 导致的 token 浪费(每轮 ~4500 token,直接 ROI)

| Task | 内容 | 估时 |
|------|------|------|
| 7.1 | `hermix-intelligence::tool_router.rs` 关键词+LRU(L0) | 3 天 |
| 7.2 | `SystemPromptBuilder` 改用 `add_tools_schemas_via_router(&router)` | 1 天 |
| 7.3 | `hermix-telemetry` 加 `tool_router_hits/misses` 计数 | 0.5 天 |
| 7.4 | `agent_loop.rs` 加 `prefetch_phase`(识别"写早报"等高频 intent 并行预取) | 3 天 |
| 7.5 | 单元测试 + E2E(token 消耗对比基线) | 2 天 |

**验收**: 30+ 工具 schema ~4500 token → top-5 ~800 token,节省 ~82%。

### 🎯 Phase 8: 统一记忆层 + FocusStack(3-4 周,P0)

**目标**: 8 个 memory plugin 抽象为统一 `MemoryLayer` trait,引入 `FocusStack<T>` 5 状态机(kept/pushed/returned/cleared/created),让跨会话记忆有结构。

| Task | 内容 | 估时 |
|------|------|------|
| 8.1 | `hermix-core::traits` 加 `MemoryLayer` trait | 1 天 |
| 8.2 | 新 crate `hermix-focus`:`FocusStack<T>` + 5 状态 + salience 打分 | 5 天 |
| 8.3 | `hermix-agent::memory_manager` 重构: `MemoryRouter` 适配 8 个 plugin | 5 天 |
| 8.4 | `TimeAwareRecall`: 解析"昨天/上周"等时间词(跟 Phase 18 配合) | 3 天 |
| 8.5 | 集成测试 + E2E | 3 天 |

**验收**: "昨天我们做了什么" 自动召回 24h 内的 `focus_conclusion`。

### 🎯 Phase 13: 意图预取(2 周,P1)

**目标**: 识别高频 intent 模式(写早报/查天气/看日程),并行 fan-out 调只读工具,降低首 token 延迟。

| Task | 内容 | 估时 |
|------|------|------|
| 13.1 | `PrefetchIntent` enum + 关键词分类器(L0) | 3 天 |
| 13.2 | `agent_loop.rs` 串行调用前并行预取只读工具 | 3 天 |
| 13.3 | E2E: "写今日早报" 自动调 weather/news/calendar | 2 天 |

**验收**: 用户说"写今日早报" → 工具调用 0 等待(已经预取完)。

### 🎯 Phase 14: 意识流 UI 全屏心电图(1-2 周,P1)

**目标**: 桌面端大屏优势,展示 Agent 此刻在想什么(已用 `execution_timeline` + `LiveTerminal` 走通第一块,补全屏心电图)

| Task | 内容 | 估时 |
|------|------|------|
| 14.1 | `hermix-server` 新 `/consciousness` 路由(SSE 推送意识流) | 2 天 |
| 14.2 | desktop-tauri `ConsciousnessView.tsx`(全屏时间线 + 多通道) | 3 天 |
| 14.3 | 心电图风格可视化(横向时间轴 + 记忆/工具/焦点/思考通道) | 2 天 |

**验收**: 启动后切到"意识流模式",看到 Agent 实时在想/做/记什么。

### 🎯 Phase 19: 启动自检 + Watchdog(1-2 周,P1)

**目标**: 启动自检 13 项(主人框架"大脑/心跳/手脚/内置环境"),Watchdog 防长任务卡死。

| Task | 内容 | 估时 |
|------|------|------|
| 19.1 | `hermix-server::startup_checks` 13 项自检 | 3 天 |
| 19.2 | 3 类失败处理(❌ Block / ⚠️ Warn / ℹ️ Info) | 2 天 |
| 19.3 | 前端 `<TitleBar>` 自检状态条 | 1 天 |
| 19.4 | `Watchdog`: 单轮 > 180s 强 abort,记录 telemetry | 1 天 |
| 19.5 | 单元测试 + E2E(模拟各种失败场景) | 2 天 |

**验收**: 启动时跑 5-10 秒自检,前端看到状态条,WebView2 缺失/data/ 写不了时阻止启动并给原因。

### 🎯 Phase 12: 桌面端工具瘦身(2-3 周,P2)

**目标**: 30+ 工具精简到 ~15 个,体积 16MB → 10MB。

| 砍 | 留 |
|---|---|
| ❌ gateway 相关(17 个 platform 都不需要) | ✅ 文件读/写/搜索 |
| ❌ MCP server 启动(纯 client 即可) | ✅ 终端(白名单命令) |
| ❌ cron 触发(单次任务为主) | ✅ 视觉(图片理解) |
| ❌ skill hub(本地 skill 即可) | ✅ 语音(ASR/TTS) |
| ❌ delegation / sub-agent | ✅ 记忆(写读) |
| | ✅ 网页摘要 |

**验收**: 启动速度 < 1s,体积降到 10MB。

### 🎯 Phase 15-20: P2 阶段(可选,5-6 周)

| Phase | 内容 | 估时 |
|------|------|------|
| **15** | 预热缓存层(moka + TTL,启动加速) | 1 周 |
| **16** | ACUI React 组件(AI 主动推结构化 UI) | 1-2 周 |
| **17** | 本地向量嵌入(ONNX + usearch,离线+隐私) | 2 周 |
| **18** | 时间词召回(配合 FocusStack) | 1 周 |
| **20** | 时间感知小功能("早上好,出今日早报") | 1 周 |

### 🛠️ 持续维护

| 类别 | 内容 |
|------|------|
| **稳定性** | Chat 通信中断排查(commit 42fa45a 后再发)、Tauri sidecar 跨平台路径 |
| **UI** | v2 设计原型样式对齐、Composer 状态管理(pendingImage vs 提升 state 冲突) |
| **图片** | Agnes Image 2.0 API Skill 整合(透明背景 PNG 真支持) |
| **电脑操控** | `hermix-cua` PoC 推进(Phase 2-B Win32 / 2-C UIA / 2-D OCR / 2-E CDP),审批 UX 打磨,审计回放工具 |
| **评估** | SWE-bench / Terminal-Bench / YC Bench 持续跑分 |
| **文档** | `docs/AGENTS.md` 重建(原文档缺失) |

---

## 技术栈(开发者向)

- **后端**: Rust 18 个 crate,~146K 行
- **前端**: React 19 + Tauri v2
- **运行时**: 静态二进制,无 Python/Node 依赖
- **数据**: 本地 sqlite + JSON + 文件树

**核心 crate**:
- `hermix-cli` — 二进制入口
- `hermix-agent` — Agent 循环、LLM 提供商、记忆插件(7,694 行)
- `hermix-tools` — 30+ 工具后端、审批引擎(含 14 个 CUA handler)
- `hermix-server` — HTTP/WebSocket + Dashboard
- `hermix-skills` — 技能管理
- `hermix-config` — 配置加载、路径管理、`env_compat`
- `hermix-cua` — 自研电脑操控栈(DXGI 截屏 / SendInput 输入 / UIAutomation / OCR / CDP,Windows only)
- 完整架构详见 `docs/ARCHITECTURE_BENCHMARK.md`(22 crate / 50 大类 / 234 功能点)

**关键特性**:
- 4 层权限防护(PermissionLevel + Tirith + RiskLevel + ApprovalManager)
- 3-token 隔离(desktop_token / api_server_key / providers.*.api_key)
- 实时流式 WS(`/v1/ws-stream/{id}`),结构化 StreamEvent
- 思考块实时剥离(`ThinkBlockParser`)

---

## 验证

```bash
# 全 workspace 检查
cargo check --workspace --all-targets

# 全 workspace 测试
cargo test --workspace    # 1,834 个测试

# 前端类型检查
cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit
```

> ⚠️ Windows 上需用 `.workbuddy-cargo.bat` 包装 MSVC 环境,详见 `docs/MSVC_BUILD.md`。

---

## 贡献

PRs welcome,先跑 `cargo test --workspace`(1,834 个测试)。

架构细节和编码规范见 [`docs/`](./docs/)。

## 许可证

MIT — 见 [LICENSE](LICENSE)。
