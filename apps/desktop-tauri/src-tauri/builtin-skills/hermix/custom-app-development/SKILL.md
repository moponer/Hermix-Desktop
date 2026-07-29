---
name: custom-app-development
description: >
  自定义应用开发助手。当用户要求创建桌面应用、桌面工具、独立程序、
  窗口应用、打包成 EXE 的程序时使用。通过 custom_app_manager 工具
  创建、更新、打包、运行、校验、冒烟用户自定义桌面应用。
  系统支持两套架构共存：
    (A) 新架构 (推荐)：自包含 index.html + manifest.json，经极简 wry 加载器
        + inject_app 注入成单文件 exe，经 app://localhost/rpc 单通道桥接
        访问系统能力 (capability 真沙箱，默认 deny)；产出真正独立、零 Tauri
        运行时依赖的 exe，可分发到未装宿主的机器。
    (B) 旧架构 (兼容)：backend.ts + ui.tsx，经 esbuild 打包 + 预编译 Tauri 壳，
        经 Tauri invoke 访问 shell_* 命令 (假沙箱，壳无条件注册全部命令)。
  用户无需安装 Rust/Tauri/Node.js/Deno。
  务必在以下场景使用：用户说"做个应用""写个桌面工具""打包成 exe"
  "做个小工具""做个窗口程序""帮我做个 app""创建自定义应用""做个独立程序"
  "做个桌面应用""make an app""create a desktop app""build a tool"
  "package as exe"；或用户描述一个需要独立窗口运行的功能需求时。
  Keywords (English, for keyword auto-routing): custom app, desktop app,
  create app, build tool, package exe, standalone application, window app,
  desktop tool, make an app.
tags:
  - custom-app
  - desktop
  - application
  - packaging
  - exe
  - webview
  - wry
  - standalone
  - 自定义应用
  - 桌面应用
  - 打包
  - 独立程序
  - 窗口应用
  - 做个应用
---

# Custom App Development

通过对话创建用户自定义桌面应用。应用代码在 WebView2 (V8) 中运行，
打包时产出独立 EXE，用户无需安装任何开发工具。

## 两套架构（共存）

| | 新架构 (A, 推荐) | 旧架构 (B, 兼容) |
|---|---|---|
| 源码 | 单文件 `index.html` + `manifest.json` | `backend.ts` + `ui.tsx` |
| 打包链 | wry 加载器 + `inject_app` 尾部注入 | esbuild 打包 TS + 预编译 Tauri 壳 |
| 系统调用 | `fetch("app://localhost/rpc", …)` 单通道 JSON-RPC | Tauri `invoke("shell_*", …)` |
| 沙箱 | **真沙箱**：manifest.permissions 门控每个方法，默认 deny | 假沙箱：壳无条件注册全部命令，不校验 |
| 运行时依赖 | 仅 WebView2（无 Tauri 运行时） | WebView2 + Tauri 壳 |
| 产物独立度 | 可分发到未装宿主的机器直接运行 | 可独立运行，但体积更大 |
| 何时用 | 新应用一律用此；用户要"独立 exe"时必用 | 已有旧应用维护；需要 React 组件 UI 时 |

> **默认用新架构 (A)**。`create` 时传 `index_html` 即走 A；传 `backend_code`/`ui_code` 走 B。
> 一个应用只选一种模型（同时传时以 `index_html` 为准）。

## 新架构 (A)：index.html 模型

应用 = 一个自包含的 `index.html`（可内联任意 JS/CSS）+ 一个 `manifest.json`。
打包时把 html + manifest 注入到极简加载器尾部，得到单文件 exe。

### manifest.json（新架构）

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | string | 是 | 唯一标识符（字母数字-_） |
| `name` | string | 是 | 应用名称 |
| `permissions` | string[] | 是 | 能力白名单（见下表），默认 deny |
| `window_config.width` | number | 否 | 窗口宽度（默认 600） |
| `window_config.height` | number | 否 | 窗口高度（默认 480） |
| `window_config.resizable` | boolean | 否 | 可调整大小（默认 true） |

`permissions` 取值（declarative，映射到具体桥接方法）：

| 权限声明 | 放行的桥接方法 |
|----------|----------------|
| `fs-read` | `fs_read` `fs_read_bytes` `fs_list` `fs_exists` |
| `fs-write` | `fs_write` `fs_write_bytes` `fs_mkdir` `fs_move` `fs_copy` `fs_delete` |
| `exec` | `exec` |
| `network` | `fetch` |
| `dialog` | `dialog_open` `dialog_save` |
| `env` | `env_get` `env_list` |
| `temp` | `temp_dir` `temp_file` |

