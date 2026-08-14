---
name: custom-app-development
version: 13
description: >
  自定义应用开发助手。当用户要求创建/开发一个桌面应用、桌面工具、独立程序、
  窗口应用、软件、小软件、打包成 EXE 的程序时使用。通过 custom_app_manager 工具
  创建、更新、打包、运行(dev测试版)、校验、冒烟用户自定义桌面应用。
  基于 hx-shell 运行时（wry/WebView2，单文件 exe），LLM 写前端代码
  （TypeScript/React/纯 HTML），Hermix 打包成单文件 exe 分发。
  用户电脑零工具链：打包 = 将 HTML/JS 作为 PE 资源节(RT_RCDATA)嵌入 hx-shell.exe，
  WebView2 即跑，无尾部注入、无结构型 Defender 误报。
  务必在以下场景使用：用户说"做个应用""开发一个应用""写个应用""生成一个应用"
  "开发一个工具""做个工具""写个小软件""做个软件""做一个桌面程序""做一个独立程序"
  "帮我做个 app""创建自定义应用""写个桌面工具""打包成 exe""make an app"
  "create a desktop app""build a tool""develop a software""package as exe"；
  或用户描述一个需要独立窗口运行的功能需求时；
  或**用户报告某个已安装的自定义应用出错**——如"我的音乐播放器打不开""应用加载失败 (os error 3)""XXX 应用崩溃/报错""本地音乐运行时出错"等。凡是"工具/应用/软件/桌面程序"类诉求，
  以及"已安装应用的报错/修复/打不开"类诉求，一律判定为自定义应用并走本 skill，不要调其他通用代码工具，
  更不要自己去 `dir`/`ls` 仓库目录或硬猜 HERMIX_HOME 路径——查/改应用永远只走 `custom_app_manager`。
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
  - 做个应用
  - 开发应用
  - 开发工具
  - 做个工具
  - 写个应用
  - 写个软件
  - 做个软件
  - 独立程序
  - 窗口应用
  - 桌面程序
  - 小软件
  - 软件
  - 工具
  - 应用
---

# Custom App Development（基于 hx-shell）

## 第零步：调 getInfo() 发现 API（强制）

**写任何代码之前，先在应用 init 脚本中调用 `await window.hx.getInfo()`，检查返回的 `apis` 列表。**

- `getInfo()` 返回当前壳**实际可用**的 API 清单（方法名、参数、返回值）+ 自定义协议 + 不支持的 API
- **只使用 `getInfo()` 返回的 `apis` 列表中的方法**。不要从记忆里拿 API 名称，不要猜
- 如果某个你想要的功能不在列表中：
  1. 用 `hx.exec()` 调系统命令作为替代（如 `dir`/`tasklist`/`ipconfig`）
  2. 或者用纯前端 JS（`localStorage`/`fetch`/`Canvas`/`IndexedDB`）
  3. 或者告诉用户该功能在 hx-shell 中不支持
- `getInfo()` 是运行时真相源。本文档可能过时，但 `getInfo()` 永远准确

```js
// 应用启动时第一行代码
const info = await window.hx.getInfo();
if (!info.ok) throw new Error('hx-shell bridge not available');
console.log('Available APIs:', Object.keys(info.data.apis));
// 之后只用这些 API 写应用逻辑
```

## 运行时：hx-shell

- **HX-SHELL** = `wry 0.55.1` + WebView2，单文件 exe 约 **1MB**
- 打包产物 = `hx-shell.exe` 的 PE 资源节(RT_RCDATA)嵌入 HTML/JS payload（替代旧式 exe 尾部注入）
- 用户电脑**零工具链**：双击 exe 即跑，不依赖 Node/Rust/Node-Webkit 等任何运行时
- 窗口能力：WebView2 全套（Chromium 渲染、JS 现代语法、wasm）
- 绑定通道：**WebView2 原生桥** `window.chrome.webview.postMessage` → Rust 侧 `ipc_handler`（**无 CORS 限制**，比 `fetch` 到 custom scheme 可靠）

## 应用模型

```
HERMIX_HOME/custom-apps/<id>/
  manifest.json      ← 应用清单（id, name, window_config, permissions）
  index.html         ← 自包含 HTML（或 Vite 构建产物）
  src/               ← [可选] Vue/React 源码（Vite 构建后打包成单文件 index.html）
  dist/
    <id>.exe         ← 打包产物（hx-shell.exe + HTML/JS 作为 PE 资源嵌入）
    WebView2Loader.dll  ← wry 运行时依赖
    <id>.dev.html    ← 测试版（run action 用 hx-shell --html 直接加载，不落单 exe）
```

## manifest 权限声明（必填，否则 validate L2 硬卡）

`manifest.json` 里 **必须** 有 `permissions` 数组。validate 的 L2 关卡做"权限一致性"检查：
**只要你在 `permissions` 里声明了任意权限，HTML/TSX 中用到的每个 `hx.*` API 就必须有对应权限，
否则报 error 级（validate `ok=false`，无法交付）。** 没声明任何权限时只是 warning，但运行时壳不拦、
 validator 仍建议声明。

**有效权限值**：`fs-read` / `fs-write` / `exec` / `dialog` / `app` / `clipboard` / `net`

**`hx.*` API → 所需权限映射**（用了哪个就声明哪个）：

| hx API | 权限 |
|---|---|
| `hx.readFile` / `hx.readDir` / `hx.stat` | `fs-read` |
| `hx.writeFile` / `hx.appendFile` / `hx.removeFile` / `hx.createDir` / `hx.removeDir` / `hx.readFileBytes` / `hx.writeFileBytes` | `fs-write` |
| `hx.exec` | `exec` |
| `hx.openDialog` / `hx.saveDialog` / `hx.showMessage` | `dialog` |
| `hx.getInfo`（**第零步必调** → 必须声明 `app`） | `app` |
| 剪贴板相关（壳当前未实现，仅占位） | `clipboard` |

**最简安全写法**：不确定就全声明，至少包含 `app`（因为 `getInfo` 是强制第一步）：
```json
"permissions": ["fs-read", "fs-write", "exec", "dialog", "app"]
```

## 两种开发模式

| 模式 | 参数 | 产物 | 推荐 |
|---|---|---|---|
| **纯 HTML** | `index_html` | 自包含 HTML（内联 CSS+JS） | ✅ 简单工具 |
| **Vue/React/TS** | `app_source` (TSX) + `index_html` (可选壳) | Vite 构建成单文件 index.html | ✅ 复杂应用 |

### Vite 单文件输出配置（Vue/React 必须）

`vite.config.ts`：
```ts
export default {
  build: {
    rollupOptions: { output: { inlineDynamicImports: true } },
    assetsInlineLimit: 100_000_000,   // 所有 JS/CSS/图片内联进单个 index.html
  },
}
```

