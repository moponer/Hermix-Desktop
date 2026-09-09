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
import { installWorkspaceFileInterceptor } from "./lib/workspaceFileInterceptor";
import "./styles/globals.css";
import "./styles/inline-thinking-dot.css";
// components.css ships a project-wide `.h-enter` animation shorthand;
// `.h-leaving` (defined in `inline-approval-card.css`) must load after
// it so the ACUI approval card's leave animation wins for elements
// carrying both classes.
import "./styles/components.css";
import "./styles/inline-approval-card.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
      <ToastContainer />
    </ErrorBoundary>
  </React.StrictMode>,
);

// Translate `stdio://workspace-file/...` URLs (sentinels emitted by
// `api.workspaceFileUrl`) into real blob URLs by round-tripping the
// bridge. Installed once at boot — see `workspaceFileInterceptor.ts`
// for the dedup + cache rules.
installWorkspaceFileInterceptor();

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
    // 20s 内 bridge 仍未回 /health 200 算真失败。
    // bridge 初始化后 round-trip 必然成功（curl/手动 invoke 已验证），
    // 走到这里意味着 hermix.exe 真的没起来 / spawn 失败 / bridge 未初始化。
    toastWarning("后端服务未就绪（20s）。如果一直无法使用，请尝试重启。");
  }
}

async function pollHealth() {
  const deadline = Date.now() + HEALTH_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const ready = await api.checkBackendHealth();
      if (ready) {
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
