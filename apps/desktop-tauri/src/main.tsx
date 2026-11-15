import { IPC } from "./ipc/commands";
import React from "react";import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "./ipc";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ScreenshotOverlay } from "./components/ScreenshotOverlay";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { BackendConnectionProvider } from "./context/BackendConnectionContext";
import { storage, STORAGE_KEYS } from "./utils/storageCompat";
import { onAccentFor } from "./styles/themes/accentPalette";
import "./styles/screenshot-overlay.css";

// 截图覆盖窗（独立 window，hash 路由分流）：全屏幕布 + 选区交互，不渲染主应用。
// 覆盖窗的页面依赖集到此为止（React + ScreenshotOverlay + 一份 CSS）。
// 主应用依赖（App / api / Toast / 主 CSS）全部走主窗分支的动态 import——
// 2026-10-10 前 main.tsx 静态 import 整份主应用图，覆盖窗（dev 下走 vite
// dev server 网络拉取）把整套聊天 UI 几百个模块全拉一遍，首帧数秒；
// 配合 Rust 侧 3s 兜底 show，#splash（静态 HTML 启动屏）就被糊成全屏——
// "截图启动慢 + 全屏样式"的回归根因。覆盖窗独立成小图后首帧毫秒级。
const isOverlayWindow = window.location.hash.startsWith("#/screenshot-overlay");

// 文件搜索小窗（独立 window，hash 路由分流）：Alt+F 全局热键唤起的
// Everything 式搜索面板。同款"独立小图"——不拉主应用图。
const isFileSearchWindow = window.location.hash.startsWith("#/file-search");

if (isOverlayWindow) {
  // 覆盖窗立即移除启动遮罩（#splash 已由 CSS data-overlay 静态隐藏，这里是
  // JS 兜底）并接管「正在截取屏幕…」占位（.so-loading 同文案无缝衔接），
  // 幕布帧就绪前一直显示，就绪后窗口才亮。
  document.getElementById("splash")?.remove();
  document.getElementById("overlay-boot")?.remove();
  applyOverlayAccent();
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <ScreenshotOverlay />
      </ErrorBoundary>
    </React.StrictMode>,
  );
} else if (isFileSearchWindow) {
  // 文件搜索小窗：启动遮罩移除（若存在），组件 mount 后自行 show。
  document.getElementById("splash")?.remove();
  void (async () => {
    const [{ FileSearchWindow }, { GlobalContextMenu }] = await Promise.all([
      import("./components/FileSearchWindow"),
      import("./components/GlobalContextMenu"),
      import("./styles/file-search.css"),
    ]);
    ReactDOM.createRoot(document.getElementById("root")!).render(
      <React.StrictMode>
        <ErrorBoundary>
          <FileSearchWindow />
          {/* 搜索窗也是桌面窗口：右键给应用菜单，不给网页菜单。 */}
          <GlobalContextMenu />
        </ErrorBoundary>
      </React.StrictMode>,
    );
  })();
} else {
  void mainEntry();
}

/**
 * 覆盖窗的强调色：主窗允许用户自选强调色（写在主窗 documentElement 的行内样式上），
 * 覆盖窗是**另一个 document**，拿不到那份行内样式 → 从 localStorage 把用户选的值
 * 读回来，连同 on-accent 文字色一起注入（口径与主窗 useTheme 一致）。
 *
 * 不注入的后果：覆盖窗用主题默认强调色，用户自选了别的颜色时工具条主按钮色不一致。
 * 只接受 #rrggbb（主窗写入前已规范化），避免 localStorage 被写坏时注入非法值。
 */
