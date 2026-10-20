import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";
// 菜单栏 + 系统资源管理弹窗的样式（须在 styles.css 之后：它覆盖 .tb-brand 布局）
import "./system-panel.css";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
