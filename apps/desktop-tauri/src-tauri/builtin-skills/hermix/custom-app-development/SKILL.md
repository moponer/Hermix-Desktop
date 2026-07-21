---
name: custom-app-development
description: >
  自定义应用开发助手。当用户要求创建桌面应用、桌面工具、独立程序、
  窗口应用、打包成 EXE 的程序时使用。通过 custom_app_manager 工具
  创建、更新、打包、运行用户自定义桌面应用。用户无需安装 Rust/Tauri/
  Node.js/Deno——系统使用预编译壳 + esbuild 实现零工具链打包。
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
  - tauri
  - webview
  - esbuild
  - 自定义应用
  - 桌面应用
  - 打包
  - 独立程序
  - 窗口应用
  - 做个应用
---

# Custom App Development (WebView-Native 方案)

通过对话创建用户自定义桌面应用。应用代码在 WebView2 (V8) 中运行，
打包时使用预编译壳 + esbuild，用户无需安装任何开发工具。

## 核心架构

```
用户对话 → LLM 生成 backend.ts + ui.tsx → custom_app_manager 工具创建
    ↓
用户点击"打包" → esbuild 打包 TS → 复制预编译壳 → 独立 EXE
    ↓
用户点击"启动" → 运行 EXE → 壳加载 manifest → 渲染 UI → 后端在 WebView 中运行
```

## 使用 custom_app_manager 工具

### action: create — 创建新应用

```json
{
  "action": "create",
  "manifest": {
    "id": "pdf-converter",
    "name": "PDF 转换器",
    "version": "1.0.0",
    "description": "将图片转换为 PDF 文件",
    "author": "LLM-generated",
    "icon": "📄",
    "icon_file": "icon.png",
    "category": "file-tools",
    "tags": ["pdf", "image", "convert"],
    "window_config": {
      "width": 800,
      "height": 600,
      "resizable": true
    },
    "permissions": ["fs-read", "fs-write"]
  },
  "backend_code": "// backend.ts 代码",
  "ui_code": "// ui.tsx 代码",
  "icon_data": "data:image/png;base64,iVBOR..."
}
```

### action: package — 打包为独立 EXE

```json
{
  "action": "package",
  "id": "pdf-converter"
}
```

### action: run — 启动已打包的应用

```json
{
  "action": "run",
  "id": "pdf-converter"
}
```

## backend.ts 规范

`backend.ts` 在 WebView2 中运行，**不能使用 Node.js API**（如 `fs`、`path`）。
所有系统操作通过 Tauri IPC (`invoke`) 完成。

### 可用 API

| 功能 | 调用方式 |
|------|----------|
| 读取文件 | `await invoke("shell_fs_read", { path })` |
| 写入文件 | `await invoke("shell_fs_write", { path, content })` |
| 读取文件(字节) | `await invoke("shell_fs_read_bytes", { path })` |
| 写入文件(字节) | `await invoke("shell_fs_write_bytes", { path, content })` |
| 列出目录 | `await invoke("shell_fs_list", { dir })` |
| 检查路径存在 | `await invoke("shell_fs_exists", { path })` |
| 删除文件/目录 | `await invoke("shell_fs_delete", { path })` |
| 创建目录 | `await invoke("shell_fs_mkdir", { path })` |
| 移动/重命名 | `await invoke("shell_fs_move", { from, to })` |
| 复制 | `await invoke("shell_fs_copy", { from, to })` |
| 执行外部命令 | `await invoke("shell_exec", { cmd, args, cwd, timeoutSecs })` |
| HTTP 请求 | `await invoke("shell_fetch", { url, method, headers, body })` |
| 打开文件对话框 | `await invoke("shell_dialog_open", { title, filters })` |
| 保存文件对话框 | `await invoke("shell_dialog_save", { title, defaultName })` |
| 路径拼接 | `await invoke("shell_path_join", { parts })` |
| 获取文件名 | `await invoke("shell_path_basename", { path })` |
| 获取目录名 | `await invoke("shell_path_dirname", { path })` |
| 获取扩展名 | `await invoke("shell_path_extname", { path })` |
| 获取环境变量 | `await invoke("shell_env_get", { key })` |
| 列出环境变量 | `await invoke("shell_env_list")` |
| 获取临时目录 | `await invoke("shell_temp_dir")` |
| 创建临时文件 | `await invoke("shell_temp_file", { prefix, suffix })` |

### backend.ts 模板

```typescript
// backend.ts — 在 WebView2 中运行，使用 invoke 进行系统操作

// 导出 execute 函数，UI 通过 window.__APP_BACKEND__.execute(params) 调用
export async function execute(params: any): Promise<any> {
  // 示例: 读取文件内容
  const content = await invoke("shell_fs_read", { path: params.filePath });
  return { success: true, content };
}

// 如果需要长时间运行的后台任务，可以使用 async function
export async function processFiles(dir: string): Promise<{ count: number }> {
  const entries = await invoke("shell_fs_list", { dir });
  let count = 0;
  for (const entry of entries) {
    if (!entry.is_dir) {
      count++;
      // 处理文件...
    }
  }
  return { count };
}
```

