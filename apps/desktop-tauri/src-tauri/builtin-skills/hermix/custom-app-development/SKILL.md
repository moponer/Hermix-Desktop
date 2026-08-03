---
name: custom-app-development
version: 5
description: >
  自定义应用开发助手。当用户要求创建桌面应用、桌面工具、独立程序、
  窗口应用、打包成 EXE 的程序时使用。通过 custom_app_manager 工具
  创建、更新、打包、运行、校验、冒烟用户自定义桌面应用。
  基于 hx-shell 运行时（wry/WebView2，~1MB exe），LLM 写前端代码
  （Vue/React/纯 HTML），Hermix 打包成单文件 exe 分发。
  用户电脑零工具链：打包 = 文件尾部注入 HTML，浏览器/WebView2 即跑。
  务必在以下场景使用：用户说"做个应用""写个桌面工具""打包成 exe"
  "做个小工具""做个窗口程序""帮我做个 app""创建自定义应用""做个独立程序"
  "做个桌面应用""make an app""create a desktop app""build a tool"
  "package as exe"；或用户描述一个需要独立窗口运行的功能需求时。
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
  - javascript
  - 自定义应用
  - 桌面应用
  - 打包
  - 独立程序
  - 窗口应用
  - 做个应用
---

# Custom App Development（基于 hx-shell）

## 运行时：hx-shell

- **HX-SHELL** = `wry 0.55.1` + WebView2，单文件 exe 约 **1MB**
- 打包产物 = `hx-shell.exe` + `index.html` 尾部注入（`[html][len:8LE][HXAPP1]`）
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
    <id>.exe         ← 打包产物（hx-shell.exe + html + trailer）
    WebView2Loader.dll  ← wry 运行时依赖
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

壳通过 WebView2 原生桥提供 RPC。LLM 直接调用：

| 方法 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `hx.writeFile(path, content)` | string, string | `{ok}` | 写文件（UTF-8） |
| `hx.readFile(path)` | string | `{ok, data}` | 读文件 |
| `hx.getInfo()` | — | `{ok, data}` | 壳信息 |

```js
await window.hx.writeFile("data.txt", "hello");
const data = await window.hx.readFile("data.txt");
```

**实现原理**（了解即可）：
- `hx.writeFile(p,c)` → `window.chrome.webview.postMessage(JSON.stringify({id, m:"writeFile", a:[p,c]}))`
- Rust 端 ipc_handler 收到 → 处理 → `evaluate_script("window['__hx_resolve_<id>'](result)")`
- JS 端 `window['__hx_resolve_<id>']` 是 Promise resolver → 返回结果

**别用 fetch 到 `https://app.localhost/hx/...`**（custom scheme 的 fetch 被 Chromium CORS 拒绝）—— 必须用 `window.chrome.webview.postMessage`！

### 系统能力不足？用纯前端 JS

- 数据：`localStorage` / `IndexedDB` / **SQLite-wasm**（本地数据库）
- 网络：`fetch()`（到 https URL）
- 媒体：Canvas / WebGL / `<video>` / `<audio>`
- 算力：**wasm 库**（ffmpeg-wasm、pdf-wasm、jszip）

## agent 开发流

1. 写 `manifest.json`：
   ```json
   {"id": "my-app", "name": "我的应用", "window_config": {"width": 900, "height": 600, "resizable": true}}
   ```
2. 写 `index.html`（自包含，或 Vite 单文件构建产物）
3. 调 `custom_app_manager.create({manifest, index_html, app_source?, source_files?})`
4. （可选）`validate` 检查 HTML/JS 完整性
5. （可选）`smoke` 自动冒烟（执行 `hx.writeFile("auto-verify.txt", ...)` + 读回）
6. `package` 打包成 `dist/<id>.exe` + `dist/WebView2Loader.dll`
7. `run` 启动 app
8. 把 `dist/<id>.exe` 发给用户，用户双击即跑（WebView2 系统自带，无需安装）

## 关键约束

- **不要 import React**：`window.React` 是全局变量（从 CDN 或单文件 HTML 内嵌），用 `const { useState } = React;`
- **不要 `fetch('app://...')`**：CORS 拒绝。用 `window.hx.*`（内部走 chrome.webview.postMessage）
- **不要依赖 Node.js API**（path/fs/child_process）：沙箱里没有，用 `window.hx.*` 替代
- **不要用 sourcemap 单独文件**：Vite 单文件输出不要 `.map`（影响单文件化）
- **HTML 文件编码**：UTF-8，无 BOM

## 模板：纯 HTML 工具

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>My Tool</title>
<style>body{font:14px system-ui;padding:24px;background:#0f0f12;color:#e4e4e7}</style>
</head>
<body>
  <h1>My Tool</h1>
  <textarea id="in" placeholder="输入..."></textarea>
  <button id="save">保存到文件</button>
  <pre id="out"></pre>
<script>
document.getElementById('save').onclick = async () => {
  const r = await window.hx.writeFile('output.txt', document.getElementById('in').value);
  document.getElementById('out').textContent = JSON.stringify(r);
};
</script>
</body>
</html>
```

## 模板：Vue 3 单文件

```html
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Vue App</title>
<style>/* ... */</style>
<script src="https://unpkg.com/vue@3/dist/vue.global.prod.js"></script>
</head>
<body>
<div id="app"><button @click="save">保存</button><p>{{ msg }}</p></div>
<script>
const { createApp, ref } = Vue;
createApp({
  setup() {
    const msg = ref('ready');
    async function save() {
      const r = await window.hx.writeFile('a.txt', 'hello');
      msg.value = JSON.stringify(r);
    }
    return { msg, save };
  }
}).mount('#app');
</script>
</body>
</html>
```