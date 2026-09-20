import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import * as api from "./api";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { toastWarning } from "./hooks/useToast";
import { ToastContainer } from "./components/ToastContainer";
import "./styles/globals.css";
import "./styles/inline-thinking-dot.css";
// components.css ships a project-wide `.h-enter` animation shorthand;
// `.h-leaving` (defined in `inline-approval-card.css`) must load after
// it so the ACUI approval card's leave animation wins for elements
// carrying both classes.
import "./styles/components.css";
import "./styles/workbench.css";
import "./styles/inline-approval-card.css";
import { restoreScale } from "./utils/uiScale";

// 恢复上次界面缩放。必须早于 React 渲染：否则首帧按 100% 布局、第二帧才跳到
// 用户设定值，会看到一次明显的整体缩放跳变。
restoreScale();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
      <ToastContainer />
    </ErrorBoundary>
  </React.StrictMode>,
);

// 不甩活验证：每次构建时把当前 commit hash 打到 console，
// 主人刷新 devtools 后搜 "hermix-ui build" 能立刻看到当前运行的是哪次 commit。
// 比让主人贴日志更直接——他自己能看到。
if (import.meta.env.DEV) {
  // eslint-disable-next-line no-console
    console.info(
    `%c[hermix-ui build]%c running commit d1eeb71c (审批 race flushSync 根治, 2026-08-24)`,
    "color:#1A1A1A;font-weight:600;background:#F0E68C;padding:2px 6px;border-radius:3px",
    "color:inherit",
  );
}

// Remove the splash screen once the backend is ready.
//
// Stdio transport: instead of polling an HTTP `/health` URL (there
// is no HTTP — the bridge owns all I/O), we round-trip the bridge's
// `backend_http_request("GET", "/health")` until it returns 200.
// `apiInvoke` is the same chokepoint every other call site uses
// (see `../api/rest.ts`), so a future transport swap touches one
// file, not five.
//
// We also accept the Tauri-side `hermix:server-ready` event so the
// splash can be dismissed the moment the sidecar's startup probe
// succeeds — the two paths race to whoever fires first.
const HEALTH_TIMEOUT_MS = 20_000;
const HEALTH_POLL_MS = 500;
// 终极兜底：无论 health 探测 / server-ready 事件是否到达，到时无条件关掉
// splash。防止 `invoke("backend_ready")` 在异常情况下永久 pending 把整段
// 关闭逻辑钉死，导致界面永远卡在启动页。splash 只是加载遮罩，真正后端离线
// 状态由 BackendStatus / ConnectionIndicator 呈现，关掉遮罩不丢任何信息。
const HARD_FALLBACK_MS = 22_000;

function dismissSplash() {
  // 先停掉 CSS 自推进动画并补满，再淡出——否则进度条停在中途就消失。
  const bar = document.getElementById("sp-progress-bar");
  if (bar) {
    bar.classList.remove("animating");
    bar.style.width = "100%";
  }
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
    // pollHealth 20s 内仍未拿到 backend_ready=true 算真失败（hermix.exe
    // 没起来 / spawn 失败 / bridge 未初始化）。splash 已关，离线状态交由
    // 顶栏连接指示器呈现。
    toastWarning("后端服务未就绪（20s）。如果一直无法使用，请尝试重启。");
  }
}

// 单次 health 探测，自带超时 race：即使 `invoke("backend_ready")` 永久
// pending，本次探测也会在 timeoutMs 后判定为 false，轮询不会被单个 hung
// invoke 拖死（否则 while 循环永远停在 await 上，20s 兜底也到不了）。
function checkOnce(timeoutMs: number): Promise<boolean> {
  return Promise.race([
    api.checkBackendHealth(),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(false), timeoutMs)),
  ]);
}

async function pollHealth() {
  const deadline = Date.now() + HEALTH_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      if (await checkOnce(2_000)) {
        safeDismiss(false);
        return;
      }
    } catch {
      // bridge not yet initialised or round-trip failed — keep polling.
    }
    await new Promise((r) => setTimeout(r, HEALTH_POLL_MS));
  }
  safeDismiss(true);
}

// 终极兜底：先于所有探测注册，保证 splash 永不永久卡住。
setTimeout(() => safeDismiss(false), HARD_FALLBACK_MS);

pollHealth();

// Tauri 端 server-ready 事件（旧机制，保留兼容）：收到即 dismiss。
listen("hermix:server-ready", () => {
  setTimeout(() => safeDismiss(false), 600);
});

// 启动阶段文案：Rust setup 的每个真实步骤都会 emit `hermix:boot-stage`。
// setup 可能早于前端 listen，所以挂载后回查一次当前阶段补上丢失的事件。
function setSplashStage(text: string) {
  const el = document.getElementById("sp-stage-text");
  if (el && text) el.textContent = text;
}

listen<string>("hermix:boot-stage", (event) => {
  setSplashStage(event.payload);
  // Rust 端最后一步会 emit "服务已就绪，正在进入主界面"——收到即关。
  if (event.payload && event.payload.includes("就绪")) {
    safeDismiss(false);
  }
});

invoke<string>("boot_stage")
  .then(setSplashStage)
  .catch(() => {
    // 纯浏览器调试下没有 Tauri command，忽略即可。
  });

// 无边框窗口没有系统标题栏：启动异常卡住时，用户得能最小化/关闭。
(function bindSplashWindowControls() {
  const bind = (id: string, action: () => void) => {
    document.getElementById(id)?.addEventListener("click", action);
  };
  bind("sp-minimize", () => {
    void getCurrentWindow().minimize().catch(() => {});
  });
  bind("sp-close", () => {
    void getCurrentWindow().close().catch(() => {});
  });
})();
