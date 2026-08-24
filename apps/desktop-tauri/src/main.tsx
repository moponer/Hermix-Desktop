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
import "./styles/components.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
      <ToastContainer />
    </ErrorBoundary>
  </React.StrictMode>
);

// 不甩活验证：每次构建时把当前 commit hash 打到 console，
// 主人刷新 devtools 后搜 "hermix-ui build" 能立刻看到当前运行的是哪次 commit。
// 比让主人贴日志更直接——他自己能看到。
if (import.meta.env.DEV) {
  // eslint-disable-next-line no-console
  console.info(
    `%c[hermix-ui build]%c running commit 84e229be (审批 ref race 双路 fallback, 2026-08-24)`,
    "color:#1A1A1A;font-weight:600;background:#F0E68C;padding:2px 6px;border-radius:3px",
    "color:inherit",
  );
}

// Remove the splash screen only after the backend health check passes.
// 不依赖 Tauri 端 reqwest 探测 emit 的 `hermix:server-ready` 事件（该事件
// 实测经常不触发/晚到，导致后端已就绪仍弹"启动超时"误报）。改为**前端
// 自己 fetch /health**——浏览器 fetch 走 WebView2 网络栈，与 curl 行为一致，
// 后端 listening 即 200，判定可靠。若 fetch 失败则继续轮询直到 20s，仍失败
// 才降级提示（非红框）。
const HEALTH_URL = "http://127.0.0.1:3000/health";
const HEALTH_TIMEOUT_MS = 20_000;
const HEALTH_POLL_MS = 500;

function dismissSplash() {
  const splash = document.getElementById("splash");
  if (splash) {
    splash.classList.add("hide");
    setTimeout(() => splash.remove(), 400);
  }
}

let dismissed = false;
function safeDismiss(timedOut = false) {
  if (dismissed) return;
  dismissed = true;
  dismissSplash();
  if (timedOut) {
    // 20s 内 /health 仍不通才算真失败（不是误报）。
    // 注意：后端 listening 后 /health 一定 200（curl 已验证），
    // 走到这里意味着 hermix.exe 真的没起来 / 端口被占 / 崩溃。
    toastWarning("后端服务未就绪（20s）。如果一直无法使用，请尝试重启。");
  }
}

// 前端自检：fetch /health 成功即 dismiss splash（无 toast）。
// 与 Tauri 端 server-ready 事件并存——两者谁先到谁 dismiss，互不冲突。
async function pollHealth() {
  const deadline = Date.now() + HEALTH_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      // no-cors 模式下 fetch 能 resolve 即说明服务可达
      // （后端无 CORS 头，普通模式会被浏览器拦截，必须 no-cors —— 与 index.html 一致）。
      await fetch(HEALTH_URL, { mode: "no-cors" });
      safeDismiss(false);
      return;
    } catch {
      // 服务未就绪（连接被拒 / 超时），继续轮询
    }
    await new Promise((r) => setTimeout(r, HEALTH_POLL_MS));
  }
  safeDismiss(true);
}
pollHealth();

// Tauri 端 server-ready 事件（旧机制，保留兼容）：收到即 dismiss。
listen("hermix:server-ready", () => {
  setTimeout(() => safeDismiss(false), 600);
});