`path_join` / `path_basename` / `path_dirname` / `path_extname` 是纯路径计算，**永远放行**，无需声明。

### 桥接 RPC（前端调用系统能力）

所有系统能力走同一个端点：

```js
async function rpc(method, params) {
  const res = await fetch("app://localhost/rpc", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id: Date.now(), method, params }),
  });
  return await res.json(); // { id, ok, result?, error? }
}
```

方法表（签名与旧架构 `shell_*` 大体对应，但方法名更短、且受 capability 门控）：

| 能力 | 方法 | params | result |
|------|------|--------|--------|
| 读文本 | `fs_read` | `{path}` | string |
| 读字节 | `fs_read_bytes` | `{path}` | `{base64}` |
| 写文本 | `fs_write` | `{path, content}` | null |
| 写字节 | `fs_write_bytes` | `{path, base64}` | null |
| 列目录 | `fs_list` | `{dir}` | `[{name,is_dir,size,modified}]` |
| 存在? | `fs_exists` | `{path}` | bool |
| 删除 | `fs_delete` | `{path}` | null |
| 建目录 | `fs_mkdir` | `{path}` | null |
| 移动 | `fs_move` | `{from,to}` | null |
| 复制 | `fs_copy` | `{from,to}` | null |
| 执行命令 | `exec` | `{cmd, args?, cwd?, timeout_secs?}` | `{stdout,stderr,exit_code}` |
| HTTP | `fetch` | `{url, method?, headers?, body?}` | `{status,body,headers}` |
| 打开对话框 | `dialog_open` | `{title?, filters?}` | string\|null |
| 保存对话框 | `dialog_save` | `{title?, default_name?}` | string\|null |
| 拼路径 | `path_join` | `{parts:[...]}` | string |
| 文件名 | `path_basename` | `{path}` | string |
| 目录名 | `path_dirname` | `{path}` | string |
| 扩展名 | `path_extname` | `{path}` | string |
| 取环境变量 | `env_get` | `{key}` | string\|null |
| 列环境变量 | `env_list` | — | `{"K":"V",…}` |
| 临时目录 | `temp_dir` | — | string |
| 临时文件 | `temp_file` | `{prefix?, suffix?}` | string |

> 未声明对应 `permissions` 时调用会被拒绝，返回 `{ok:false, error:"permission denied"}`。

### index.html 模板（新架构）

```html
<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8" />
    <title>我的应用</title>
    <style> body { font-family: system-ui; background:#0f0f12; color:#e0e0e0; padding:20px; } </style>
  </head>
  <body>
    <h2>我的应用</h2>
    <button id="pick">选择文件</button>
    <pre id="out"></pre>
    <script>
      async function rpc(method, params) {
        const res = await fetch("app://localhost/rpc", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ id: Date.now(), method, params }),
        });
        return await res.json();
      }
      document.getElementById("pick").onclick = async () => {
        const r = await rpc("dialog_open", { title: "选择文件" });
        if (r.ok && r.result) document.getElementById("out").textContent = "已选: " + r.result;
      };
    </script>
  </body>
</html>
```

### create（新架构）

```json
{
  "action": "create",
  "manifest": {
    "id": "my-tool",
    "name": "我的工具",
    "permissions": ["fs-read", "fs-write", "dialog", "temp"],
    "window_config": { "width": 620, "height": 520, "resizable": true }
  },
  "index_html": "<!doctype html>…完整 HTML 字符串…",
  "icon_data": "data:image/png;base64,iVBOR…"
}
```

## 旧架构 (B)：backend.ts + ui.tsx 模型

> 仅用于维护已有旧应用，或需要 React 组件 UI 的场景。新应用请用 (A)。

`backend.ts` 在 WebView2 中运行，**不能使用 Node.js API**（`fs`/`path` 等），
所有系统操作通过 Tauri `invoke("shell_*", …)` 完成。`ui.tsx` 是 React 组件，
由 esbuild 打包（React 经 `--jsx=automatic` 内联，不从 CDN 加载）。

### create（旧架构）

```json
{
  "action": "create",
  "manifest": {
    "id": "pdf-converter", "name": "PDF 转换器", "version": "1.0.0",
    "description": "将图片转换为 PDF 文件", "author": "LLM-generated",
    "icon": "📄", "category": "file-tools", "tags": ["pdf","image"],
    "window_config": { "width": 800, "height": 600, "resizable": true },
    "permissions": ["fs-read", "fs-write"]
  },
  "backend_code": "// backend.ts 代码",
  "ui_code": "// ui.tsx 代码",
  "icon_data": "data:image/png;base64,iVBOR…"
}
```

