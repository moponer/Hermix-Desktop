# Decision Note: 关闭窗口 incognito 修复 WebView2 157 白屏

## Decision

`tauri.conf.json` 主窗口的 `incognito: true` 改为 `false`（2026-11-21）。

背景：WebView2 运行时自动更新到 157.0.4341.60（2026-11-19 00:09 推送）后，InPrivate 模式下加载 tauri 自定义协议（`http://tauri.localhost`）的嵌入式前端失败——窗口白屏（React 不挂载、无 JS 错误事件、无控制台输出）。0.1.20 与 0.1.21 的前端均受影响，与前端内容无关，纯属运行时兼容性回归。

诊断路径：产物 index.html 注入 onerror/unhandledrejection 钩子（嵌入 exe 后随包运行）→ 窗口白屏但无任何错误事件 → 排除 JS 崩溃 → CDP（`additionalBrowserArgs: --remote-debugging-port`）确认页面导航正常 → 网络与后端全绿 → 对照实验锁定 `incognito` 与运行时 157 的组合。

## Alternatives considered

- **回滚 WebView2 运行时**：运行时由 EdgeUpdate 自动维护，回滚会被再次覆盖，不可持续。
- **等 tauri/wry 修复**：本机无法复现最小化 case 上报，阻塞交付。
- **保留 incognito 并换数据目录**：InPrivate 的本质（不落盘）与白屏根因相同，换目录无效（已实测：干净数据目录仍白屏）。

## Consequences

- WebView 数据目录回到 `%LOCALAPPDATA%\com.hermix.desktop\EBWebView`（落盘持久化）——会话内 `localStorage`/缓存跨启动保留，属正常桌面应用行为。
- 白屏消失，暗色主题与首启动种子（SOUL.md）路径不变。
- 若后续 WebView2 再出回归，可临时用 `additionalBrowserArgs` 加 `--disable-features=<feature>` 应对（本次诊断即用此通道）。
