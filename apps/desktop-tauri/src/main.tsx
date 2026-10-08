import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import * as api from "./api";
import App from "./App";
import { ScreenshotOverlay } from "./components/ScreenshotOverlay";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { toastError, toastSuccess, toastWarning } from "./hooks/useToast";
import { ToastContainer } from "./components/ToastContainer";
import "./styles/globals.css";
import "./styles/screenshot-overlay.css";
import "./styles/inline-thinking-dot.css";
// 2026-09-29：`components.css` 已删除。它是 `components-*.css` 的陈旧副本，
// 两者曾同时加载且后者胜出，288 个选择器被双定义，只有旧副本赢。独有的
// 33 个规则已迁回对应模块文件（见各文件末尾「从 components.css 迁入」段）。
// `.h-leaving` 仍在 `inline-approval-card.css`，它排在最后，离场动画照常生效。
import "./styles/workbench.css";
import "./styles/inline-approval-card.css";
import { restoreScale } from "./utils/uiScale";

// 截图覆盖窗（独立 window，hash 路由分流）：全屏幕布 + 选区交互，
// 不渲染主应用。必须早于 App 挂载判断，避免覆盖窗加载整套聊天 UI。
const isOverlayWindow = window.location.hash.startsWith("#/screenshot-overlay");

if (isOverlayWindow) {
  // 覆盖窗立即移除启动遮罩：遮罩 z-index 9999、亮色主题白底，曾把覆盖窗
  // 糊成全屏白屏直到 10-20s 兜底触发。splash 生命周期整段对覆盖窗跳过。
  document.getElementById("splash")?.remove();
} else {
  // 恢复上次界面缩放。必须早于 React 渲染：否则首帧按 100% 布局、第二帧才
  // 跳到用户设定值，会看到一次明显的整体缩放跳变。覆盖窗禁用——zoom≠1 会
  // 破坏选区屏幕坐标换算。
  restoreScale();
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      {isOverlayWindow ? (
        <ScreenshotOverlay />
      ) : (
        <>
          <App />
          <ToastContainer />
        </>
      )}
    </ErrorBoundary>
  </React.StrictMode>,
);

// 不甩活验证：每次构建时把当前 commit hash 打到 console，
// 刷新 devtools 后搜 "hermix-ui build" 能立刻看到当前运行的是哪次 commit。
// 无需让使用者贴日志，自己就能看到。
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

// ── Splash 视觉状态机（方向 A：进度条 + 百分比 + 阶段列表逐项点亮） ──
// Rust setup 按真实步骤 emit `hermix:boot-stage` 文案；前端把文案映射到 4 个
// 固定阶段（index.html 的 sp-stages），推进 active/done 态并同步百分比。
// 映射依据 apps/desktop-tauri/src-tauri/src/lib.rs 的 set_boot_stage 调用序列：
//   "正在准备运行环境…"        → 阶段 0
//   "正在解包内置技能…"        → 阶段 1
//   "正在准备运行时组件…"      → 阶段 1（同属加载内置能力）
//   "正在启动本地服务…"        → 阶段 2
//   "正在等待服务就绪…"        → 阶段 2
//   "服务已就绪…"              → 阶段 3 + dismiss
const SPLASH_STAGES = [
  { text: "准备运行环境", pct: 25 },
  { text: "加载内置能力", pct: 50 },
  { text: "启动本地服务", pct: 75 },
  { text: "准备就绪", pct: 100 },
];

function stageIndexFromText(text: string): number {
  if (!text) return 0;
  if (text.includes("准备运行环境")) return 0;
  if (text.includes("解包内置技能") || text.includes("准备运行时组件")) return 1;
  if (text.includes("启动本地服务") || text.includes("等待服务就绪")) return 2;
  if (text.includes("就绪")) return 3;
  return 0; // 未知文案：保持当前阶段（不重置）
}

let currentStage = 0;
let crawlTimer: number | undefined;
let realStageArrived = false;

// 百分比平滑补间：状态机写入目标值，rAF 逐帧逼近——数字滚动而非跳变，
// 配合进度条 width 过渡同时运动，观感上"活"。
let pctTarget = 0;
let pctShown = 0;
let pctRaf = 0;
function tweenPct(target: number) {
  pctTarget = target;
  if (pctRaf) return;
  const step = () => {
    pctShown += (pctTarget - pctShown) * 0.16;
    if (Math.abs(pctTarget - pctShown) < 0.4) {
      pctShown = pctTarget;
    } else {
      pctRaf = requestAnimationFrame(step);
    }
    const bar = document.getElementById("sp-progress-bar");
    if (bar) bar.style.width = `${pctShown}%`;
    const el = document.getElementById("sp-progress-pct");
    if (el) el.textContent = String(Math.round(pctShown));
    if (pctTarget === pctShown) pctRaf = 0;
  };
  pctRaf = requestAnimationFrame(step);
}