## 绑定 API（window.hx.*）

壳通过 WebView2 原生桥提供 RPC。所有方法返回 Promise，resolve 完整结果对象 `{ok, data?, err?}`。
调用方通过 `r.ok` 判断成功/失败，通过 `r.data` 取数据，通过 `r.err` 取错误信息。

### 文件系统

| 方法 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `hx.writeFile(path, content)` | string, string | `{ok}` | 写文件（UTF-8 文本） |
| `hx.readFile(path)` | string | `{ok, data}` | 读文件（**仅文本**，返回 UTF-8 字符串） |
| `hx.readFileBytes(path)` | string | `{ok, data}` | 读二进制 → **base64**（页面端 `atob()` 解码） |
| `hx.writeFileBytes(path, b64)` | string, string | `{ok}` | 二进制写入（base64 输入） |
| `hx.appendFile(path, content)` | string, string | `{ok}` | 追加写入 |
| `hx.removeFile(path)` | string | `{ok}` | 删除文件 |
| `hx.readDir(path)` | string | `{ok, data: [...]}` | 列出目录内容（返回文件名数组） |
| `hx.readDirDetailed(path)` | string | `{ok, data}` | 带元信息列表 `[{name,isDir,isFile,size,modified}]`，免去逐个 stat |
| `hx.stat(path)` | string | `{ok, data}` | 文件元信息（size/isDir/isFile/modified） |
| `hx.createDir(path)` | string | `{ok}` | 创建目录（递归） |
| `hx.removeDir(path)` | string | `{ok}` | 删除目录（递归） |

### 本地文件 URL（hxfile:// 协议）

| 方法 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `hx.fileUrl(path)` | string | string | 把本地路径转成 `hxfile://` URL，可直接用于 `<audio>`/`<img>`/`<video>` src |

```js
// 播放本地音乐
const url = window.hx.fileUrl('C:\\Music\\song.mp3');
// url = 'hxfile://localhost/C:/Music/song.mp3'
audioElement.src = url;
audioElement.play();
```

**重要**：`hx.readFile` 只能读**文本文件**。对于二进制文件（MP3/图片/PDF）：
- **首选** `hx.fileUrl(path)` 生成 URL 直接喂 `<audio>`/`<img>`/`<video>`（`hxfile://` 协议 Rust 端读磁盘，支持 seek，不占 IPC 内存）
- **需要拿到字节**时用 `hx.readFileBytes(path)` → base64 → `atob()` 转 Uint8Array

### 进程/系统

| 方法 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `hx.exec(cmd, args)` | string, string[] | `{ok, data}` | 执行外部命令，返回 `{stdout, stderr, exitCode}` |
| `hx.getInfo()` | — | `{ok, data}` | 壳信息（shell/engine/version/single_file/完整 API 清单） |
| `hx.showMessage(msg)` | string | `{ok, data}` | 弹原生消息框（需 `dialog` 权限） |

### 原生对话框（V6 壳新增）

| 方法 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `hx.openDialog({title?, directory?, multi?, filters?})` | object | `{ok, data}` | 原生文件/目录选择，**免 PowerShell 绕** |
| `hx.saveDialog({title?, defaultName?, filters?})` | object | `{ok, data}` | 原生保存对话框 |

```js
// 选目录（音乐播放器用这个）
const r = await window.hx.openDialog({ title: "选择音乐文件夹", directory: true });
if (r.ok && r.data) console.log(r.data); // 'C:\\Music'

// 选文件（带过滤器）
const f = await window.hx.openDialog({
  title: "选图片",
  filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg"] }],
});

// 多选文件
const fs = await window.hx.openDialog({ multi: true });
// fs.data = ['C:\\a.txt', 'C:\\b.txt']

// 保存
const s = await window.hx.saveDialog({ title: "保存为", defaultName: "report.json" });
```

### 窗口控制（V7 壳 — 无装饰窗口必备）

**V7 壳起窗口无原生标题栏**（`decorations: false`）。应用必须自行实现标题栏，包括：
- 拖动区域（鼠标按下标题栏空白处可移动窗口）
- 窗口控制按钮（最小化 / 最大化 / 关闭）

| 方法 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `hx.win.minimize()` | — | `{ok}` | 最小化窗口 |
| `hx.win.toggleMaximize()` | — | `{ok, data: {maximized: bool}}` | 切换最大化/还原 |
| `hx.win.close()` | — | never | 关闭窗口（直接退出进程） |
| `hx.win.setTitle(title)` | string | `{ok}` | 动态设置窗口标题 |
| `hx.win.startDrag()` | — | `{ok, err?}` | 鼠标按下标题栏时调用，启动窗口拖动 |
| `hx.win.isMaximized()` | — | `{ok, data: bool}` | 检查窗口是否最大化 |

### 自定义标题栏模板（必须包含在每个应用中）

每个自定义应用的 HTML **顶部必须包含标题栏**。以下是标准标题栏 snippet，直接粘贴到 `<body>` 最前面：

```html
<!-- 自定义标题栏 — 无装饰窗口必备 -->
<div class="hx-titlebar" onmousedown="window.hx.win.startDrag()">
  <span class="hx-titlebar__title" id="hxTitle">应用名称</span>
  <div class="hx-titlebar__controls">
    <button class="hx-titlebar__btn" onclick="window.hx.win.minimize()" title="最小化">
      <svg width="12" height="12" viewBox="0 0 12 12"><rect x="2" y="6" width="8" height="1" fill="currentColor"/></svg>
    </button>
    <button class="hx-titlebar__btn" onclick="window.hx.win.toggleMaximize()" title="最大化/还原">
      <svg width="12" height="12" viewBox="0 0 12 12"><rect x="2.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/></svg>
    </button>
    <button class="hx-titlebar__btn hx-titlebar__btn--close" onclick="window.hx.win.close()" title="关闭">
      <svg width="12" height="12" viewBox="0 0 12 12"><path d="M3 3L9 9M9 3L3 9" stroke="currentColor" stroke-width="1.2"/></svg>
    </button>
  </div>
</div>
```

标题栏 CSS（浅色主题）：
```css
.hx-titlebar {
  display: flex; align-items: center; justify-content: space-between;
  height: 32px; padding: 0 0 0 12px;
  background: #f0f0f0; border-bottom: 1px solid #e4e4e7;
  user-select: none; -webkit-user-select: none;
}
.hx-titlebar__title { font-size: 12px; color: #71717a; }
.hx-titlebar__controls { display: flex; height: 100%; }
.hx-titlebar__btn {
  width: 46px; height: 100%; border: none; background: transparent;
  color: #52525b; cursor: pointer; display: flex; align-items: center; justify-content: center;
}
.hx-titlebar__btn:hover { background: #e4e4e7; }
.hx-titlebar__btn--close:hover { background: #c62828; color: #fff; }
```

