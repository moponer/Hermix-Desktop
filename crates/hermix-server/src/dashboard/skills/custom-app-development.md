---
name: custom-app-development
description: >
  自定义应用开发助手。当用户要求创建桌面应用、桌面工具、独立程序、
  窗口应用、打包成 EXE 的程序时使用。通过 custom_app_manager 工具
  创建、更新、打包、运行用户自定义桌面应用。底层是 hx-shell
  (Wry + WebView2) 模板壳 + esbuild，用户无需安装 Rust/Node。
  务必在以下场景使用：用户说"做个应用""写个桌面工具""打包成 exe"
  "做个小工具""做个窗口程序""帮我做个 app""创建自定义应用""做个独立程序"
  "做个桌面应用""make an app""create a desktop app""build a tool"
  "package as exe"；或用户描述一个需要独立窗口运行的功能需求时。
  Keywords (English, for keyword auto-routing): custom app, desktop app,
  create app, build tool, package exe, standalone application, window app,
  desktop tool, make an app.
version: 1.0.0
tags:
  - custom-app
  - desktop
  - application
  - packaging
  - exe
  - hx-shell
  - webview2
  - esbuild
  - 自定义应用
  - 桌面应用
  - 打包
  - 独立程序
  - 窗口应用
  - 做个应用
---

# Custom App Development (hx-shell + esbuild 方案)

通过对话创建用户自定义桌面应用。底层是 hx-shell (Wry 0.55 + WebView2) 模板壳 +
esbuild 转译，用户机器零依赖（Windows 10/11 自带 WebView2）。

## When to Activate

任何用户提出"做一个应用 / 写个工具 / 打包成 exe / 做个独立程序"的请求。
无论功能是什么（播放器、转换器、批处理、UI 工具），只要用户要的是
"可独立运行、用户机器零环境"的桌面应用，都走本 skill + custom_app_manager 工具。
不要让用户用别的方式实现（除非他们明确拒绝这条路径）。

## Core Concepts

- **架构**:  hx-shell 单文件壳 + WebView2 渲染 + JSON-RPC 桥 (`window.hx.*` API)。
  用户机器零依赖 (Windows 10/11 自带 WebView2)。
- **应用产物**: 一个文件夹 ($HERMIX_HOME/plugins/<id>/)，含 manifest.json + index.html
  (+ 可选 src/main.tsx 和 source_files 拆分组件)。打包后是 dist/<id>.exe 单文件。
- **两种开发模式**:
  1. **HTML 模式 (简单)**: 直接写 index_html，用 <script> 内联 JS，调用 window.hx.* 桥 API。
  2. **TypeScript/React 模式 (★推荐)**: 提供 app_source (TypeScript 入口) + 可选 source_files，
     esbuild 自动转译为 resources/js/app.js。React/ReactDOM 由打包器 UMD inline 注入为全局变量（不要 import）。
- **权限模型**:  manifest.permissions 是字符串数组（fs-read/fs-write/exec/dialog/env/temp/clipboard/network/app），
  声明后 window.hx.* 对应 API 才允许调用。validate 工具会做一致性检查。
- **打包模式**:
  1. sidecar (默认, 零依赖): 复制 hx-shell.exe + 资源 → 用户机器无需任何运行时。
  2. neu-build (需 Node.js + neu CLI): neu build 编译为 resources.neur，体积更小。

## Practical Guidance

### custom_app_manager 工具的真实 schema（★ 必须严格按这个写）

接受 9 个 action: create / update / list / get / delete / package / run / validate / smoke。
**create 是用户最常用的入口。**

工具调用必须用以下 JSON 结构（XML 工具调用时用 `<parameter name="...">{...}</parameter>` 包裹）：

```json
{
  "action": "create",
  "manifest": {
    "id": "music-player",
    "name": "本地音乐播放器",
    "version": "1.0.0",
    "description": "扫描并播放本地音乐文件",
    "icon": "🎵",
    "author": "LLM-generated",
    "category": "media",
    "tags": ["music", "player", "media"],
    "window_config": {"width": 900, "height": 600, "resizable": true, "title": "本地音乐"},
    "permissions": ["fs-read", "exec", "dialog"]
  },
  "app_source": "/* TypeScript/React 入口代码，详见下方 Working Example */",
  "source_files": {
    "components/SongList.tsx": "/* 可选，拆分的子组件 */"
  }
}
```