**重要**: `invoke` 是全局可用的（由 Tauri shell 注入），无需 import。

## ui.tsx 规范

`ui.tsx` 是 React 组件，会被 esbuild 打包。必须默认导出一个 React 组件。

### ui.tsx 模板

```tsx
// ui.tsx — React 组件，在 WebView 中渲染
import { useState, useEffect } from "react";

export default function App() {
  const [result, setResult] = useState<string>("");
  const [loading, setLoading] = useState(false);

  const handleAction = async () => {
    setLoading(true);
    try {
      // 调用后端
      const backend = (window as any).__APP_BACKEND__;
      const res = await backend.execute({ action: "process" });
      setResult(JSON.stringify(res, null, 2));
    } catch (e) {
      setResult(`错误: ${e}`);
    } finally {
      setLoading(false);
    }
  };

  // 直接调用 shell IPC
  const readFile = async () => {
    const path = await invoke("shell_dialog_open", {
      title: "选择文件",
    });
    if (path) {
      const content = await invoke("shell_fs_read", { path });
      setResult(content);
    }
  };

  return (
    <div style={{ padding: "20px", fontFamily: "system-ui" }}>
      <h1>我的应用</h1>
      <button onClick={handleAction} disabled={loading}>
        {loading ? "处理中..." : "执行操作"}
      </button>
      <button onClick={readFile}>选择文件</button>
      {result && (
        <pre style={{
          marginTop: "20px",
          padding: "12px",
          background: "#1a1a2e",
          color: "#e0e0e0",
          borderRadius: "8px",
          overflow: "auto",
          maxHeight: "400px",
        }}>
          {result}
        </pre>
      )}
    </div>
  );
}
```

**重要**: 
- `invoke` 全局可用，无需 import
- React 和 ReactDOM 会被 esbuild 打包进 ui.bundle.js
- CSS 变量可在 `manifest.json` 的 `theme` 字段中定义

## manifest.json 规范

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | string | 是 | 唯一标识符 (字母数字-_) |
| `name` | string | 是 | 应用名称 |
| `version` | string | 是 | 语义版本号 |
| `description` | string | 是 | 简短描述 |
| `long_description` | string | 否 | 详细描述 |
| `author` | string | 是 | 作者 |
| `icon` | string | 是 | Emoji 图标 |
| `icon_file` | string | 否 | 自定义图标文件名 (如 "icon.png")，当通过 icon_data 传入时会自动设置 |
| `category` | string | 否 | 分类 (默认 "other") |
| `tags` | string[] | 否 | 标签 |
| `window_config.width` | number | 否 | 窗口宽度 (默认 600) |
| `window_config.height` | number | 否 | 窗口高度 (默认 480) |
| `window_config.resizable` | boolean | 否 | 可调整大小 (默认 true) |
| `permissions` | string[] | 否 | 权限列表 |

## 开发流程

1. **理解需求**: 仔细分析用户想要什么样的桌面应用
2. **设计 manifest**: 确定应用 ID、名称、窗口大小、权限
3. **编写 backend.ts**: 实现业务逻辑，使用 `invoke` 进行系统操作
4. **编写 ui.tsx**: 实现 React UI，调用后端或直接使用 `invoke`
5. **创建应用**: 使用 `custom_app_manager` 的 `create` action
6. **提示用户打包**: 告诉用户在"应用"页面点击"打包"按钮
7. **提示用户运行**: 打包完成后点击"启动"按钮

## 最佳实践

- **保持简洁**: 每个应用专注解决一个问题
- **错误处理**: backend.ts 中所有 `invoke` 调用都要 try-catch
- **用户体验**: UI 中显示加载状态和错误信息
- **文件操作**: 使用 `shell_dialog_open` 让用户选择文件，不要硬编码路径
- **中文 UI**: 面向中国用户，UI 文本使用中文
- **合理权限**: 只声明实际需要的权限
- **自定义图标**: 为每个应用提供 `icon_data` (base64 PNG) 让其在任务栏和窗口标题栏上有独特图标，推荐 256x256 或 512x512 的正方形 PNG

## 常见问题

**Q: backend.ts 可以使用 npm 包吗？**
A: 可以。esbuild 会打包所有 import 的 npm 包。但注意：
   - 不能使用 Node.js 内置模块 (fs, path, http 等)
   - 不能使用依赖 Node.js API 的包 (如 `node-fetch`)
   - 推荐使用浏览器兼容的包

**Q: 如何存储应用数据？**
A: 使用 `shell_temp_dir` 获取临时目录，或使用 `shell_env_get` 获取
   `HERMIX_HOME` 环境变量后在其中创建数据目录。

**Q: 应用可以联网吗？**
A: 可以。使用 `shell_fetch` 发起 HTTP 请求，或直接使用浏览器内置的 `fetch`。
   `fetch` 在 WebView 中原生可用，不需要 `invoke`。

**Q: 如何调试应用？**
A: 打包后的应用在 WebView2 中运行，可以在应用窗口中按 F12 打开开发者工具
   (如果壳的开发模式已启用)。