**深色主题标题栏**（仅当应用使用深色主题时）：
```css
.hx-titlebar { background: #1c1c20; border-bottom: 1px solid #2e2e34; }
.hx-titlebar__title { color: #a0a0a8; }
.hx-titlebar__btn { color: #d0d0dc; }
.hx-titlebar__btn:hover { background: #2e2e34; }
.hx-titlebar__btn--close:hover { background: #c62828; color: #fff; }
```

**交互要点**：
- `onmousedown="window.hx.win.startDrag()"` — 鼠标按下标题栏空白区时启动窗口拖动
- 按钮的 `onclick` 必须在 mousedown 时不触发 startDrag（按钮在 controls 容器内，mousedown 事件冒泡到标题栏会同时触发 startDrag）
- **解决方案**：在 controls 容器上加 `onmousedown="event.stopPropagation()"` 阻止冒泡

修正版：
```html
<div class="hx-titlebar" onmousedown="window.hx.win.startDrag()">
  <span class="hx-titlebar__title">应用名称</span>
  <div class="hx-titlebar__controls" onmousedown="event.stopPropagation()">
    <!-- 按钮 -->
  </div>
</div>
```

manifest 的 `icon` 字段 = **Lucide 图标库名**（如 `"music"` / `"settings"` / `"folder"`），
打包器从内置图标库（2007 个 SVG）读对应图标注入 payload，壳用 resvg 渲染成窗口图标。
**不要传 base64 大图**（>50KB 会解码失败）。

```json
"manifest": {
  "id": "music-player",
  "name": "本地音乐",
  "icon": "music",
  "permissions": ["fs-read", "exec", "dialog"]
}
```

常用图标名：`music` / `play` / `pause` / `skip-forward` / `folder` / `file-text` /
`settings` / `search` / `download` / `upload` / `trash` / `edit` / `plus` / `x` /
`check` / `alert-circle` / `image` / `video` / `mic` / `headphones` / `monitor` /
`printer` / `camera` / `book-open` / `message-square` / `send` / `bell` / `star` / `heart`
（完整 2007 个清单见 `apps/desktop-tauri/src-tauri/resources/icons/`，文件名去掉 `.svg` 即名字）

```js
// 文本读写
await window.hx.writeFile('data.txt', 'hello');
const r = await window.hx.readFile('data.txt');
if (r.ok) console.log(r.data); // "hello"

// 扫描文件夹（readDirDetailed 直接带大小/类型）
const r2 = await window.hx.readDirDetailed('C:\\Music');
if (r2.ok) console.log(r2.data);
// [{name:'song1.mp3', isFile:true, size:5242880}, ...]

// 播放音频
audioElement.src = window.hx.fileUrl('C:\\Music\\song1.mp3');
```

**实现原理**（了解即可）：
- `hx.writeFile(p,c)` → `window.chrome.webview.postMessage(JSON.stringify({id, m:"writeFile", a:[p,c]}))`
- Rust 端 ipc_handler 收到 → 处理 → `evaluate_script("window['__hx_resolve_<id>'](result)")`
- JS 端 `window['__hx_resolve_<id>']` 是 Promise resolver → 返回结果

**别用 fetch 到 `https://app.localhost/hx/...`**（custom scheme 的 fetch 被 Chromium CORS 拒绝）—— 必须用 `window.chrome.webview.postMessage`！

### 媒体/二进制文件访问

- **音频播放**：`hx.readDir(dir)` 扫描文件夹 → `hx.fileUrl(path)` 生成 URL → `<audio src=url>`
- **图片显示**：`hx.fileUrl(path)` → `<img src=url>`
- **视频播放**：`hx.fileUrl(path)` → `<video src=url>`
- `hxfile://` 协议支持 seek（拖动进度条），不占 IPC 内存

### 系统能力不足？用纯前端 JS

- 数据：`localStorage` / `IndexedDB` / **SQLite-wasm**（本地数据库）
- 网络：`fetch()`（到 https URL）
- 媒体：Canvas / WebGL / `<video>` / `<audio>` + `hxfile://` 协议
- 算力：**wasm 库**（ffmpeg-wasm、pdf-wasm、jszip）

## 设计阶段（强制）：先出配色与布局方案，经用户确认再开发

> **这是强制前置关卡，不走过场。** 当前实测痛点是"整体架构尚可、细节粗糙、组件位置 / 边距 / 屏幕适应无统一标准、主体色系千篇一律（所有应用套同一个墨绿）"。
> 根因是 LLM 直接上手写代码、没先做设计确认。本阶段要求：**先设计、再确认、后开发**。

### 1. 先界定应用功能定位（决定配色气质）

动手前先用 1 句话明确该应用的功能类型，按功能气质选配色方向，**禁止所有应用套同一个墨绿 #4b7a5e**：

| 功能类型 | 配色气质建议（仅方向，具体色值在设计稿定） |
|---|---|
| 媒体 / 音乐 / 影音 | 暖调、沉静（暖灰 + 琥珀 / 陶土点缀） |
| 笔记 / 文档 / 写作 | 纸感、克制（米白 + 墨色 + 低饱和点缀） |
| 工具 / 转换 / 批处理 | 中性、专业（石墨灰 + 单一功能色） |
| 数据 / 统计 / 看板 | 冷静、清晰（冷灰 + 低饱和青绿，仍禁高饱和蓝紫） |
| 系统 / 监控 / 运维 | 深色优先、科技克制（近黑 + 单色高亮） |
| 教育 / 儿童 / 娱乐 | 可活泼，但仍是低饱和、有节制的双色搭配 |

### 2. 自主设计明暗两套配色（按功能区分，避免千篇一律）

为每个应用**独立设计**一套配色 token（不要全局复用墨绿）：
- **必须同时交付「浅色」与「深色」两套**（用 CSS 变量 `--bg --panel --text --muted --accent --border --danger` 等组织），并提供主题切换入口。
- 每套至少给出 7 个 token：页面背景、面板背景、主文字、次级文字、边框、强调色、危险 / 成功色。
- **硬约束（不可违反）**：中性灰家族优先；**禁止高饱和蓝 / 紫 / 靛**（blue/indigo/violet/purple）；浅色默认；文本对比度 ≥ 4.5:1；强调色按钮用白字。
- 强调色仅用于"主按钮 / 选中态"一个点缀，**大面积仍是中性灰**，不要整屏染色。

### 3. 布局与间距基线（统一标准，消灭随意性）

设计稿必须明确以下基线（所有自定义应用共用，避免各自为政）：
- 内容最大宽度 ~880px 居中；页面边距 20-24px；卡片间距 12-16px。
- 控件尺寸：按钮 ≥32px 高、点击目标 ≥36px；输入框与按钮对齐、同高。
- 圆角：卡片 10px / 按钮 8px / 输入框 8px（统一）。
- 字号：标题 18-20px、正文 14px、辅助 12px；行高 1.6。
- 信息层级：标题区 → 工具栏 / 操作区 → 内容区 → 状态 / 输出区（四级分区）。
- 屏幕适应：窗口可缩放时内容区 `overflow-y:auto`；不压缩堆叠、不贴边；高分屏 / 缩放下不错位。

