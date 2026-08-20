# 自定义应用导致主窗口刷新的根因分析与彻底隔离方案

## 一、现象
`tauri dev` 下，点击 `local-music` 自定义应用（hx-shell 应用）的音量滑块，
**主 Tauri 窗口被整体刷新**，控制台连续打印：

```
[TAURI] Couldn't find callback id 3993113035. This might happen when the app is reloaded while Rust is running an asynchronous operation.
```

这条警告来自**主窗口的 Tauri IPC 桥**——说明主窗口的 webview 上下文被重新加载，
导致 JS 侧注册的异步回调（流式对话 WS、文件操作等）在 Rust 侧回调用时找不到对应句柄。

## 二、根因链（端到端，已实锤）

| 环节 | 事实 | 证据 |
|---|---|---|
| 1. 自定义 app 用相对路径存状态 | `local-music` 调 `window.hx.writeFile('local-music.state.v4', json)`，`STORE_KEY='local-music.state.v4'` 是**裸文件名** | `index.html` 内 `saveState()` / `STORE_KEY` |
| 2. hx-shell 按 cwd 解析相对路径 | `hx_write_file` 用 `std::fs::write(path, …)`；Rust 的相对路径按**进程 cwd** 解析 | `apps/hx-shell/src/main.rs` |
| 3. 主程序 spawn 时未隔离 cwd | `run_plugin` (`lib.rs:547`) 与 `run_dev` (`hx_packager.rs:359`) 都是 `Command::new(…).spawn()`，**没有 `.current_dir()`** | 源码确认；仅 smoke 测试 (`smoke.rs:68`) 设了 |
| 4. 子进程 cwd 落到主项目源码目录 | 父进程（`tauri dev`）的 cwd = `src-tauri/`，子进程继承之 | — |
| 5. 状态文件泄漏进被监听目录 | `local-music.state.v3/v4` 出现在 `src-tauri/` | `ls src-tauri/*.state*` |
| 6. 文件监听触发主窗口重建 | Tauri dev 监听 `src-tauri/**`（排除 target/gen/…），新文件触发重建 | Tauri v2 dev watcher 默认行为 |

**对照证据**：应用自己的目录 `~/.hermix/custom-apps/local-music/` 里是 `local-music.state.v1/v2`
（打包 exe 运行时 cwd=自身目录 → 正确落点）；而 `src-tauri/` 里是 `v3/v4`
（dev / `run_plugin` 继承 `src-tauri/` cwd → 泄漏）。位置差异直接证明了 cwd 是元凶。

> 注意：自定义 app 本身是**独立进程**（own wry/WebView2，UserDataFolder 已是
> `local-music.exe.WebView2`，与主程序完全隔离），它的 JS 代码也没有 `window.top` /
> `location.reload` / `postMessage` 去碰主窗口。问题**不在 app 代码，而在主程序
> spawn 它时没把 cwd 关进"笼子"**。

## 三、修复（两层，均已落地、编译通过、壳已重新部署）

### 层 A — cwd 隔离（决定性，旧壳也能生效）
- `crates/hermix-plugin/src/lib.rs` `run_plugin`：
  ```rust
  Command::new(&exe_path).arg("--app-dir").arg(&entry.dir).current_dir(&entry.dir).spawn()
  ```
- `crates/hermix-plugin/src/hx_packager.rs` `run_dev`：
  ```rust
  Command::new(&self.shell_path).arg("--html").arg(&html_path)
    .arg("--app-dir").arg(app_dir).current_dir(app_dir).spawn()
  ```
  子进程 cwd 恒为应用目录，**永不再继承 `src-tauri/`** → 相对路径状态文件落到
  `~/.hermix/custom-apps/<id>/`，不再触发主窗口重建。

### 层 B — hx-shell 相对路径沙箱（彻底防御，需新壳）
- `apps/hx-shell/src/main.rs`：新增 `--app-dir` 解析（兜底 `--html` 父目录 / 当前 exe 父目录），
  存为 `APP_BASE_DIR`（`OnceLock`）。
- 新增 `resolve_app_path()`：绝对路径原样返回；相对路径**join 到 app-dir**，绝不依赖 cwd。
- `hx_write_file` / `hx_read_file` / `hx_write_file_bytes` / `hx_read_file_bytes` /
  `hx_shell_fs_write` / `hx_shell_fs_delete` 全部改用 `resolve_app_path()`。
- 效果：即便未来某条 spawn 路径忘记设 cwd，相对路径也只会落在应用目录，不可能逃逸到主项目。

### 清理
- 删除已泄漏的 `src-tauri/local-music.state.v3` / `local-music.state.v4`。

### 部署
- `cargo build` hx-shell → 部署到：
  - `~/.hermix/loader/hx-shell.exe`（HERMIX_HOME，resolve_hx_shell_path 首选）
  - `apps/desktop-tauri/src-tauri/resources/loader/hx-shell.exe`（打包内置）
- 旧壳已备份为 `.bak-20260814`（可作回滚）。`ensure_shell_deployed` 从 `resources/loader`
  复制到 HERMIX_HOME，两处现已一致，不会被降级。

## 四、验证
- `cargo check -p hermix-plugin` → EXIT=0
- `cargo check` / `cargo build` hx-shell → EXIT=0（仅 1 个无关 pre-existing warning:`bit_depth`）
- **运行时**：重启 `tauri dev` 后点音量滑块，主窗口不应再刷新；状态文件应出现在
  `~/.hermix/custom-apps/local-music/` 而非 `src-tauri/`。

## 五、给未来的架构铁律（自定义 app 隔离契约）
1. **独立进程** —— 已有（own wry/WebView2）。
2. **cwd 锁应用目录** —— 所有 spawn 子进程显式 `.current_dir(app_dir)`，**绝不继承父 cwd**。
3. **文件写入解析到 app 目录** —— 相对路径 join 到 `--app-dir`，不依赖 cwd（已做）。
4. **WebView2 UserDataFolder 各自隔离** —— 已有（`<id>.exe.WebView2`）。
5. **无共享 JS / IPC 上下文** —— 仅保留显式 spawn/exit 握手（已有）。
6. **Defensive watcher** —— 主项目文件监听排除自定义 app 运行时产物（可选加固）。

> 一句话：**任何 spawn 子进程都显式 `current_dir`，绝不继承父 cwd；相对路径一律解析到
> 应用私有目录。** 这是"自定义应用永不影响主项目"的根本保证。
