import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { PinWindow } from "./PinWindow";
import "./styles.css";
// 说明 + 系统资源面板：与主应用同源但独立打包 styles.css 之后的 .tb-brand 与 .dot-… 标记。
import "./system-panel.css";

// 钉住的预览浮窗（label 以 "pin-" 开头）走**轻量只读入口**：不渲染完整
// App（资源管理器 / AI 栏 / 工具条全不要），只渲染被钉住的那一个预览。
// 路径由 Rust 侧放在 query 里（`index.html?pin=<编码后的路径>`）。
const pinParam = new URLSearchParams(window.location.search).get("pin");

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {pinParam ? <PinWindow path={pinParam} /> : <App />}
  </React.StrictMode>,
);