### 4. 产出设计稿，征求用户同意（关键闸门）

把 1-3 落成一份**简短设计稿**（文字即可，列出：功能定位一句话、浅色 token 表、深色 token 表、布局基线、主题切换方式），**先发给用户确认，得到明确同意后再写任何代码**：
- 用户同意 → 进入开发（token 直接落到 CSS 变量，全程不偏离）。
- 用户要求调整 → 改设计稿再确认，不带着未确认的设计写代码。
- **严禁跳过本阶段**：不允许"边写边改""先写完再问"。

## 技术选型与复用检索（强制）：禁止重复造轮子

> 自定义应用虽是小工具，但"从零手搓一个已有成熟方案的功能"既浪费又易出错。开发前必须先检索复用。

### 1. 先界定要复用的能力点

列出应用里"非业务逻辑、业界已有成熟实现"的能力（如 Markdown 渲染、代码高亮、图表、PDF 预览、富文本编辑、diff、二维码、加密、日期选择等）。这些**优先找现成库 / 组件嵌入**，不要自己造。

### 2. 检索顺序（GitHub 优先，国内平台兜底）

1. **优先 GitHub**：搜 `topic:` / 关键词 + `stars:>xxx`，找活跃、License 宽松（MIT / Apache-2.0 / BSD）的项目。
2. **GitHub 不可达时（超时 / 403 / 网络错误）→ 立刻切国内开源平台**，不要反复重试 GitHub 浪费轮次：
   - **Gitee（码云）** `gitee.com`：国内最大开源托管，优先搜。
   - **GitCode** `gitcode.com`、`OSChina`、`AtomGit` 等作为补充。
   - 也可用 **npm 国内镜像**（`registry.npmmirror.com`）拉包后在 Vite 里打包内联（仍是"构建期内联"，不是运行时 CDN 外链）。
3. 检索结论要记录：用了哪个项目、版本、License、嵌入方式（Vite 打包 / 内联单文件）。

### 3. 复用约束

- 选库必须满足"可离线打包内联"（禁止任何运行时 CDN 外链；见前端默认标准第 5 条）。
- License 合规：MIT / Apache-2.0 / BSD 优先；GPL 类谨慎（小工具分发需注意）。
- 若检索后确认无合适现成方案，才允许自研，并在交付说明里写明"已检索，无合适复用，故自研"。

## 前端默认标准（必须遵守，违反视为不合格）

> **这是硬性标准**。LLM 生成的任何自定义应用界面，默认必须符合以下规范；
> 只有用户**明确要求**深色/特殊风格时才允许偏离。
> **配色与布局必须先走上方「设计阶段」章节**——为每个应用按功能独立设计明暗两套 token；下方色板仅为**未做功能化配色时的默认回退**，不要把墨绿当成所有应用的统一色。

### 1. 默认浅色主题（不要上来就写深色）

- **默认浅色**：白/浅灰底 + 深灰文字。**禁止默认深色**（深色仅作可选主题，且要提供切换入口）
- 配色用**中性灰家族**，禁止蓝灰（slate）和任何饱和蓝/紫/靛（blue/indigo/violet/purple）
- 浅色基准色板：

| 用途 | 色值 |
|---|---|
| 页面背景 | `#fafafa` |
| 卡片/面板背景 | `#ffffff` |
| 次级背景（输入框/悬浮） | `#f4f4f5` |
| 边框/分隔线 | `#e4e4e7` |
| 主文字 | `#18181b` |
| 次级文字 | `#71717a` |
| 强调色（**回退默认**，正式应用应走「设计阶段」产出功能化强调色） | `#4b7a5e`（墨绿，仅作未做功能化配色时的回退，禁止所有应用统一用它） |
| 成功/危险（如需要） | `#2e7d32` / `#c62828`（低饱和） |

- 文本对比度 ≥ 4.5:1；强调色按钮上用白字
- `prefers-color-scheme: dark` 可作为**可选**增强，但默认仍是浅色

### 2. 简约大气，布局合理

- **信息层级清晰**：标题区（应用名+简述）→ 工具栏/操作区 → 内容区 → 状态/输出区
- 卡片式布局：功能块用卡片分组，卡片间 12-16px 间距
- 最大内容宽度 ~880px，居中；边距 20-24px
- 控件尺寸：按钮 ≥32px 高、点击目标 ≥36px；输入框与按钮对齐
- 圆角：卡片 10px、按钮 8px、输入框 8px；统一
- 留白充足，不要贴边堆叠；单屏放不下用滚动，不要压缩
- 标题字号 18-20px；正文 14px；辅助信息 12px；行高 1.6

### 3. 该有的图标不能少（但禁止 emoji）

- **操作按钮必须有图标**：保存/打开/删除/刷新/导出/设置等主操作，图标是刚需
- 图标用 **内联 SVG**（lucide 风格，24×24 viewBox，`stroke="currentColor"`，`stroke-width="2"`，无填充），**禁止外链图标库**（离线不可用）、**禁止 emoji 当图标**
- 常用图标 SVG path（lucide，直接内嵌）：
  - 保存 `M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z` + `M17 21v-8H7v8` + `M7 3v5h8`
  - 文件夹打开 `M6 2h6l2 4h6a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z`
  - 刷新 `M21 12a9 9 0 1 1-2.64-6.36` + `M21 3v6h-6`
  - 删除 `M3 6h18` + `M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2` + `M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6`
  - 下载 `M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4` + `M7 10l5 5 5-5` + `M12 15V3`
  - 设置 `M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z` + 齿轮外圈
  - 运行/播放 `M5 3l14 9-14 9V3z`
  - 信息 `M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z` + `M12 16v-4` + `M12 8h.01`
- 图标规格：16-18px，与文字 6px 间距（`display:inline-flex;align-items:center;gap:6px`）
- 状态区可用小圆点/色块表示（如运行中=绿点），也禁止用 emoji

### 4. 文本与语言

- **界面文案用简体中文**（除非用户指定英文）；术语保持英文（如"Export"写成"导出"）
- **禁止 emoji**（包括标题、按钮、状态、装饰）——这是硬性要求
- 文案简洁、动词开头（"保存文件"不是"文件保存操作"）；错误提示要具体（"无法读取 data.txt：文件不存在"）

### 5. 交互细节

