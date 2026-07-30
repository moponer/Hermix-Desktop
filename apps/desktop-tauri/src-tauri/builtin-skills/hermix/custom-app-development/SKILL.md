---
name: custom-app-development
version: 2
description: >
  自定义应用开发助手。当用户要求创建桌面应用、桌面工具、独立程序、
  窗口应用、打包成 EXE 的程序时使用。通过 custom_app_manager 工具
  创建、更新、打包、运行、校验、冒烟用户自定义桌面应用。
  应用 = 自包含 index.html + manifest.json，经 wry 加载器
  + inject_app 注入成单文件 exe，经 app://localhost/rpc JSON-RPC
  访问系统能力。产出真正独立的 exe，可分发到任意 Windows 机器运行。
  用户无需安装 Rust/Tauri/Node.js/Deno。
  务必在以下场景使用：用户说"做个应用""写个桌面工具""打包成 exe"
  "做个小工具""做个窗口程序""帮我做个 app""创建自定义应用""做个独立程序"
  "做个桌面应用""make an app""create a desktop app""build a tool"
  "package as exe"；或用户描述一个需要独立窗口运行的功能需求时。
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

## 应用模型

应用 = 一个自包含的 `index.html`（可内联任意 JS/CSS）+ 一个 `manifest.json`。
打包时把 html + manifest 注入到极简 wry 加载器尾部，得到单文件 exe。
exe 可分发到任意 Windows 机器运行，仅依赖系统 WebView2。

应用文件存储目录：`HERMIX_HOME/custom-apps/<id>/`（注意是连字符 `custom-apps`）。

## ★硬约束（不可违反）★

自定义应用开发有三条铁律，**任何情况下都不得绕过**：

1. **唯一入口 = `custom_app_manager` 工具**。应用的 `manifest.json` / `index.html` 只能由工具的 `create` / `update` action 写入，工具会把它存到正确目录（`HERMIX_HOME/custom-apps/<id>/`）。**严禁**用 `fs_write` / `shell` / 其他文件工具把应用文件写到任意路径（如 `release/apps/`、项目根目录、桌面等）——写错路径自定义应用界面扫不到，等于白做。
2. **应用模型 = 单文件 `index.html` + `manifest.json`**。**严禁**生成 `backend.ts` / `ui.tsx` / `src/` 等多文件结构——当前加载器只认自包含 `index.html`。
3. **强制闭环交付**。每个应用必须走 `create → validate（直到 ok=true）→（可选 smoke）→ package → run`。**严禁**"生成文件后让用户手动导入 / 手动打包"——交付标准 = 应用在自定义应用页面可见且可运行。

**若 `custom_app_manager` 工具不可用（系统提示里没有它 / 调用报"未注册"）：立即停下来明确告知用户，不要降级到 `fs_write` 自创路径。** 工具缺失是环境问题，不是让你裸写文件的理由。

## ★工具调用格式（关键！）★

`custom_app_manager` 的 `manifest` 和 `window_config` 参数是**嵌套对象**，
`permissions` 和 `tags` 是**数组**。在 XML 工具调用格式中，这些**必须以 JSON 字符串**作为
`<parameter>` 的内容传递，不能使用 XML 子标签嵌套：

✅ **正确写法**（JSON 作为 parameter 内容）：
```xml
<function_calls>
<invoke name="custom_app_manager">
<parameter name="action">create</parameter>
<parameter name="manifest">{"id":"my-tool","name":"我的工具","version":"1.0.0","description":"一个工具","author":"Hermix","icon":"🔧","permissions":["fs-read","dialog"],"window_config":{"width":620,"height":520}}</parameter>
<parameter name="index_html"><![CDATA[<!doctype html>...完整 HTML...]]></parameter>
</invoke>
</function_calls>
```

❌ **错误写法**（XML 子标签嵌套——解析器会把整段当字符串，Rust 端反序列化失败）：
```xml
<!-- 不要这样写！ -->
<manifest>
  <id>my-tool</id>
  <name>我的工具</name>
  <window_config>
    <width>620</width>
  </window_config>
</manifest>
```

> 解析原理：`<parameter name="X">VALUE</parameter>` 中的 VALUE 会被尝试
> 用 `serde_json::from_str` 解析。如果 VALUE 是合法 JSON（对象 `{...}` 或
> 数组 `[...]`），则解析为 JSON 值；否则降级为字符串。所以嵌套对象/数组
> **必须用 JSON 文本**作为 parameter 内容。