字段说明（**不要瞎猜字段名**）:
- `action` (string, 必填): create/update/list/get/delete/package/run/validate/smoke
- `manifest` (object, create 必填): 应用元数据，下面"manifest 字段表"逐项列
- `app_source` (string, 可选, 与 index_html 二选一): TS/React 入口，写到 src/main.tsx
- `index_html` (string, 可选): 纯 HTML 模式入口，**必须是完整 HTML 文档字符串**（不是文件路径）
- `source_files` (object, 可选): {"相对路径": "文件内容"}，拆分子组件
- `icon_data` (string, 可选, 不推荐): base64 PNG；大图易解码失败，**优先用 manifest.icon 库内图标名**
- `id` (string): 仅 update/get/delete/package/run 用
- `build_mode` (string, package 可选): "sidecar" (默认) | "neu-build"

### manifest 字段表

| 字段 | 类型 | 必填 | 默认 | 说明 |
|------|------|------|------|------|
| id | string | ✅ | - | 字母数字-_，唯一 |
| name | string | ✅ | - | 应用名 |
| version | string | 否 | "1.0.0" | 语义版本 |
| description | string | 否 | "" | 简短描述 |
| icon | string | 否 | "music" | 窗口图标 = Lucide 库内名（如 music/settings/folder），见 Gotchas 图标教学 |
| author | string | 否 | "LLM-generated" | 作者 |
| category | string | 否 | "other" | file-tools/media/productivity/developer/other |
| tags | string[] | 否 | [] | 标签 |
| window_config | object | 否 | {w:600,h:480} | width/height/resizable/title |
| permissions | string[] | 否 | [] | 权限列表（合法值见下） |

### permissions 合法值

`fs-read` / `fs-write` / `exec` / `network` / `dialog` / `env` / `temp` / `clipboard` / `app`
- 错误示例：`"permissions": ["filesystem", "read"]`  ← 俩都非法
- 正确示例：`"permissions": ["fs-read"]`
- 数组,不是字符串

### ★ Working Example: 本地音乐播放器（TS/React 模式）

用户场景：扫描用户选的目录里的 .mp3/.flac/.wav/.m4a/.ogg 文件，列出歌单，点击播放。

**关键技术点（★ 不要走弯路）**:
- **选目录**：hx-shell **没有 openDialog**。用 `exec` 跑 PowerShell FolderBrowserDialog，stdout 就是目录路径。
- **列目录**：`readDir(path)` 返回**纯名字数组** `data: string[]`（没有 isDir 字段），要逐个 `stat()` 判断 isFile。
- **播音频**：**没有 readFileBytes**。用 `fileUrl(path)` 生成 `hxfile://localhost/...` URL 直接给 `<audio src>`，WebView2 原生支持音频 seek。

app_source（写到 src/main.tsx）:

```typescript
// 全局 React/ReactDOM 由 index.html 注入，不能 import
const { useState, useRef } = React;

interface Song {
  path: string;
  name: string;
  size: number;
}

function App() {
  const [songs, setSongs] = useState<Song[]>([]);
  const [current, setCurrent] = useState<string>("");
  const [folder, setFolder] = useState<string>("");
  const audioRef = useRef<HTMLAudioElement>(null);

  // 选目录：exec 跑 PowerShell FolderBrowserDialog，stdout = 路径
  const pickFolder = async (): Promise<string> => {
    const ps = "powershell -NoProfile -Command \"Add-Type -AssemblyName System.Windows.Forms; $f = New-Object System.Windows.Forms.FolderBrowserDialog; if ($f.ShowDialog() -eq 'OK') { $f.SelectedPath }\"";
    const r = await window.hx.exec(ps, []);
    if (!r.ok) throw new Error(r.err || "选目录失败");
    const line = String(r.data.stdout || "").trim();
    const m = line.match(/[A-Za-z]:[^\r\n]+/);
    return m ? m[0] : "";
  };

  const loadFolder = async () => {
    const dir = await pickFolder();
    if (!dir) return;
    setFolder(dir);

    // readDir 返回纯名字数组；逐个 stat 判断是文件 + 音频扩展名
    const names: string[] = (await window.hx.readDir(dir)).data || [];
    const songs: Song[] = [];
    for (const name of names) {
      if (!/\.(mp3|flac|wav|m4a|ogg)$/i.test(name)) continue;
      const p = dir + "\\" + name;
      const st = await window.hx.stat(p);
      if (st.ok && st.data && st.data.isFile) {
        songs.push({ path: p, name, size: st.data.size || 0 });
      }
    }
    setSongs(songs);
  };

  const play = (path: string) => {
    // fileUrl → hxfile://localhost/... → <audio> 直接播放（支持 seek）
    setCurrent(window.hx.fileUrl(path));
    setTimeout(() => audioRef.current?.play(), 50);
  };

  return React.createElement("div", { className: "app" },
    React.createElement("header", null,
      React.createElement("h1", null, "🎵 本地音乐"),
      React.createElement("button", { onClick: loadFolder },
        folder ? "📁 " + folder.split("\\").pop() : "选择音乐文件夹"),
    ),
    React.createElement("ul", { className: "playlist" },
      songs.map((s, i) =>
        React.createElement("li", {
          key: s.path,
          className: current === window.hx.fileUrl(s.path) ? "active" : "",
          onClick: () => play(s.path),
        },
          `${i + 1}. ${s.name} (${(s.size / 1024 / 1024).toFixed(1)} MB)`,
        ),
      ),
    ),
    current && React.createElement("audio", {
      ref: audioRef, src: current, controls: true, autoPlay: true,
    }),
    songs.length === 0 && React.createElement("p", { className: "hint" },
      "点击上方按钮选择音乐文件夹"),
  );
}

const root = ReactDOM.createRoot(document.getElementById("root")!);
root.render(React.createElement(App));
```

