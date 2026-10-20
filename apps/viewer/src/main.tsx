import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { App } from "./App";
import { PinWindow } from "./PinWindow";
import "./styles.css";
// 说明 + 系统资源面板：与主应用同源但独立打包 styles.css 之后的 .tb-brand 与 .dot-… 标记。
import "./system-panel.css";

// 钉住的预览浮窗（label 以 "pin-" 开头）走**轻量只读入口**：不渲染完整
// App（资源管理器 / AI 栏 / 工具条全不要），只渲染被钉住的那一个预览。
// 被钉的路径向 Rust 问（label → 路径的映射在 Rust 侧）。
const isPin = getCurrentWebviewWindow().label.startsWith("pin-");

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isPin ? <PinWindow /> : <App />}
  </React.StrictMode>,
);