> **容错机制**：即使 manifest 被传成了字符串（如 XML 子标签嵌套导致），
> 后端会尝试将其二次解析为 JSON 对象。但如果内容不是合法 JSON，
> 仍会失败。所以**务必用 JSON 对象写法**，不要依赖容错。

> `index_html` 内容可能包含 `<` 和 `>` 字符，建议用 `<![CDATA[...]]>` 包裹，
> 或确保 HTML 中的特殊字符不干扰 XML 解析。

## manifest.json

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `id` | string | **是** | — | 唯一标识符（字母数字-_） |
| `name` | string | **是** | — | 应用名称 |
| `version` | string | 否 | `"1.0.0"` | 语义版本号 |
| `description` | string | 否 | `""` | 简短描述 |
| `long_description` | string | 否 | — | 详细描述 |
| `author` | string | 否 | `"Hermix"` | 作者 |
| `icon` | string | 否 | `"📦"` | Emoji 图标（用于应用列表显示） |
| `icon_file` | string | 否 | — | 自定义图标文件名（如 `icon.png`） |
| `category` | string | 否 | `"other"` | 分类（file-tools, media, productivity 等） |
| `tags` | string[] | 否 | `[]` | 搜索/过滤标签 |
| `window_config` | object | 否 | 600×480 | 窗口配置（见下） |
| `permissions` | string[] | 否 | `[]` | 能力声明（见下表） |
| `dependencies` | object | 否 | — | 外部依赖（已弃用） |

`window_config` 子字段（全部可选）：

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `width` | number | 600 | 窗口宽度 |
| `height` | number | 480 | 窗口高度 |
| `resizable` | boolean | true | 可调整大小 |
| `title` | string | — | 窗口标题（默认用应用名称） |

`permissions` 取值（声明后对应方法才可调用）：

| 权限声明 | 放行的桥接方法 |
|----------|----------------|
| `fs-read` | `fs_read` `fs_read_bytes` `fs_list` `fs_exists` |
| `fs-write` | `fs_write` `fs_write_bytes` `fs_mkdir` `fs_move` `fs_copy` `fs_delete` |
| `exec` | `exec` `shell` |
| `network` | `fetch` |
| `dialog` | `dialog_open` `dialog_save` |
| `env` | `env_get` `env_list` |
| `temp` | `temp_dir` `temp_file` |

`path_join` / `path_basename` / `path_dirname` / `path_extname` 是纯路径计算，**永远放行**，无需声明。

## 桥接 RPC（前端调用系统能力）

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

方法表：

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
| 执行程序 | `exec` | `{cmd, args?, cwd?, timeout_secs?}` | `{stdout,stderr,exit_code}` |
| Shell 命令 | `shell` | `{command, timeout_secs?}` | `{stdout,stderr,exit_code}` |
| HTTP | `fetch` | `{url, method?, headers?, body?}` | `{status,body,headers}` |
| 打开对话框 | `dialog_open` | `{title?, filters?, type?}` | string\|null |
| 保存对话框 | `dialog_save` | `{title?, default_name?}` | string\|null |
| 拼路径 | `path_join` | `{parts:[...]}` | string |
| 文件名 | `path_basename` | `{path}` | string |
| 目录名 | `path_dirname` | `{path}` | string |
| 扩展名 | `path_extname` | `{path}` | string |
| 取环境变量 | `env_get` | `{key}` | string\|null |
| 列环境变量 | `env_list` | — | `{"K":"V",…}` |
| 临时目录 | `temp_dir` | — | string |
| 临时文件 | `temp_file` | `{prefix?, suffix?}` | string |

> `shell` 是 `exec` 的便捷方法：直接执行 PowerShell 命令字符串，不需要拆分 cmd+args。
> 适合快速执行 `dir`、`python script.py` 等一行命令。

> **`dialog_open` 的 `type` 参数**：默认 `"file"`（选文件），传 `"folder"` 可选择文件夹。
> 选文件夹时 `filters` 参数无效。示例：
> ```js
> // 选择文件夹
> const folder = await rpc('dialog_open', { title: '选择文件夹', type: 'folder' });
> // 选择文件
> const file = await rpc('dialog_open', { title: '选择音乐', filters: [['MP3', ['mp3']]] });
> ```

> 未声明 `permissions` 时调用会被拒绝，返回的 `error` 字段包含详细修复提示
> （告诉你在 manifest 中添加哪个权限）。