manifest（create 的 manifest 字段）:

```json
{
  "id": "music-player",
  "name": "本地音乐",
  "icon": "🎵",
  "category": "media",
  "tags": ["music", "player"],
  "window_config": {"width": 900, "height": 600, "title": "本地音乐"},
  "permissions": ["fs-read", "exec", "dialog"]
}
```

### create 之后的工作流

1. `validate` → 不通过按 issues 改
2. 提示用户去 dashboard "应用" 页面点 "打包" (默认 sidecar)
3. `run` → 启动已打包的 exe
4. 后续代码修改 → `update` action 替换 app_source

### UI 设计规范（★ 强制遵守，主人铁律 2026-08-03）

**默认浅色主题，简约美观，有呼吸感。** 除以下两种例外外：
- **用户明确要求**（如"做个深色的""要紫色调"）
- **纯展示动效**（如播放旋转唱片、加载动画背景、渐变的封面图）

以下"AI 味"设计**一律禁止**：
- ❌ **渐变色**：按钮、图标、标题、边框不得用 `linear-gradient` / `radial-gradient` 做渐变
- ❌ **紫色系**：禁 purple / violet / indigo / fuchsia 家族（如 `#f093fb`、`#667eea`、`#764ba2`、`#f5576c` 渐变组合）
- ❌ **组件边框线堆砌**：不要给每个元素都描一道细边框（`border: 1px solid rgba(...)` 满屏），那是"组件库模板感"
- ❌ **花哨发光**：`box-shadow: 0 20px 60px rgba(紫,0.4)` 这种大阴影发光

**默认配色（浅色）**：
| 用途 | 值 |
|------|-----|
| 页面背景 | `#fafafa` |
| 卡片/面板 | `#ffffff` |
| 次级背景 | `#f4f4f5` |
| 边框（必要处） | `#e4e4e7` |
| 主文字 | `#18181b` |
| 次要文字 | `#71717a` |
| 强调色（按钮主操作） | `#4b7a5e`（墨绿） |

**布局原则**：
- 标题 / 工具栏 / 内容区 / 状态栏 分区清晰
- 卡片式，统一圆角 8-10px，间距留白充足（padding 20px+）
- 主操作按钮用**墨绿实底 + 白字**；次要操作用**浅灰底 + 深灰字**（无边框或极淡边框）
- 图标：主操作按钮必须有内联 SVG（lucide 风格 24×24，`stroke="currentColor"`），禁 emoji 当图标、禁外链图标库
- 强调文字用墨绿或深灰，不要彩色渐变

**自检清单（生成代码前过一遍）**：
1. 无渐变（grep 一下 `gradient` 应为空，除纯展示动效）
2. 无紫/蓝紫（`#f093fb` / `#667eea` / `#764ba2` / purple / violet 全无）
3. 无满屏细边框
4. 背景浅色（`#fafafa` / `#ffffff` 系）
5. 图标是 SVG 不是 emoji
6. 圆角统一、间距留白充足

