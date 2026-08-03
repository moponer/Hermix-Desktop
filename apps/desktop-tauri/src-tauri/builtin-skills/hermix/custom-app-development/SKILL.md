---
name: custom-app-development
version: 6
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

## 前端默认标准（必须遵守，违反视为不合格）

> **这是硬性标准**。LLM 生成的任何自定义应用界面，默认必须符合以下规范；
> 只有用户**明确要求**深色/特殊风格时才允许偏离。

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
| 强调色（主按钮/选中态） | `#4b7a5e`（墨绿，Hermix 品牌色） |
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

- [ ] 浅色默认、中性灰配色、无蓝紫
- [ ] 主操作按钮都有 SVG 图标
- [ ] 全文无 emoji
- [ ] 无 CDN 外链（Vue/React/图标全部内联）
- [ ] 布局有层级：标题/工具栏/内容/状态分区
- [ ] 按钮/输入框尺寸一致、间距统一
- [ ] 失败场景有具体错误提示

## agent 开发流

1. 写 `manifest.json`：
   ```json
   {"id": "my-app", "name": "我的应用", "window_config": {"width": 900, "height": 600, "resizable": true}}
   ```
2. 写 `index.html`（自包含，或 Vite 单文件构建产物）——**按"前端默认标准"生成界面**
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
- **不要 CDN**：Vue/React/图标全部内联（离线可用）
- **不要 emoji**：图标用内联 SVG
- **默认浅色**：不要上来就写深色

## 模板：纯 HTML 工具（浅色 + SVG 图标）

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>文本工具</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font: 14px/1.6 system-ui, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
         background: #fafafa; color: #18181b; padding: 24px; }
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
<div class="wrap">
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
</div>
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
