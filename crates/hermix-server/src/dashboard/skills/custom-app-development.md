---
name: custom-app-development
version: 15
description: >
  自定义应用开发助手。当用户要求创建/开发一个桌面应用、桌面工具、独立程序、窗口应用、软件、小软件，
  或"打包成 exe"时使用；也用于**用户报告已安装自定义应用报错/打不开/崩溃**的修复场景。
  通过 custom_app_manager 工具创建、更新、打包、运行(dev)、校验、冒烟用户自定义桌面应用。
  基于 hx-shell 运行时（wry/WebView2，单文件 exe），LLM 写前端（TS/React/纯 HTML），Hermix 打包成单文件 exe 分发。
  用户电脑零工具链。务必在以下场景使用：用户说"做个应用/开发一个工具/写个小软件/打包成 exe"，
  或描述需独立窗口运行的功能，或报告"XXX 应用报错/打不开/加载失败 (os error 3)"。
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
- **应用模型** `HERMIX_HOME/custom-apps/<id>/`：`manifest.json` + `index.html`（纯 HTML 模式）或 `src/main.tsx`（TS/React 模式，Vite 构建成单文件 index.html）+ `dist/<id>.exe`（打包产物）。`dist/` 是纯构建产物，**禁止手改**。
- **manifest 权限**：必须含 `permissions` 数组，validate L2 做"权限一致性"卡——HTML/TSX 用到的每个 `hx.*` API 都要有对应权限，否则 `ok=false`。有效值：`fs-read`/`fs-write`/`exec`/`dialog`/`app`/`clipboard`/`net`（最简全声明含 `app` 因 getInfo 强制）。
- **两种模式**：纯 HTML（`index_html`）/ Vue·React·TS（`app_source`+`index_html`）。Vite 必须 `inlineDynamicImports:true` + 超大 `assetsInlineLimit` 以单文件输出。
- **绑定 API**：`window.hx.*` 经 WebView2 原生桥 `postMessage`→Rust `ipc_handler`，**无 CORS 限制**。所有方法返回 `{ok,data?,err?}`。
- **三道强制闸门**：①设计阶段（先出明暗两套配色+布局基线，用户确认再写码）②技术选型与复用检索（先搜 OSS，GitHub 不可达切 Gitee/GitCode）③代码审查与验证（对照 code-review.md + 跑 smoke）。

## Practical Guidance

**agent 开发流（一次性到位交付）**：
1. 识别为自定义应用 → 想清数据存哪/用哪些 `hx.*` API（先 `getInfo()` 取可用清单）/功能气质定配色
2. **设计阶段闸门**：按功能设计明暗两套 token + 布局基线，发设计稿等用户确认；未确认禁写码
3. **复用检索闸门**：能力点先搜 GitHub（MIT/Apache/BSD），不可达切 Gitee/GitCode；记录 项目/版本/License/嵌入方式；无合适才自研
4. 写 `manifest.json`（`long_description` 必填：功能/使用/更新/分发方法）
5. 写码（`index_html` 或 `app_source` TSX），严格按设计 token + 前端默认标准（浅色/中性灰/禁蓝紫/禁 emoji/禁 CDN/必带标题栏/必含主题切换）
6. **先 `custom_app_manager.list` 查重** → 已有同类用 `update` 迭代，否则 `create`
7. `validate` 循环到 `ok=true`（L0 manifest/L1 HTML/L2 权限/L3 前端质量），不得带 error 交付
8. `run` 测试版即时预览（`hx-shell --html` 加载 dev.html）；要确认无报错调 `smoke`
9. **代码审查闸门**：对照 code-review.md + frontend-architecture.md，跑 smoke 确认无运行时/控制台报错，提示友好
10. 交付说明：去哪运行（run 测试版 / package 出 exe）、如何打包、如何回来 `update` 继续

**关键 `hx.*` API（常用）**：
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

