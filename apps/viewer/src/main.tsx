import React, { lazy, Suspense } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { currentWindowLabel } from "./windowRole";
import { startViewerControl } from "./viewerControl";
import "./styles.css";
// Notion 主题取值表（须在 styles.css 之后：它覆盖同一批令牌）
import "./notion.css";
// 菜单栏 + 系统资源管理弹窗的样式（须在 styles.css 之后）
import "./system-panel.css";

/**
 * 钉住浮窗是**同一个前端入口的另一个角色**：窗口 label 以 `pin-` 开头时只
 * 渲染轻量预览（不加载资源管理器 / AI 栏 / 工具条）。
 *
 * 为什么按 **label** 判角色而不是 URL 参数：带 query 的地址在 Tauri 的资源
 * 服务里找不到前端资源 → 整窗空白（实测）。label 是运行期就有的权威身份，
 * 不进 URL，自然没有这个问题。
 */
const isPinWindow = currentWindowLabel().startsWith("pin-");

/**
 * 钉住浮窗**懒加载**：它 import 的是 `@hermix/preview` 的 barrel，而 barrel
 * 会带进整条预览栈（PreviewSurface / pdf.js / docx-preview / xlsx）。静态
 * import 时这条链进首屏图，`index.html` 的 modulepreload 直接把 2.6MB 资源
 * 预载出来 —— 而浮窗只在用户点「钉住」时才会出现。浏览文件是主窗口的常态，
 * 不该替它付这份钱。
 */
const PinWindow = lazy(() =>
  import("./PinWindow").then((m) => ({ default: m.PinWindow })),
);

// AI 自控面：听 MCP 下发的动作 + 定时上报界面状态（只在主窗口开；浮窗是
// 轻量只读入口，不需要自控面）。
if (!isPinWindow) {
  startViewerControl();
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isPinWindow ? (
      <Suspense fallback={null}>
        <PinWindow />
      </Suspense>
    ) : (
      <App />
    )}
  </React.StrictMode>,
);