- 加载/处理中要有反馈：按钮禁用+"处理中…"文字，或顶部细进度条；禁止无反馈干等
- 操作成功要有轻提示（toast/状态文字变绿），失败要红字+具体原因
- 表单校验即时反馈（空值/格式错误），回车可提交
- 所有外部资源**禁止 CDN**（unpkg/jsdelivr 等）：Vue/React 库要么 Vite 打包内联，要么内嵌进单文件 HTML；字体用系统字体栈 `system-ui, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif`

### 6. 自检清单（交付前过一遍）

- [ ] **设计阶段已过闸门**：明暗两套配色 token 已按功能设计、用户已确认（见「设计阶段」章节）
- [ ] **复用检索已做**：能力点已先检索 OSS，结论已记录；无合适才自研（见「技术选型与复用检索」章节）
- [ ] 浅色默认、中性灰配色、无蓝紫（墨绿仅作未设计时的回退）
- [ ] 主操作按钮都有 SVG 图标
- [ ] 全文无 emoji
- [ ] 无 CDN 外链（Vue/React/图标全部内联）
- [ ] 布局有层级：标题/工具栏/内容/状态分区
- [ ] 按钮/输入框尺寸一致、间距统一
- [ ] 失败场景有具体错误提示
- [ ] **代码审查闸门已过**（见「代码审查与验证机制」章节）：无低级错误、提示友好、validate ok、smoke 无运行时报错

## 已有自定义应用定位流程（强制）

只要用户的话涉及"我已有的某个应用/工具/小软件"、"修改/更新/删除/打包/运行我的 XXX"、"帮我看看已安装的应用"、"应用装在哪"、**"我的 XXX 应用报错/打不开/加载失败 (os error 3)/崩溃"**——**第一步必须调 `custom_app_manager`**，**严禁**自己拼 hermix home 路径去 `ls`/`read_file`/`read_dir`。

**执行顺序（不允许跳）**：
1. **`custom_app_manager.list`**：拿到所有已安装应用的 `id + name + description + category`。如果用户没给精确名字（"我那个播放器""之前的音乐 app"），把候选列出来让用户确认；禁止空泛反问"具体是哪个"。
2. **匹配到唯一应用** → **`custom_app_manager.get({id})`**：读取完整 manifest + source。要修改就 `update`、要运行就 `run`、要打包就 `package`、要删除就 `delete`。
3. **工具返回空列表** → 告诉用户"当前没有已安装的自定义应用"，不要再去猜 `C:/Users/*/hermix/custom-apps` 之类的路径。
4. **工具调用失败** → 报告错误，不要连猜多个路径重试。

**反例（严禁）**：截图里连续 `ls C:/Users/Public/hermix/custom-apps`、`C:/Users/qcc/hermix/custom-apps`、`C:/hermix/custom-apps` 全错——HERMIX_HOME 只能在运行时由工具解析，硬猜路径必然失败。

## agent 开发流（一次性到位交付）

**核心铁律**：用户说"做个/开发一个/写个 工具/应用/软件"时，**直接判定为自定义应用**，不要反问、不要调其他通用代码工具。按下面步骤走完，**代码一次性到位、前端美观、功能完整、无任何报错**才交付。

1. **识别 + 分析前后端最优方案**：确认是自定义应用后，先想清楚——
   - 数据存哪（localStorage / `hx.writeFile` 落盘 / `hx.exec` 调系统命令）
   - 用哪些 `hx.*` API（先 `await window.hx.getInfo()` 取可用清单）
   - 功能类型与气质（音乐 / 笔记 / 工具 / 数据 / 系统 / 娱乐，决定配色方向）
2. **设计阶段（强制闸门，见上方「设计阶段」章节）**：按功能定位自主设计**明暗两套配色 token + 布局基线**，产出简短设计稿发用户确认。**未获明确同意，禁止进入下一步写代码**。用户要求调整则改稿再确认。
3. **技术选型与复用检索（强制，见上方「技术选型与复用检索」章节）**：先界定要复用的能力点，GitHub 优先检索成熟库；GitHub 不可达立刻切 Gitee / GitCode 等国内平台。记录"用了什么 / 版本 / License / 嵌入方式"；确认无合适复用才自研。
4. 写 `manifest.json`：
   ```json
   {"id": "my-app", "name": "我的应用", "version": "1.0.0", "permissions": ["fs-read", "fs-write", "exec", "dialog", "app"], "long_description": "功能介绍...\n使用方法...\n更新方法...\n分发方法...", "window_config": {"width": 900, "height": 600, "resizable": true}}
   ```
   **`long_description` 必填**：含功能介绍、**使用方法、更新方法（回来用 update action 改代码）、分发方法（package 出单 exe）**，在详情界面"说明"Tab 展示。
5. 写代码（`index_html` 自包含 或 `app_source` TSX + `source_files?`）——**严格按「设计阶段」产出的 token、「前端默认标准」与「自检清单」生成界面**，禁 CDN / 禁 emoji / 禁 input-file / 必带标题栏 / 必含主题切换入口。
6. **先 `custom_app_manager.list` 查重**：若已存在同类应用（按名称/描述/功能判断，如"本地音乐"和"音乐播放器"是同一个东西）→ 直接 `update` 它迭代；否则 `custom_app_manager.create({manifest, index_html, app_source?, source_files?})`。
7. **`validate` 循环到 ok=true**（L0 manifest + L1 HTML/JS + L2 权限 + L3 前端质量）。每轮失败按 issues 的 message 修复，再 validate，直到 `ok=true`。**不得带着 error 交付**。
8. **`run` 测试版即时预览**：`run` 用 dev 模式（`hx-shell --html` 加载 `dist/<id>.dev.html`），不落单 exe，即时看到效果。如需确认运行时无报错，调 `smoke`（先 `package` 再启动 exe 抓 stderr）。
9. **代码审查与验证闸门（强制，见下方「代码审查与验证机制」章节）**：按 `code-review.md` 的审查清单过一遍；核对「前端默认标准 / 自检清单」全部命中；跑 `smoke` 确认无运行时报错与控制台错误；确认所有提示文案友好、错误提示具体。**任一项不过，不交付**。
10. **交付说明（必须告诉用户）**：
   - **去哪运行**：开发期用 `run` action 启动测试版（hx-shell 内部加载 `dist/<id>.dev.html`，无需手动碰文件）；正式交付用 `package` 出 `dist/<id>.exe` 双击即跑。**严禁让用户用浏览器直接打开 `.html` 文件**——那样 `window.hx.*` 全部不可用，应用必报错。
   - **如何打包**：调 `package` → 生成 `dist/<id>.exe` + `WebView2Loader.dll`，双击即跑（WebView2 系统自带），可发给别人。
   - **如何回来继续更新**：直接调 `update` action 改 `app_source`/`index_html`，再 `run` 即时预览；或桌面端"编辑"按钮回到本 skill 继续迭代。

