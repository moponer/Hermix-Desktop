// 当前窗口的 label（前端判「我是主窗口还是钉住浮窗」的唯一依据）。
//
// 为什么用 label 而不是 URL 参数：Tauri 的资源服务对**带 query 的地址**
// 找不到前端资源 → 整窗空白（2026-10-21 实测）。label 是运行期就有的
// 权威身份，不进 URL，没有这个问题。
//
// `getCurrentWindow()` 是纯 JS 调用（读的是注入的内部元数据，不发 IPC），
// 所以它不会因为 capability 或窗口类型而失败。

import { getCurrentWindow } from "@tauri-apps/api/window";

export function currentWindowLabel(): string {
  try {
    return getCurrentWindow().label ?? "";
  } catch {
    // 不在 Tauri 运行时里（例如用 vite dev server 单独开页面）→ 当主窗口。
    return "";
  }
}