## 本地文件直通 (/file 端点)

对于 `<audio>`、`<video>`、`<img>` 等需要直接引用本地文件的场景，
使用 `app://localhost/file?path=` 端点，无需 base64 转换：

```js
// 音频播放器
const audio = document.getElementById('audio');
const r = await rpc('dialog_open', { title: '选择音乐', filters: [['MP3', ['mp3']]] });
if (r.ok && r.result) {
  // 直接把本地文件路径传给 <audio> 的 src
  audio.src = 'app://localhost/file?path=' + encodeURIComponent(r.result);
  audio.play();
}
```

```js
// 图片查看器
const img = document.getElementById('img');
const r = await rpc('dialog_open', { title: '选择图片' });
if (r.ok && r.result) {
  img.src = 'app://localhost/file?path=' + encodeURIComponent(r.result);
}
```

支持的文件类型（自动识别 Content-Type）：
- 音频：mp3, wav, ogg, flac, m4a, aac, wma
- 视频：mp4, webm, avi, mkv, mov
- 图片：jpg, png, gif, svg, webp, bmp, ico
- 文档：pdf, html, txt, json, xml

**需要 `fs-read` 权限**。支持 HTTP Range 请求，音频/视频可正常拖动进度条。

## index.html 模板

```html
<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8" />
    <title>我的应用</title>
    <style>
      body { font-family: system-ui; background:#1a1a2e; color:#e0e0e0; padding:20px; }
      button { background:#4a9eff; color:white; border:none; padding:8px 16px; border-radius:4px; cursor:pointer; margin:4px; }
      button:hover { background:#3a8eef; }
      pre { background:#16213e; padding:12px; border-radius:4px; overflow:auto; }
    </style>
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
        else if (!r.ok) document.getElementById("out").textContent = "错误: " + r.error;
      };
    </script>
  </body>
</html>
```

## Action

### create — 创建应用

**manifest 参数是嵌套对象，必须以 JSON 字符串传递（见上方"工具调用格式"章节）**。

```json
{
  "action": "create",
  "manifest": {
    "id": "my-tool",
    "name": "我的工具",
    "version": "1.0.0",
    "description": "一个实用的桌面工具",
    "author": "Hermix",
    "icon": "🔧",
    "category": "productivity",
    "permissions": ["fs-read", "fs-write", "dialog", "exec", "temp"],
    "window_config": { "width": 620, "height": 520, "resizable": true }
  },
  "index_html": "<!doctype html>…完整 HTML 字符串…",
  "icon_data": "data:image/png;base64,iVBOR…"
}
```

> 最简 manifest 只需 `{"id":"my-tool","name":"我的工具"}`，
> 其余字段都有默认值（`window_config` 默认 600×480 可调整大小，`permissions` 默认空数组）。
> 建议填写 `description` / `icon` / `permissions` 以获得更好的应用列表展示和正确的权限声明。
> **不要**省略 manifest 或传空字符串——`id` 和 `name` 是必填的。

### update — 更新应用代码
```json
{
  "action": "update",
  "id": "my-tool",
  "index_html": "<!doctype html>…修改后的完整 HTML…"
}
```

> `update` 也可以传 `manifest` 来更新清单（如补充 permissions），格式同 create。

### package — 打包为独立 EXE
```json
{ "action": "package", "id": "my-tool" }
```

### run — 启动已打包的应用
```json
{ "action": "run", "id": "my-tool" }
```

### validate — 开发期自验证（★必须★，闭环纠错）
```json
{ "action": "validate", "id": "my-tool" }
```
返回 `{success, message, validation:{ok, issues:[{level,code,message}]}}`。
校验内容：
- **manifest 完整性**：必填字段是否齐全、JSON 格式是否正确
- **HTML 结构**：`<html>`/`<body>`/`<script>` 标签完整性、`<script>` 是否闭合
- **RPC 调用模式**：是否包含 `app://localhost/rpc` 调用
- **权限一致性**：HTML 中使用的 RPC 方法是否都在 `permissions` 中声明
- **JS 括号匹配**：基本的括号闭合检查

`ok == false` 时按 `issues` 中的 `message` 修复后重试，**直到 `ok == true`** 才算开发完成。
每个 `issue` 的 `message` 都包含修复提示，直接照做即可。