### 其它 action 速查

- list: 列所有应用 (无参数)
- get: 看应用详情+源码 (需 id)
- update: 替换 app_source / index_html / manifest (需 id)
- delete: 卸载应用 (需 id)
- package: 打包成 exe (需 id, 可选 build_mode)
- run: 启动已打包应用 (需 id)
- validate: 静态校验 manifest+HTML+API+权限 (需 id)
- smoke: 先打包再启 exe 看进程存活 (需 id)

## Gotchas

- **【致命】不要用 backend_code / ui_code 字段**！这是旧 skill 残留的字段名，
  真实 schema 完全没有这俩。create 必填的是 `app_source` (TS) 或 `index_html` (HTML)。
  看到模型还在写 backend_code 立即纠正。
- **【致命】不要 import React/ReactDOM**！这两是全局变量 (由壳注入)，
  `import React from 'react'` 必然报错。用 `const { useState } = React;`。
- **【致命】API 命名**: 用 `window.hx.*`，不是 `Neutralino.*` 也不是 `shell_*`。
  **真实可用的 hx API（以壳 `getInfo()` 为准）**:
  - 文件: `writeFile(path, content)` / `readFile(path)` (文本 only) /
    `appendFile(path, content)` / `removeFile(path)` / `readDir(path)` (**返回纯名字 string[]**) /
    `stat(path)` ({size,isDir,isFile,modified}) / `createDir(path)` / `removeDir(path)`
  - 进程: `exec(cmd, args)` → {stdout, stderr, exitCode}
  - 媒体: `fileUrl(path)` → `hxfile://localhost/...` (给 `<audio src>` 直读本地文件, 支持 seek)
  - 其它: `getInfo()` (API 清单) / `showMessage(msg)`
  - **NOT SUPPORTED**: `openDialog` / `saveDialog` / `clipboardGet` / `clipboardSet` / `readFileBytes` / `getEnv` / `fetch`
    —— 不要写这些, 运行时返回 {ok:false}。
- **【致命】运行时 window.hx 注入依赖壳版本**: 打包用 `$HERMIX_HOME/loader/hx-shell.exe`。
  若该文件是旧版 (不含 `window.hx` init script), 打包出的 exe 所有 hx API 都 undefined →
  按钮点不动。**症状 = 控制台 "window.hx is undefined"**。修法: 用新版壳覆盖
  `~/.hermix/loader/hx-shell.exe` (从仓库 apps/desktop-tauri/src-tauri/resources/loader/ 拷贝),
  然后重新 package。
- **【致命】exec 找不到 powershell 等常见程序**: hx-shell 的 `exec(cmd, args)` 用
  `std::process::Command::new(cmd)`, 依赖 PATH 完整。Tauri/WebView2 子进程的 PATH 可能
  剥离了 `C:\Windows\System32\WindowsPowerShell\v1.0\`, 直接 `await hx.exec("powershell", ...)`
  会返回 `program not found`。**新壳已内置 fallback** (powershell / cmd / pwsh / where
  自动用 System32 全路径), 但**旧壳没有** → 必须用新版壳。短期绕过: app 代码用完整
  路径 `"C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"`。
- **【窗口图标】manifest.icon 用内置 Lucide 图标库名（★推荐，不要 base64）**:
  - manifest `icon` 字段 = Lucide 图标名（如 `"music"` / `"settings"` / `"folder"`），
    打包器从内置图标库（2007 个 SVG）读对应图标注入 payload，壳运行时用 resvg 渲染成窗口图标。
  - **2007 个可用图标名清单**: 见 `apps/desktop-tauri/src-tauri/resources/icons/`（文件名去掉 `.svg` 就是名字）。
  - 常见图标名：music / play / pause / skip-forward / folder / file-text / settings / search /
    download / upload / trash / edit / plus / x / check / alert-circle / image / video / mic /
    headphones / monitor / printer / camera / book-open / message-square / send / bell / star / heart
  - 旧方式 `icon_data`（base64 PNG）仍兼容，但**不推荐**：base64 大图（>50KB）会解码失败。
  - 格式: `"icon": "music"`（字符串），不要带 `.svg` 后缀，不要传 base64。
- **【致命】UI 配色反 AI 味（主人铁律）**: 默认浅色主题。**禁止**渐变按钮/图标（`linear-gradient`）、紫色系（`#f093fb`/`#667eea`/`#764ba2`/purple/violet/fuchsia）、满屏细边框线、大阴影发光。唯一例外：用户明确要求、或纯展示动效。主操作按钮 = 墨绿实底 `#4b7a5e` + 白字；背景 `#fafafa`/卡片 `#ffffff`；图标用内联 SVG 禁 emoji。详见 Practical Guidance 的 UI 设计规范 + 自检清单。
- **【常见错】tags/permissions 必须是数组**：常见 LLM 写成 `"permissions": "fs-read"`
  正确: `"permissions": ["fs-read"]`。
