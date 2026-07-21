# Custom App Shell

预编译的 Tauri 壳，用于自定义应用系统。

## 构建方式 (仅开发者需要执行一次)

```bash
cd apps/custom-app-shell/src-tauri
cargo tauri build --release
```

产出 `app-shell.exe` (~5MB)，会被复制到主应用的 `resources/` 目录。

## 工作原理

1. 壳启动时读取 `resources/manifest.json` 获取窗口配置
2. `index.html` (app-loader) 动态加载 `resources/ui.bundle.js`
3. UI 通过 `invoke("shell_*")` 调用壳提供的 IPC 命令
4. 后端代码 (`backend.bundle.js`) 在 WebView 中运行

## 提供的 IPC 命令

| 命令 | 说明 |
|------|------|
| `shell_fs_read` | 读取文件 |
| `shell_fs_read_bytes` | 读取文件 (字节) |
| `shell_fs_write` | 写入文件 |
| `shell_fs_write_bytes` | 写入文件 (字节) |
| `shell_fs_list` | 列出目录 |
| `shell_fs_exists` | 检查路径存在 |
| `shell_fs_delete` | 删除文件/目录 |
| `shell_fs_mkdir` | 创建目录 |
| `shell_fs_move` | 移动/重命名 |
| `shell_fs_copy` | 复制 |
| `shell_exec` | 执行外部命令 |
| `shell_fetch` | HTTP 请求 |
| `shell_dialog_open` | 打开文件对话框 |
| `shell_dialog_save` | 保存文件对话框 |
| `shell_path_join` | 路径拼接 |
| `shell_path_basename` | 获取文件名 |
| `shell_path_dirname` | 获取目录名 |
| `shell_path_extname` | 获取扩展名 |
| `shell_env_get` | 获取环境变量 |
| `shell_env_list` | 列出所有环境变量 |
| `shell_temp_dir` | 获取临时目录 |
| `shell_temp_file` | 创建临时文件 |