### smoke — 运行时冒烟（可选，验证实际运行）
```json
{ "action": "smoke", "id": "my-tool" }
```
启动打包后的 exe，捕获 JS 运行时错误（`onerror` / `unhandledrejection`）和
页面挂载状态。返回 `{success, message, smoke:{ok, ready, errors, detail}}`。
`errors` 中的 `message` 包含具体 JS 错误信息和行号，方便修复。

### list — 列出所有应用
```json
{ "action": "list" }
```
扫描 `custom-apps/` 目录，返回所有有效应用。每个应用子目录必须包含
`manifest.json` 和 `index.html`。

### get / delete / run
```json
{ "action": "get", "id": "my-tool" }
{ "action": "delete", "id": "my-tool" }
```

## 开发流程（★重要★）

1. **理解需求**：分析用户想要什么样的桌面应用
2. **设计 manifest**：确定 `id`/`name`/`window_config`/`permissions`
3. **编写 index.html**：实现 UI + 通过 `rpc()` 调用系统能力
4. **create**：用 `custom_app_manager` 的 `create` action（manifest 以 JSON 字符串传递）
5. **★ validate（必须）★**：调 `validate`，`ok == false` 则按 `issues` 修复后 `update` 重试，
   直到 `ok == true`。**这一步是闭环纠错的关键**
6. **（可选）smoke**：想确认「打包后能跑 / 不白屏」时调 `smoke`
7. **package + run**：打包并运行，让用户看到最终效果

## 纠错闭环

验证失败时，每个 `issue` 的 `message` 都包含：
- **问题描述**：什么错了
- **修复提示**：怎么改

常见错误及修复：
| 错误 | 修复 |
|------|------|
| `缺少 index.html` | `update` 时传 `index_html` |
| `缺少必填字段 \`id\`` | `update` 时在 `manifest` 中补上 |
| `<script> 标签未闭合` | 检查 HTML，补上 `</script>` |
| `方法 'exec' 未声明权限` | `update` 时在 `manifest.permissions` 中添加 `"exec"` |
| `JS 括号不匹配` | 检查 JS 代码的括号闭合 |
| `未发现 RPC 调用` | 在 JS 中使用 `rpc('方法名', {参数})` 调用桥接 |
| `参数解析失败: missing field` | manifest JSON 缺少必填字段（`id` 或 `name`） |
| `参数解析失败: expected struct` | 嵌套对象被传成了字符串——确认 manifest 是 JSON 对象不是 XML 子标签 |

## 最佳实践

- **所有 rpc() 调用检查 r.ok**：`if (!r.ok) { 显示 r.error }`
- **用 dialog 让用户选文件**：不要硬编码路径
- **用 shell 执行一行命令**：比 exec 更方便，不需要拆分 cmd+args
- **用 fs_mkdir 确保目录存在**：写文件前先创建父目录
- **UI 用中文**：面向中国用户
- **传 icon_data**：base64 PNG（256/512 正方形），让应用有独特图标
- **自包含**：所有 JS/CSS 内联在 index.html 中，不依赖外部 CDN 或文件
- **manifest 传 JSON 字符串**：在 XML 工具调用中，`manifest`/`window_config`/`permissions`/`tags` 等嵌套参数必须以 JSON 文本作为 `<parameter>` 内容，不能用 XML 子标签

## 常见问题

**Q: 能用 npm 包 / React 吗？**
A: `index.html` 是纯前端页面，可内联任意 JS。需要 React 可以用 CDN 引入，
或自行用 esbuild/webpack 打包后内联。宿主不替你打包。

**Q: 调用 rpc 报 "permission denied"？**
A: 错误信息会告诉你需要添加哪个权限。在 `manifest.permissions` 里补上即可，
然后用 `update` action 更新。

**Q: 如何执行外部命令？**
A: 用 `rpc('exec', {cmd: 'python', args: ['script.py']})` 或
`rpc('shell', {command: 'python script.py'})`。`shell` 更简单，一行搞定。

**Q: 如何分发独立 exe？**
A: `package` 产出的 exe 自包含 html + manifest（尾部 payload），仅依赖系统 WebView2，
可拷贝到任何 Windows 机器直接运行。

**Q: 调用 create 报 "参数解析失败"？**
A: 这几乎总是因为 `manifest` 参数没有以 JSON 字符串传递。在 XML 工具调用格式中，
`<parameter name="manifest">` 的内容必须是合法 JSON 对象文本（如 `{"id":"x",...}`），
不能是 XML 子标签嵌套（如 `<manifest><id>x</id></manifest>`）。详见上方"工具调用格式"章节。
