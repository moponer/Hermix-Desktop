import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { toastError } from "./hooks/useToast";
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
    // 兜底超时已对齐 Tauri 端 /health 探测 30s 窗口（lib.rs:502）。
    // 后端通常 LLM warmup ~1s + awakening tick 1 约 10s 就绪，30s 足够。
    // 真正触发到这里 = 后端启动确实有问题，不是误报。
    toastError("后端服务启动超时（30s）。应用可能无法正常工作，请尝试重启。");
  }
}

listen("hermix:server-ready", () => {
  // Small delay so the "后端已就绪" message is visible briefly
  setTimeout(() => safeDismiss(false), 600);
});

// Safety timeout: matched to Tauri-side /health probe deadline
// (lib.rs:502 sets 30s). 之前的 15s 与后端 30s 探测窗口不匹配，
// 误报"启动超时"实际后端已连上（LLM warmup 1s、awakening tick 1~10s 都过了）。
setTimeout(() => safeDismiss(true), 30000);