**禁止**：带着 validate error 交付 / 用 input-file 让用户选文件（改用 `hx.openDialog`）/ 引 CDN / 留 emoji / 缺标题栏 / **跳过设计确认直接写代码** / **未做复用检索就自研** / **未经代码审查闸门交付**。

## 代码审查与验证机制（强制闸门）

> 小工具也要规范开发。**本闸门是交付前最后一道关**，必须逐项过，全部通过才允许交付。审查方法论详见同目录 `code-review.md`，前端质量基线详见 `frontend-architecture.md`。

### 1. 审查清单（对照 `code-review.md`）

- [ ] **无低级错误**：无未定义变量、无悬挂事件、无 `ReferenceError`；`window.hx.*` 调用都做了 `r.ok` 判断；异步都 `await`。
- [ ] **无死代码 / 无废弃分支**：不留注释掉的整段代码、不留 `console.log` 调试残留（必要的运行时日志除外）。
- [ ] **权限声明一致**：`manifest.permissions` 覆盖 HTML/TSX 中所有用到的 `hx.*` API（validate L2 已卡，这里复核）。
- [ ] **离线可用**：无任何运行时 CDN / 外链；Vue/React/图标全部内联或 Vite 打包内联。
- [ ] **设计稿落实**：实际配色 token 与设计阶段产出一致；明暗两套都在、主题可切换；未偏离用户确认稿。

### 2. 交互友好性（强制）

- [ ] **提示文案友好**：加载 / 处理中要有反馈（按钮禁用 + "处理中…" 或顶部细进度条），禁止无反馈干等。
- [ ] **错误提示具体**：失败必须红字 + 具体原因（如"无法读取 data.txt：文件不存在"），禁止空泛的"出错了 / error"。
- [ ] **成功有轻提示**：toast / 状态文字变绿，让用户知道成了。
- [ ] **表单校验即时**：空值 / 格式错误即时反馈，回车可提交。

### 3. 验证动作（强制）

- [ ] **`validate` 已 ok=true**（L0-L3 全过），无遗留 error。
- [ ] **`run` 测试版**人工看一遍：布局在默认窗口尺寸下无错位、无贴边、控件对齐。
- [ ] **`smoke` 跑一遍**（先 `package` 再启动 exe 抓 stderr）：确认无运行时崩溃、无控制台报错；若有 `hx.*` 调用异常能在界面以红字呈现而非静默失败。
- [ ] **缩放 / 高分屏抽查**：拖动窗口缩放、缩放到 125%/150% 下不出现错位或溢出。

## 关键约束

- **【致命】技术栈唯一，禁止再问用户选型**：自定义应用运行时就是 **hx-shell（wry + WebView2）**，不存在"要不要用 Tauri / Electron / Neutralinojs / 终端 TUI"这种问题。
  - 用户说"做个应用/工具/软件" → 直接按 hx-shell 实现，不要反问技术形态。
  - 如果某个功能 hx-shell 做不到（如需要系统级 hook、内核驱动、独立 Rust 后台进程），明确告诉用户"该功能超出 hx-shell 能力范围"，而不是换壳。
  - **hx-shell 不能"加载"Tauri 应用**：Tauri 需要独立编译的 Rust 二进制和框架运行时，hx-shell 只能跑 HTML/JS payload。两者是不同壳，不能混用。

- **【致命】创建前必须 `list` 查重，禁止重复建同类应用**：每次要 `create` 之前，先 `custom_app_manager.list` 看是否已有功能重叠的应用（按名称/描述判断，如"本地音乐"和"音乐播放器"是同一个东西）。若已存在 → 用 `update` 迭代它，**绝不再 `create` 第二个**。否则会出现"两个音乐播放器"这类重复应用，用户要手动清理。
- **【致命】安装目录由工具解析，禁止手动拼路径写应用文件**：自定义应用由 `custom_app_manager` 的 `create`/`update` action 写入，落盘位置是运行时 `hermix_home()/custom-apps/<id>/`（用户的**数据目录**，与 Hermix 装在哪、源码在哪都无关——换电脑/换安装路径不变）。agent **绝不**自己 `write_file` 拼路径去写应用文件，**更绝不**写到 Hermix 安装目录或项目源码树（`apps/`、`src/`、仓库任意位置）——那是给开发者改框架用的，不是放用户自定义应用的地方。要改应用代码，永远走 `update` action。

- **【致命】任何查看/读取自定义应用的操作都必须先调 `custom_app_manager`，严禁自己拼 `hermix_home` 路径**：
  - 想知道"已安装哪些自定义应用"、"某个应用是否存在"、"应用装在哪个目录" → **只能**先调 `custom_app_manager.list`；要读单个应用详情 → **只能**调 `custom_app_manager.get({id})`。
  - **绝对禁止**自己拼以下任何路径去 `ls`/`read_dir`/`read_file`：`C:/Users/*/hermix/custom-apps`、`C:/Users/Public/hermix/custom-apps`、`C:/hermix/custom-apps`、`~/hermix/custom-apps`、`%USERPROFILE%/hermix/custom-apps`、`<repo>/apps/<任何>`。HERMIX_HOME 由 Hermix 运行时根据当前用户/安装位置解析，**不同机器路径不同**，任何硬猜都必然出错（如截图里连续三个路径全错）。
  - 如果 `custom_app_manager.list` 返回空列表 → 告诉用户"当前没有已安装的自定义应用"；如果工具调用失败 → 报告工具错误，**不要**连猜多个路径试。那种"查看目录 Tool execution failed"连续多次就是典型错误。

- **【致命】窗口无原生标题栏（V7 壳）**：窗口 `decorations: false`，**必须**在 HTML 顶部自行实现标题栏（拖动区 + 最小化/最大化/关闭按钮）。不写标题栏 = 用户无法移动/关闭窗口（只能 Alt+F4）。用 `window.hx.win.*` API 控制窗口。标题栏模板见上方"自定义标题栏模板"章节。
- **整个应用就是一个 `index.html`**：没有 backend.ts / ui.tsx 分离文件！所有 HTML/CSS/JS 都内联在单个 index.html 里。TSX 模式由 esbuild 编译后 inline。
- **不要 import React**：`window.React` 是全局变量（从 CDN 或单文件 HTML 内嵌），用 `const { useState } = React;`
- **不要 `fetch('app://...')`**：CORS 拒绝。用 `window.hx.*`（内部走 chrome.webview.postMessage）
- **不要依赖 Node.js API**（path/fs/child_process）：沙箱里没有，用 `window.hx.*` 替代
- **不要用 Tauri API**（`convertFileSrc`/`shell_dialog_open`/`shell_fs_read_bytes`）：这些在 hx-shell 里**不存在**！只有 `window.hx.*` 这一套 API
- **二进制文件用 `hx.fileUrl`**：`readFile` 只读文本，MP3/图片/视频用 `hx.fileUrl(path)` 生成 URL
- **不要用 sourcemap 单独文件**：Vite 单文件输出不要 `.map`（影响单文件化）
- **HTML 文件编码**：UTF-8，无 BOM
- **不要 CDN**：Vue/React/图标全部内联（离线可用）
- **不要 emoji**：图标用内联 SVG（manifest.json 里也不要用 emoji 当 icon）