- **【常见错】manifest 必须传 JSON 对象，不是字符串**。
  XML 工具调用时: `<parameter name="manifest">{"id":"x","name":"y"}</parameter>`
  不是 `<parameter name="manifest"><id>x</id><name>y</name></parameter>`。
  如果用 XML 子标签，服务端会提示 "expected struct"。
- **【常见错】permissions 写错英文**: `filesystem` / `file` / `disk` / `os` 都是非法的，
  只能是 fs-read/fs-write/exec/dialog/env/temp/clipboard/network/app。
- **【常见错】audio 不能用 file:// 直读本地 MP3** (浏览器安全策略)，
  正确姿势: `window.hx.fileUrl(path)` → `hxfile://localhost/C:/...` → `<audio src>`。
  不要用 Blob/readFileBytes (该 API 不存在)。
- **【debug】create 之后跑一次 validate** 看 issues 数组，按 code 修。
  validate 比浏览器报错更直接（能告诉你 "permission missing: exec"）。
- **【注意】build_mode 默认 sidecar，零依赖；neu-build 需 Node.js + neu CLI**。
  用户机器无 Node 时，sidecar 是唯一能跑的。
- **【注意】Window width/height 单位是 px** (CSS pixel)，不要传 100% 这种相对值。
- **【注意】index_html 必须是完整 HTML 文档字符串**（含 <!DOCTYPE html>），
  不是文件路径也不是片段。

## Integration

- **与 dashboard 协作**: create 成功后，$HERMIX_HOME/plugins/<id>/ 目录出现，
  主人去 desktop-tauri 应用的 "应用" 页面能看到。点 "打包" 按钮会调
  custom_app_manager.package action。
- **与 hermix CLI 协作**: `hermix plugin list / get / package / run` 提供等效
  命令行入口。
- **与 file-tools 协作**: 创作期间可以用 read_file/write_file 查 manifest.json
  现状 (放 $HERMIX_HOME/plugins/<id>/manifest.json)。
- **与 system 能力对比**: 不要让用户去装 Tauri / Neutralino / Electron 等其他
  框架,本系统是 hx-shell 自家壳,不依赖外部运行时。
- **失败时调试链**:
  1. 看 create 错误信息 (含具体缺什么字段)
  2. create 成功但应用跑不起来 → validate 看 issues
  3. validate 通过但运行时崩 → smoke 看进程退出码和 stderr
  4. smoke 通过但 UI 错 → 在打包后的 dist/<id>.exe 上按 F12 (开发模式壳支持)

## References

- **核心源码**:
  - `crates/hermix-tools/src/tools/custom_app_manager.rs` — 工具入口 + schema 定义
  - `crates/hermix-plugin/src/api.rs` — CreateAppRequest / PluginManifest 类型
  - `crates/hermix-plugin/src/manifest.rs` — manifest 字段 + permissions→Neutralino allowlist 映射
  - `crates/hermix-plugin/src/validate.rs` — 静态校验 (L0 manifest + L1 hx API + L2 权限)
  - `crates/hermix-plugin/src/smoke.rs` — 运行时冒烟 (启 exe + 抓 stderr)
  - `crates/hermix-plugin/src/loader_packager.rs` — sidecar / neu-build 两种打包
  - `crates/hermix-plugin/src/hx_packager.rs` — hx-shell 模板壳定位
- **运行路径**: $HERMIX_HOME/plugins/<id>/{manifest.json,index.html,src/main.tsx,...}
- **打包产物**: $HERMIX_HOME/plugins/<id>/dist/<id>.exe (sidecar 模式)
- **前端入口**: apps/desktop-tauri/src/pages/CustomApps (Tauri dashboard)
- **hx-shell 工程**: 主人本机编译 (<USERPROFILE>\AppData\Local\Temp\hx-wry-shell)