`backend.ts` 导出 `execute(params)`，UI 经 `window.__APP_BACKEND__.execute(params)` 调用。
可用 `invoke`：`shell_fs_read` `shell_fs_write` `shell_fs_list` `shell_fs_exists`
`shell_fs_delete` `shell_fs_mkdir` `shell_fs_move` `shell_fs_copy` `shell_exec`
`shell_fetch` `shell_dialog_open` `shell_dialog_save` `shell_path_join`
`shell_path_basename` `shell_path_dirname` `shell_path_extname` `shell_env_get`
`shell_env_list` `shell_temp_dir` `shell_temp_file`。（此法为旧架构假沙箱，不校验 permissions。）

## 通用 action（两种架构通用）

### package — 打包为独立 EXE
```json
{ "action": "package", "id": "my-tool" }
```

### run — 启动已打包的应用
```json
{ "action": "run", "id": "my-tool" }
```

### validate — 开发期自验证（必须，闭环）
```json
{ "action": "validate", "id": "my-tool" }
```
返回 `{success, message, validation:{ok, issues:[{level,code,message}]}}`。
- 新架构：校验 `manifest.permissions` 完整性 + `index.html` 是否调用了未授权
  的 `app://localhost/rpc` 方法（缺权限会提示补声明）。
- 旧架构：校验 manifest + `ui.tsx`/`backend.ts` 导入白名单（只允许 react /
  react-dom/client + 相对/URL 路径，裸 npm 包报错）+ 同款 esbuild 预打包。
`ok == false` 时按 `issues` 修复后重试，**直到 `ok == true`** 才算开发完成。

### smoke — 运行时冒烟（L2，可选）
```json
{ "action": "smoke", "id": "my-tool" }
```
先刷新打包产物，再无头启动应用，捕获运行时错误（白屏 / 挂载失败 / 桥接异常）。
返回 `{success, message, smoke:{ok, ready, errors, detail}}`。仅在需要进一步确认
运行时行为时调用（会真实启动隐藏进程，较重）。

## 开发流程

1. **理解需求**：分析用户想要什么样的桌面应用。
2. **选架构**：新应用一律用 (A) 新架构（独立 exe + 真沙箱）。
3. **设计 manifest**：确定 `id`/`name`/`window_config`/**`permissions`**（按实际需要的系统能力声明）。
4. **编写 index.html**：实现 UI + 通过 `app://localhost/rpc` 调用系统能力。
5. **create / update**：用 `custom_app_manager` 的 `create`（`index_html`）/`update` action。
6. **★ 开发期自验证（必须）**：每次生成/修改后调 `validate`，`ok == false` 则修复重试，
   直到 `ok == true`。这一步把 manifest 错误、桥接方法缺权限、esbuild 失败挡在开发期。
7. **（可选）冒烟**：想确认「打包后能跑 / 不白屏」时调 `smoke`。
8. **提示打包/运行**：告诉用户在「应用」页面点「打包」「启动」即可。

## 最佳实践

- **默认新架构 (A)**，除非在维护旧应用。
- **最小权限**：只声明实际用到的 `permissions` 方法；未声明会被桥接拒绝。
- **错误处理**：所有 `rpc()` 调用检查 `r.ok`，桥接异常在 `r.error` 返回。
- **文件操作**：用 `dialog_open` 让用户选文件，不要硬编码路径。
- **中文 UI**：面向中国用户，UI 文本用中文。
- **自定义图标**：传 `icon_data`（base64 PNG，推荐 256/512 正方形），让任务栏/标题栏有独特图标。
- **环境无外部依赖**：新架构 exe 仅依赖 WebView2，可分发到未装宿主的机器直接运行。

## 常见问题

**Q: 新架构能用 npm 包 / React 吗？**
A: `index.html` 是纯前端页面，可内联任意 JS（含 React CDN 或自打包的 bundle）。
若需要 React 组件化，自行用 esbuild/打包工具产出 JS 内联进 html 即可；宿主不替你打包。

**Q: 调用 rpc 报 "permission denied"？**
A: 在 `manifest.permissions` 里补上对应权限声明（如 `fs-write`/`exec`/`network`）。

**Q: 应用可以联网吗？**
A: 新架构声明 `network` 权限后用 `rpc("fetch", {url, method, headers, body})`；
旧架构用 `invoke("shell_fetch", …)` 或原生 `fetch`。

**Q: 如何分发独立 exe？**
A: `package` 产出的 exe 自包含 html + manifest（尾部 payload），仅依赖系统 WebView2，
可拷贝到任何 Windows 机器直接运行，无需安装宿主。