## 关键约束（桌面定位）

- **【致命】禁止用浏览器 input/file API 选文件/目录**：`<input type="file">` / `<input type="file" webkitdirectory multiple>` /
  `FileReader` / `<input>` 手输路径 —— 这些会触发 WebView2 原生"上传文件?"确认弹窗（蓝色"上传/取消"按钮），
  **跟桌面小工具的定位完全不搭**。桌面应用必须用壳层 rfd 原生对话框：
  ```js
  // ✅ 正确：调壳层 API，弹 Windows 原生选择框（无确认弹窗）
  const r = await window.hx.openDialog({ title: "选音乐文件夹", directory: true });
  if (r.ok && r.data) console.log(r.data);

  // ❌ 错误：浏览器 input 触发"上传"确认弹窗
  <input type="file" webkitdirectory multiple onchange="..." />
  ```
  - 选目录 → `openDialog({directory: true})`
  - 选文件 → `openDialog({filters: [{name:"图片", extensions:["png","jpg"]}]})`
  - 多选 → `openDialog({multi: true, ...})`
  - 保存 → `saveDialog({defaultName: "report.txt"})`
- **【致命】不要用纯文本 `<input>` 让用户手输路径**：粘贴/转义易错，体验差。**永远用按钮调 `openDialog` 弹原生框**。
- **默认浅色**：不要上来就写深色
- **manifest.json 禁止 emoji**：icon 字段不要用 emoji，留空或用文字