- **【致命】禁止 `<input type=file>`/`webkitdirectory`/手输路径选文件**：触发 WebView2"上传"确认弹窗，与桌面定位不搭。永远用 `openDialog`/`saveDialog` 弹 Windows 原生框。
- **【致命】禁止 `fetch('app://...')`**：CORS 拒绝。一律 `window.hx.*`（走 `chrome.webview.postMessage`）。
- **【致命】不要用 Tauri API**（`convertFileSrc`/`shell_*`）**或 Node API**（`path`/`fs`/`child_process`）：hx-shell 里不存在，只有 `window.hx.*`。
- **【致命】禁止 CDN 外链**（unpkg/jsdelivr）：Vue/React/图标全部内联或 Vite 打包内联，离线可用。
- **【致命】禁止 emoji**（界面/按钮/状态/manifest icon）：图标用内联 SVG（lucide 风格 24×24 `stroke=currentColor`）。
- **【致命】查/改应用只走 `custom_app_manager`**：绝不自己拼 `C:/Users/*/hermix/custom-apps` 等路径去 `ls`/`read_file`（HERMIX_HOME 运行时解析，硬猜必错）。
- **【致命】create 前必须 `list` 查重**：同类应用（如"本地音乐"="音乐播放器"）已存在就 `update`，绝不再建第二个。
- **【致命】V7 壳无原生标题栏**：`decorations:false`，HTML 顶部必须自实现标题栏（拖动区+最小化/最大化/关闭），否则用户无法移动/关闭。
- **禁止手改 `dist/`**：`dist/<id>.exe` 与 `<id>.dev.html` 每次 `run`/`package` 从源码重建覆盖，改源码后必须重新 `run`/`package`。
- **默认浅色**：不要上来写深色；墨绿 `#4b7a5e` 仅作未做功能化配色时的回退，禁止所有应用统一用它；中性灰优先，禁高饱和蓝紫。
- **二进制用 `fileUrl`**：`readFile` 只读文本，MP3/图片/视频用 `hx.fileUrl(path)` 生成 URL。

## Integration

- **唯一入口工具 `custom_app_manager`**：`list`/`get`/`create`/`update`/`package`/`run`/`validate`/`smoke`/`delete`。所有查/改/打包/运行都走它，落盘到 `hermix_home()/custom-apps/<id>/`（数据目录，与安装位置/源码无关）。
- **`@app` 快捷入口**：聊天 `@app 创建新应用` → 输入框预填 `@app 创建新应用 应用要求：<需求>`，直接进需求梳理，不要当成查已有应用。
- **三闸门协同**：设计阶段 → 复用检索 → 代码审查，三者都过才交付；审查方法论见同目录 `code-review.md`，前端质量基线见 `frontend-architecture.md`。
- **与对话流关系**：本 skill 在用户提"应用/工具/软件"诉求或报告已装应用报错时自动加载；运行时若 `getInfo()` 返回 API 与本文不符，以 `getInfo()` 为准（本文档可能滞后）。

## References

- 同目录 `code-review.md`：交付前审查清单（无低级错误/死代码/权限一致/离线可用/设计落实）
- 同目录 `frontend-architecture.md`：前端质量基线（布局层级/控件尺寸/响应式）
- 图标库：内置 2007 个 Lucide SVG，清单见 `apps/desktop-tauri/src-tauri/resources/icons/`（文件名去 `.svg` 即 `manifest.icon` 名，如 `music`/`settings`/`folder`）
- **`tile_color`（可选）**：`#RRGGBB` 或 `#RRGGBBAA`，给列表卡片/详情头部/窗口标题栏/打包 exe 任务栏图标加一个**纯色圆角背景块 + 白色字形**（无渐变）。三处同源（`iconResolver` + `icon_library::generate_tiled_icon_*`），视觉一致。规则：`icon_file`/`icon_data`（用户自备 PNG logo）或 `icon` 为 emoji 时**跳过**背景（与前端逻辑一致）；非法值被 `validate_tile_color` 拒绝。不填则列表/详情回退灰底，exe 走 hash 调色板兜底。
- hx-shell API 真相源：`window.hx.getInfo()`（运行时返回完整可用 API 清单，本文档可能滞后）
