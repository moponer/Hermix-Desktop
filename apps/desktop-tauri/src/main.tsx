import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { toastWarning } from "./hooks/useToast";
import { ToastContainer } from "./components/ToastContainer";
import "./styles/globals.css";
import "./styles/inline-thinking-dot.css";
import "./styles/inline-approval-card.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
      <ToastContainer />
    </ErrorBoundary>
  </React.StrictMode>
);

// Remove the splash screen only after the backend health check passes
// (hermix:server-ready event from Tauri) OR after a 30s safety timeout
// (matched to the Tauri-side /health probe deadline in lib.rs:502).
function dismissSplash() {
  const splash = document.getElementById("splash");
  if (splash) {
    splash.classList.add("hide");
    setTimeout(() => splash.remove(), 400);
  }
}

// Wait for real backend readiness signal
let dismissed = false;
function safeDismiss(timedOut = false) {
  if (dismissed) return;
  dismissed = true;
  dismissSplash();
  if (timedOut) {
    // 2026-08-04 改：主人实测 Tauri 端 `hermix:server-ready` 事件经常晚于
    // 后端真实 ready（log 显示 06:26:40 listening 早完成但事件没 emit），
    // 30s 兜底弹红色"启动失败"几乎 100% 误报。改为温和 warning（不再红框）
    // + 延长到 60s（让 Tauri 端 /health 探测 30s 窗口 + spawn 慢启动 都有缓冲）。
    // 注意：若真启动失败，所有 API 调用都会失败，UI 自然卡住，会暴露问题。
    toastWarning("后端启动较慢（60s），仍在准备中…");
  }
}

listen("hermix:server-ready", () => {
  // Small delay so the "后端已就绪" message is visible briefly
  setTimeout(() => safeDismiss(false), 600);
});

// Safety timeout: 主环境实测 Tauri-side 30s /health 探测常晚于后端真正 ready，
// 30s 兜底 = 误报。延长到 60s 留缓冲，且仅 warning 不 error。
setTimeout(() => safeDismiss(true), 60000);