/** 爬行兜底：exe 侧还没有 boot-stage 广播（旧 Rust / command 缺失）时，
 *  进度条匀速爬到 90% 停住，不再是死 0%。真实阶段一旦到达立即停爬让位。 */
function startProgressCrawl() {
  if (realStageArrived || crawlTimer !== undefined) return;
  crawlTimer = window.setInterval(() => {
    if (realStageArrived) {
      window.clearInterval(crawlTimer);
      crawlTimer = undefined;
      return;
    }
    if (pctShown >= 90) return;
    // +2% / 320ms：前段爬快些（ease-out 观感），90% 停住等真实事件。
    tweenPct(Math.min(90, pctShown + 2));
  }, 320);
}

function advanceSplashStage(text: string) {
  const idx = stageIndexFromText(text);
  if (idx < currentStage) return; // 只前进不后退（事件乱序保险）
  if (!realStageArrived) {
    realStageArrived = true;
    if (crawlTimer !== undefined) {
      window.clearInterval(crawlTimer);
      crawlTimer = undefined;
    }
  }
  currentStage = idx;
  const stage = SPLASH_STAGES[idx];
  tweenPct(stage.pct);
  const textEl = document.getElementById("sp-progress-text");
  if (textEl) textEl.textContent = stage.text;
  // 阶段列表逐项点亮：done < idx / active === idx
  document.querySelectorAll<HTMLElement>(".sp-stage-item").forEach((el) => {
    const i = Number(el.dataset.i ?? -1);
    el.classList.toggle("done", i < idx);
    el.classList.toggle("active", i === idx);
  });
}

function dismissSplash() {
  // 补满进度条 + 百分比到 100%，所有阶段点亮为 done，再淡出
  advanceSplashStage("服务已就绪，正在进入主界面");
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

// ── Splash 生命周期：仅主窗执行。覆盖窗复用本模块，但后端/启动屏逻辑
//    与它无关，跑一遍只会拖慢首帧并把白底遮罩糊在全屏画布上。──
if (!isOverlayWindow) {
  // 终极兜底：先于所有探测注册，保证 splash 永不永久卡住。
  setTimeout(() => safeDismiss(false), HARD_FALLBACK_MS);

  // 启动即动：不等 boot_stage 回查（invoke 往返有延迟，且失败路径才开爬的
  // 旧逻辑让 splash 在空窗期死在 0%）。模块加载时立刻点亮阶段 0 + 开爬行，
  // 真实 boot-stage 事件到达后 advanceSplashStage 会自然接管并停爬。
  advanceSplashStage("正在准备运行环境…");
  startProgressCrawl();

  pollHealth();

  // Tauri 端 server-ready 事件（旧机制，保留兼容）：收到即 dismiss。
  listen("hermix:server-ready", () => {
    setTimeout(() => safeDismiss(false), 600);
  });

  // 启动阶段文案：Rust setup 的每个真实步骤都会 emit `hermix:boot-stage`。
  // setup 可能早于前端 listen，所以挂载后回查一次当前阶段补上丢失的事件。
  // 收到即推进阶段；Rust 端最后一步 emit 的文案含"就绪"——此时全阶段点亮
  // 并 dismiss（与 server-ready / pollHealth 成功三路竞走，先到先关）。
  listen<string>("hermix:boot-stage", (event) => {
    advanceSplashStage(event.payload ?? "");
    if (event.payload && event.payload.includes("就绪")) {
      safeDismiss(false);
    }
  });

  // 挂载后回查当前阶段（setup 可能早于 listen，补上错过的事件）。
  // 回查失败 / 返回空（旧 exe 没有 boot_stage command）都开爬行兜底 ——
  // 修复"进度条永远 0%、启动项全灰"的死屏观感。
  invoke<string>("boot_stage")
    .then((stage) => {
      if (stage) {
        advanceSplashStage(stage);
      } else {
        startProgressCrawl();
      }
    })
    .catch(() => {
      // command 不存在（旧 exe）或纯浏览器调试：爬行兜底撑住观感。
      startProgressCrawl();
    });

  // 截图反馈：Rust 会话结果经全局事件回主窗（成功复制 / 失败原因）。
  listen<string>("hermix:screenshot-error", (event) => {
    toastError(event.payload ? `截图失败：${event.payload}` : "截图失败");
  });
  listen("hermix:screenshot-copied", () => {
    toastSuccess("截图已复制到剪贴板");
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
}