## 模板：纯 HTML 工具（浅色 + SVG 图标）

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>文本工具</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font: 14px/1.6 system-ui, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
         background: #fafafa; color: #18181b; display: flex; flex-direction: column; height: 100vh; }
  /* ── 自定义标题栏（V7 壳无原生标题栏，必须自行实现）── */
  .hx-titlebar { display: flex; align-items: center; justify-content: space-between;
    height: 32px; padding: 0 0 0 12px; background: #f0f0f0; border-bottom: 1px solid #e4e4e7;
    user-select: none; -webkit-user-select: none; flex-shrink: 0; }
  .hx-titlebar__title { font-size: 12px; color: #71717a; }
  .hx-titlebar__controls { display: flex; height: 100%; }
  .hx-titlebar__btn { width: 46px; height: 100%; border: none; background: transparent;
    color: #52525b; cursor: pointer; display: flex; align-items: center; justify-content: center; }
  .hx-titlebar__btn:hover { background: #e4e4e7; }
  .hx-titlebar__btn--close:hover { background: #c62828; color: #fff; }
  /* ── 内容区 ── */
  .content { padding: 24px; flex: 1; overflow-y: auto; }
  .wrap { max-width: 880px; margin: 0 auto; }
  h1 { font-size: 20px; margin-bottom: 4px; }
  .sub { color: #71717a; font-size: 12px; margin-bottom: 20px; }
  .card { background: #fff; border: 1px solid #e4e4e7; border-radius: 10px; padding: 16px; margin-bottom: 16px; }
  .card h2 { font-size: 14px; color: #71717a; font-weight: 500; margin-bottom: 12px; }
  textarea { width: 100%; height: 160px; background: #f4f4f5; border: 1px solid #e4e4e7;
             border-radius: 8px; padding: 10px; resize: vertical; font: 13px/1.5 Consolas, monospace; }
  .row { display: flex; gap: 8px; margin-top: 10px; flex-wrap: wrap; }
  button { display: inline-flex; align-items: center; gap: 6px; height: 36px; padding: 0 14px;
           background: #f4f4f5; color: #18181b; border: 1px solid #e4e4e7; border-radius: 8px;
           cursor: pointer; font-size: 13px; }
  button:hover { background: #e4e4e7; }
  button.primary { background: #4b7a5e; color: #fff; border: none; font-weight: 500; }
  button.primary:hover { background: #3d654c; }
  button svg { width: 16px; height: 16px; }
  .out { margin-top: 12px; background: #f4f4f5; border-radius: 8px; padding: 10px;
         font: 12px/1.5 Consolas, monospace; color: #2e7d32; white-space: pre-wrap; min-height: 24px; }
  .out.error { color: #c62828; }
</style>
</head>
<body>
<!-- 自定义标题栏 — onmousedown 启动窗口拖动，controls 阻止冒泡 -->
<div class="hx-titlebar" onmousedown="window.hx.win.startDrag()">
  <span class="hx-titlebar__title">文本工具</span>
  <div class="hx-titlebar__controls" onmousedown="event.stopPropagation()">
    <button class="hx-titlebar__btn" onclick="window.hx.win.minimize()" title="最小化"><svg width="12" height="12" viewBox="0 0 12 12"><rect x="2" y="6" width="8" height="1" fill="currentColor"/></svg></button>
    <button class="hx-titlebar__btn" onclick="window.hx.win.toggleMaximize()" title="最大化/还原"><svg width="12" height="12" viewBox="0 0 12 12"><rect x="2.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/></svg></button>
    <button class="hx-titlebar__btn hx-titlebar__btn--close" onclick="window.hx.win.close()" title="关闭"><svg width="12" height="12" viewBox="0 0 12 12"><path d="M3 3L9 9M9 3L3 9" stroke="currentColor" stroke-width="1.2"/></svg></button>
  </div>
</div>
<div class="content"><div class="wrap">
  <h1>文本工具</h1>
  <p class="sub">本地文本编辑与保存</p>
  <div class="card">
    <h2>内容</h2>
    <textarea id="in" placeholder="在这里输入文本…"></textarea>
    <div class="row">
      <button class="primary" id="save">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><path d="M17 21v-8H7v8"/><path d="M7 3v5h8"/></svg>
        保存文件
      </button>
      <button id="read">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2h6l2 4h6a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z"/></svg>
        读取文件
      </button>
      <button id="info">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>
        壳信息
      </button>
    </div>
    <div class="out" id="out">—</div>
  </div>
</div></div> <!-- /wrap /content -->
<script>
const $ = id => document.getElementById(id);
const out = (s, err) => { $('out').textContent = s; $('out').classList.toggle('error', !!err); };
$('save').onclick = async () => {
  const r = await window.hx.writeFile('output.txt', $('in').value);
  out(r.ok ? '已保存到 output.txt' : '保存失败: ' + r.err, !r.ok);
};
$('read').onclick = async () => {
  const r = await window.hx.readFile('output.txt');
  out(r.ok ? r.data : '读取失败: ' + r.err, !r.ok);
};
$('info').onclick = async () => {
  const r = await window.hx.getInfo();
  out(JSON.stringify(r, null, 2));
};
</script>
</body>
</html>
```

## 模板：本地音乐播放器（readDir + fileUrl + audio）

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>音乐播放器</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font: 14px/1.6 system-ui, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
         background: #fafafa; color: #18181b; display: flex; flex-direction: column; height: 100vh; }
  .toolbar { display: flex; align-items: center; gap: 8px; padding: 12px 20px; background: #fff;
             border-bottom: 1px solid #e4e4e7; }
  .toolbar input { flex: 1; height: 36px; background: #f4f4f5; border: 1px solid #e4e4e7;
                   border-radius: 8px; padding: 0 12px; font: 13px/1 Consolas, monospace; }
  button { display: inline-flex; align-items: center; gap: 6px; height: 36px; padding: 0 14px;
           background: #f4f4f5; color: #18181b; border: 1px solid #e4e4e7; border-radius: 8px;
           cursor: pointer; font-size: 13px; }
  button:hover { background: #e4e4e7; }
  button.primary { background: #4b7a5e; color: #fff; border: none; font-weight: 500; }
  button svg { width: 16px; height: 16px; }
  .playlist { flex: 1; overflow-y: auto; padding: 8px 0; }
  .song { display: flex; align-items: center; gap: 10px; padding: 8px 20px; cursor: pointer; }
  .song:hover { background: #f4f4f5; }
  .song.active { background: #e4e4e7; color: #4b7a5e; font-weight: 500; }
  .song svg { width: 16px; height: 16px; flex-shrink: 0; }
  .player { display: flex; align-items: center; gap: 12px; padding: 12px 20px; background: #fff;
            border-top: 1px solid #e4e4e7; }
  .player audio { flex: 1; height: 36px; }
  .status { padding: 4px 20px; color: #71717a; font-size: 12px; background: #fff;
            border-top: 1px solid #e4e4e7; }
</style>
</head>
<body>
  <div class="toolbar">
    <button class="primary" id="pickDir">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2h6l2 4h6a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z"/></svg>
      选择音乐文件夹
    </button>
    <span id="dirPath" style="color:#71717a;font-size:13px"></span>
  </div>
  <div class="playlist" id="playlist">
    <div class="song" style="color:#71717a;cursor:default">输入路径后点扫描</div>
  </div>
  <div class="player">
    <button id="prev" title="上一首">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="19 20 9 12 19 4 19 20"/><line x1="5" y1="19" x2="5" y2="5"/></svg>
    </button>
    <audio id="audio" controls></audio>
    <button id="next" title="下一首">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="5 4 15 12 5 20 5 4"/><line x1="19" y1="5" x2="19" y2="19"/></svg>
    </button>
  </div>
  <div class="status" id="status">就绪</div>
<script>
const $ = id => document.getElementById(id);
const EXT = ['.mp3','.flac','.wav','.ogg','.m4a','.aac','.wma'];
let songs = [], current = -1, selectedDir = '';

function status(msg) { $('status').textContent = msg; }

// 用壳层 openDialog 弹 Windows 原生选择框 —— 绝不用 <input type="file">/webkitdirectory
// （那是浏览器 API，会触发"上传文件"确认弹窗，跟桌面小工具定位不搭）
async function pickDir() {
  const r = await window.hx.openDialog({ title: "选择音乐文件夹", directory: true });
  if (!r.ok) { status('选目录失败: ' + (r.err || '')); return; }
  if (!r.data) return; // 用户取消
  selectedDir = r.data;
  $('dirPath').textContent = selectedDir;
  scan();
}

async function scan() {
  if (!selectedDir) { status('请先选择文件夹'); return; }
  status('扫描中...');
  const r = await window.hx.readDirDetailed(selectedDir);
  if (!r.ok) { status('扫描失败: ' + r.err); return; }
  songs = r.data
    .filter(it => it.isFile && EXT.some(e => it.name.toLowerCase().endsWith(e)))
    .map(it => ({ name: it.name, size: it.size, path: selectedDir + '\\' + it.name }));
  status('找到 ' + songs.length + ' 首歌曲');
  renderList();
}

function renderList() {
  const html = songs.map((s, i) =>
    '<div class="song' + (i === current ? ' active' : '') + '" data-i="' + i + '">' +
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>' +
    s.name + '</div>').join('');
  $('playlist').innerHTML = html || '<div class="song" style="color:#71717a;cursor:default">无音乐文件</div>';
}

function play(i) {
  if (i < 0 || i >= songs.length) return;
  current = i;
  $('audio').src = window.hx.fileUrl(songs[i].path);
  $('audio').play();
  status('播放中: ' + songs[i].name);
  renderList();
}

$('playlist').addEventListener('click', e => {
  const el = e.target.closest('.song[data-i]');
  if (el) play(parseInt(el.dataset.i));
});
$('audio').addEventListener('ended', () => play(current + 1));
$('prev').onclick = () => play(current - 1);
$('next').onclick = () => play(current + 1);
$('pickDir').onclick = pickDir;
</script>
</body>
</html>
```

## 模板：Vue 3 单文件（内嵌，无 CDN）

> Vue 运行时必须内嵌进单文件 HTML（用 `vue.global.prod.js` 内容直接粘贴到 `<script>` 前），
> **禁止 `<script src="https://unpkg.com/vue...">`**（离线白屏）。

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>Vue 应用</title>
<style>
  /* 同样遵守"前端默认标准"：浅色、中性灰、SVG 图标 */
  body { font: 14px/1.6 system-ui, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
         background: #fafafa; color: #18181b; padding: 24px; }
  .wrap { max-width: 880px; margin: 0 auto; }
  button { display: inline-flex; align-items: center; gap: 6px; height: 36px; padding: 0 14px;
           background: #4b7a5e; color: #fff; border: none; border-radius: 8px; cursor: pointer; font-size: 13px; }
  button svg { width: 16px; height: 16px; }
  .msg { margin-top: 12px; color: #71717a; font-size: 13px; }
</style>
<!-- 内嵌 Vue 运行时：粘贴 vue.global.prod.js 的完整内容到此处 -->
<script>/* VUE_RUNTIME_INLINED_HERE */</script>
</head>
<body>
<div id="app">
  <div class="wrap">
    <h1>Vue 应用</h1>
    <button @click="save">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><path d="M17 21v-8H7v8"/><path d="M7 3v5h8"/></svg>
      保存
    </button>
    <p class="msg">{{ msg }}</p>
  </div>
</div>
<script>
const { createApp, ref } = Vue;
createApp({
  setup() {
    const msg = ref('就绪');
    async function save() {
      const r = await window.hx.writeFile('a.txt', 'hello');
      msg.value = r.ok ? '已保存' : '保存失败: ' + r.err;
    }
    return { msg, save };
  }
}).mount('#app');
</script>
</body>
</html>
```