function applyOverlayAccent() {
  try {
    const accent = storage.getItem(STORAGE_KEYS.accentColor);
    if (!accent || !/^#[0-9a-fA-F]{6}$/.test(accent)) return;
    const root = document.documentElement;
    root.style.setProperty("--color-accent", accent);
    root.style.setProperty("--color-on-accent", onAccentFor(accent));
  } catch {
    /* 读不到就用主题默认强调色 */
  }
}

/** 主窗入口：动态 import 主应用依赖（覆盖窗不加载这份图），随后挂载 + splash 状态机。 */
async function mainEntry() {
  const [AppMod, ToastContainerMod, toastMod, storageMod, scaleMod, api] =
    await Promise.all([
      import("./App"),
      import("./components/ToastContainer"),
      import("./hooks/useToast"),
      import("./utils/storageCompat"),
      import("./utils/uiScale"),
      import("./api"),
    ]);
  const App = AppMod.default;
  const ToastContainer = ToastContainerMod.ToastContainer;
  const { toastError, toastSuccess, toastWarning } = toastMod;
  const { storage, STORAGE_KEYS } = storageMod;
  const { restoreScale } = scaleMod;

  await Promise.all([
    import("./styles/globals.css"),
    import("./styles/workbench.css"),
    import("./styles/inline-approval-card.css"),
    import("./styles/inline-thinking-dot.css"),
  ]);

  // 恢复上次界面缩放。必须早于 React 渲染：否则首帧按 100% 布局、第二帧才
  // 跳到用户设定值，会看到一次明显的整体缩放跳变。覆盖窗禁用——zoom≠1 会
  // 破坏选区屏幕坐标换算。
  restoreScale();

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <BackendConnectionProvider>
          <App />
          <ToastContainer />
        </BackendConnectionProvider>
      </ErrorBoundary>
    </React.StrictMode>,
  );

  // 不甩活验证：每次构建时把当前 commit hash 打到 console，
  // 刷新 devtools 后搜 "hermix-ui build" 能立刻看到当前运行的是哪次 commit。
  if (import.meta.env.DEV) {
    // eslint-disable-next-line no-console
    console.info(
      `%c[hermix-ui build]%c running commit d1eeb71c (审批 race flushSync 根治, 2026-08-24)`,
      "color:#1A1A1A;font-weight:600;background:#F0E68C;padding:2px 6px;border-radius:3px",
      "color:inherit",
    );
  }

  // 单次 health 探测，自带超时 race：即使 `invoke(IPC.backendReady)` 永久
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

  // 托盘图标建不起来：关窗拦截随之失效（点 X 直接退出），这个差异必须说出来，
  // 否则用户会以为程序"藏进了托盘"，而托盘里根本没有它。
  listen<string>("hermix:tray-error", (event) => {
    toastWarning(event.payload || "托盘图标创建失败");
  });

  // 后端**起不来**的真实原因（二进制缺失 / 被杀软拦 / 库被占）。此前启动
  // 失败完全静默，用户只能等 20s 探测超时后看到一句"未就绪"，没有任何线索。
  listen<string>("hermix:backend-error", (event) => {
    const reason = (event.payload ?? "").trim();
    toastError(reason ? `本地服务启动失败：${reason}` : "本地服务启动失败");
    safeDismiss(false);
  });

  // 上面那条监听几乎必然错过事件：`hermix:backend-error` 在 Rust setup 里 emit，
  // 而注册监听要等页面加载完 —— 事件早于监听者。挂载后回查一次启动期致命错误，
  // 把「后端未连接」背后的真实原因（二进制缺失 / 被杀软拦 / 库被占 / 启动即崩）
  // 补送到用户面前。回查失败（旧 exe 无此命令）静默。
  void invoke<{ bootError?: string }>(IPC.backendDiagnosis)
    .then((d) => {
      const reason = (d?.bootError ?? "").trim();
      if (!reason) return;
      toastError(`本地服务启动失败：${reason}`);
      safeDismiss(false);
    })
    .catch(() => {});

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
  invoke<string>(IPC.bootStage)
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

  // 截图热键被占用：Rust 已自动改用备选键，这里告诉用户"现在按的是哪个"。
  // 不提示的话，用户只会觉得"热键没反应"（这正是 2026-10-24 之前的状态：
  // 派发比较写死了 "alt+q"，而 HotKey 的 Display 是 "alt+KeyQ"，永远不匹配）。
  listen<{ active: string | null; requested: string | null; substituted: boolean }>(
    "hermix:screenshot-hotkey",
    (event) => {
      const st = event.payload;
      if (!st?.substituted) return;
      toastWarning(
        `截图热键 ${st.requested ?? ""} 被其他程序占用，已自动改用 ${st.active ?? ""}（设置 → 截图可更换）`,
      );
    },
  );

  // 回放截图配置（设置 → 图像 持久化的开关/热键）。从未配置过则跳过——
  // Rust setup 已注册默认 Alt+Q 兜底。
  try {
    const raw = storage.getItem(STORAGE_KEYS.screenshotConfig);
    if (raw) {
      const cfg = JSON.parse(raw) as { enabled: boolean; hotkey: string };
      void invoke<{ active: string | null; substituted: boolean }>(
        IPC.screenshotApplyConfig,
        { enabled: cfg.enabled, hotkey: cfg.hotkey },
      ).catch((err) => console.warn("[screenshot] 配置回放失败:", err));
    }
  } catch { /* ignore */ }

  // 回放文件搜索配置（热键/开关/过滤规则）。从未配置过则跳过——
  // Rust setup 已注册默认 Alt+F 兜底，helper 用默认过滤规则。
  try {
    const raw = localStorage.getItem("hermix.filesearch.config");
    if (raw) {
      const cfg = JSON.parse(raw) as {
        enabled: boolean;
        hotkey: string;
        filter?: Record<string, unknown>;
      };
      void invoke(IPC.fileSearchApplyConfig, { enabled: cfg.enabled, hotkey: cfg.hotkey }).catch(
        (err) => console.warn("[filesearch] 配置回放失败:", err),
      );
      if (cfg.filter) {
        void api
          .fetchJSON("/v1/filesearch/filter", {
            method: "POST",
            body: JSON.stringify(cfg.filter),
          })
          .catch((err) => console.warn("[filesearch] 过滤规则回放失败:", err));
      }
    }
  } catch { /* ignore */ }

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
const HEALTH_TIMEOUT_MS = 20_000;
const HEALTH_POLL_MS = 500;
// 终极兜底：无论 health 探测 / server-ready 事件是否到达，到时无条件关掉
// splash。防止 `invoke(IPC.backendReady)` 在异常情况下永久 pending 把整段
// 关闭逻辑钉死，导致界面永远卡在启动页。splash 只是加载遮罩，真正后端离线
// 状态由 BackendStatus / ConnectionIndicator 呈现，关掉遮罩不丢任何信息。
const HARD_FALLBACK_MS = 22_000;

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
