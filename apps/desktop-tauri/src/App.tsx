import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { flushSync } from "react-dom";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { Sidebar } from "./components/Sidebar";
import { SidebarRevealRow } from "./components/SidebarRevealRow";
import { ChatView } from "./components/ChatView";
import { ImageLightbox } from "./components/ImageLightbox";
import { RightSidebar, type RightTab } from "./components/RightSidebar";
import { TitleBar } from "./components/TitleBar";
import { SessionPickerDialog } from "./components/SessionPickerDialog";
import { useStreamChat } from "./hooks/useStreamChat";
import { AppProviders } from "./app/AppProviders";
import { renderRoutes, type RouteContext } from "./app/routes";
import { ArtifactPreviewModal } from "./app/ArtifactPreviewModal";
import { SystemConfigPanel, type ConfigCategory } from "./components/SystemConfigPanel";
import { useAppBootstrap } from "./app/useAppBootstrap";
import {
  readDraftWorkspace,
  localizeStreamError,
  classifyStreamErrorKind,
  normalizeStreamErrorKind,
  type StreamErrorKind,
  appendReasoningStep,
  appendTextStep,
  appendToolStep,
  updateToolStepStdout,
} from "./app/streamEvents";
import type { StreamPhase, TodoSnapshotItem } from "./hooks/useStreamChat";
import type { SubAgentState, SubAgentEventPayload } from "./types";
import { useResizable } from "./hooks/useResizable";
import { useWorkspaceRoot } from "./hooks/useWorkspaceRoot";
import { useArtifacts } from "./hooks/useArtifacts";
import { resetChatStateForModelSwitch, type RefLike } from "./lib/chatStateReset";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ToastContainer } from "./components/ToastContainer";
import { toastError, toastInfo } from "./hooks/useToast";
import { useAcuiState, type AcuiDecisionBus } from "./acui";
import type { Artifact } from "./utils/artifacts";
import { classifyArtifactKind } from "./utils/artifacts";
import * as api from "./api";
import { registerBuiltinsToBackend } from "./data/builtinExperts";
import type {
  NavPage,
  Session,
  ChatMessage,
  ApprovalDecision,
  QueuedMessage,
  QueuedImage,
  ChatMessageImage,
  ApprovalMessageMeta,
  ApprovalBatchMessageMeta,
  TimelineStep,
} from "./types";
// expandTimelineToMessages and detectToolError imports removed —
// tools are now tracked in the assistant's execution_timeline, not expanded
// into separate role:tool messages.
import { storage, STORAGE_KEYS, sessionKey } from "./utils/storageCompat";
import { applyScale, readScale, UI_SCALE_EVENT } from "./utils/uiScale";

const LEFT_KEY = STORAGE_KEYS.sidebarWidth;

const RIGHT_KEY = STORAGE_KEYS.rightWidth;
const RIGHT_OPEN_KEY = STORAGE_KEYS.rightOpen;
const ACTIVE_SESSION_KEY = STORAGE_KEYS.activeSession;

const LEFT_MIN = 200;
const LEFT_MAX = 360;
// 左列默认 280，给主列留出空间，使三列共存时主列更宽、工作区名称更舒展。
// 已存在会话的 localStorage 宽度不自动升级（用户拖动过的尊重保留），仅默认提高。
const LEFT_DEFAULT = 280;
// 右列默认 640，拖动范围 360-840；vw=1440 时主列 = 1440 - 200 - 640 = 800。
// 640 足以完整显示中文长标题分类（如"节假日危险品重大危险源..."），
// 480 会让这类标题被截断；640 也不挤压主列。
const RIGHT_MIN = 360;
const RIGHT_MAX = 840;
const RIGHT_DEFAULT = 640;

// 对话主列的"舒适最小宽度"。当主列宽度 ≥ MAIN_MIN_WIDTH 时，chat 气泡的
// 内容（消息头像 + 文本 + 内边距）有充裕的呼吸感，气泡不显拥挤。
// 为避免 1440 视口三列总宽溢出 (200+720+640=1560>1440)，MAIN_MIN_WIDTH 取 580；
// 主列 580 仍能装 50+ 中文字符，两端平衡。
// 三列共存临界视口: LEFT_MIN(200) + MAIN_MIN_WIDTH(580) + RIGHT_MIN(360) = 1140。
// 默认视口 1440 时: 左 200 + 主 580 + 右 640 = 1420 (剩 20 给 splitter, OK).
// 默认视口 1680 时: 左 200 + 主 840 + 右 640 = 1680.
// 用户主动想看右栏时（点 TitleBar 产物按钮且视口 < 1140），把 Tauri 窗口
// outer 宽扩到 DEFAULT_WINDOW_WIDTH (1280 = tauri.conf.json 默认)，高度保留当前 outer；
// 装下三列后 setRightOpen(true)。这与 resize 拖小窗口时"装不下就关右栏"不冲突：
// 后者是被动收缩，这里是用户主动请求更多空间。
// setSize 失败时仍 setRightOpen(true)，让 layout 走退化路径（窗口不够宽则右栏内容窄显示）。
// 默认视口 1920 时: 左 200 + 主 1080 + 右 640 = 1920.
/* 自适应 MAIN_MIN_WIDTH — 按**等效视口**动态缩小主列最小宽，避免窄窗口下
   左列 + 主列合计最小宽度超过视口：
   - 等效视口 ≥ 1400px: MAIN_MIN_WIDTH = 580（舒适宽）
   - 等效视口 1000~1399px: MAIN_MIN_WIDTH = 480（中等）
   - 等效视口 800~999px: MAIN_MIN_WIDTH = 400（紧凑）
   - 等效视口 < 800px: MAIN_MIN_WIDTH = 360（最小可读）
   同时 LEFT_MIN 也随视口降低。对应的三列共存阈值随之降到 920。

   注意「等效」二字：zoom 会改变真实可用宽度但不改变 window.innerWidth，
   所以分档前必须除以 zoom，见下方 currentZoom()。 */
function currentZoom(): number {
  const z = Number(document.documentElement.style.zoom);
  return z > 0 ? z : 1;
}

function computeMainMinWidth(vw: number): number {
  const eff = vw / currentZoom();
  if (eff >= 1400) return 580;
  if (eff >= 1000) return 480;
  if (eff >= 800) return 400;
  return 360;
}
function computeLeftMin(vw: number): number {
  const eff = vw / currentZoom();
  if (eff >= 1200) return 200;
  if (eff >= 900) return 160;
  return 120;
}

// 分辨率自适应 — 用 state + resize listener 而非常量；在 App 组件内初始化，
// 这样 setup 时窗口已确定，能拿到实际视口宽度。
// eslint-disable-next-line prefer-const
// (上面保留 computeMainMinWidth / computeLeftMin 供 typing 引用)

export default function App() {
  const [page, setPageRaw] = useState<NavPage>("chat");
  // 系统配置弹窗（非路由跳转）
  const [showConfigModal, setShowConfigModal] = useState(false);
  // 弹窗打开时落在哪个分类。默认「模型」：配置入口绝大多数是"没配模型/想换
  // 模型"，此前硬编码落在「工具」→ 用户点了"配置模型"却看到插件页。
  const [configCategory, setConfigCategory] = useState<ConfigCategory>("model");

  /** 打开系统配置弹窗。`category` 让调用方指定落点（如聊天输入框的
   *  "未配置模型"入口应直达「模型」），缺省「模型」。 */
  const openSettings = (category: ConfigCategory = "model") => {
    setConfigCategory(category);
    setShowConfigModal(true);
  };
  // 「自定义应用」页「创建新应用」按钮跳转时注入的预填指令（一次性消费）。
  const [pendingPrefill, setPendingPrefill] = useState<string | null>(null);
  // 视口自适应面板最小宽度 — 窗口缩小时缩小主列/左列最小宽
  const [mainMinWidth, setMainMinWidth] = useState(() => computeMainMinWidth(window.innerWidth));
  const [leftMin, setLeftMin] = useState(() => computeLeftMin(window.innerWidth));
  // 包装 setPage: 右栏是 chat 专属 UI，切到任意非对话页时主动关闭产物栏
  // （agents/experts/settings/plugins/automation/search）。
  const setPage = useCallback((p: NavPage) => {
    setPageRaw(p);
    if (p !== "chat") setRightOpen(false);
  }, []);
const [sessions, setSessions] = useState<Session[]>([]);
const [messageQueue, setMessageQueue] = useState<QueuedMessage[]>([]);
// 右栏"加入对话"右键菜单触发外部文件附件。
const [pendingFileAttachment, setPendingFileAttachment] = useState<string | null>(null);
const handleAddFileToChat = useCallback((path: string) => {
setPendingFileAttachment(path);
// Ensure we're on the chat page so the user sees the attachment chip.
setPage("chat");
}, []);

// Computer file explorer → preview modal: create a synthetic Artifact
// from a file path so the existing PreviewPanel (with syntax highlighting,
// image preview, spreadsheet rendering, etc.) can display it.
const [computerPreviewArtifact, setComputerPreviewArtifact] = useState<Artifact | null>(null);
const handlePreviewFileFromExplorer = useCallback((path: string, name: string) => {
  // 用 utils/artifacts 里统一的 classify()，保证文件浏览器与 InlinePathPreview
  // 等各处对同一文件选同一图标/预览分支（避免各写各的分类导致不一致）。
  const kind = classifyArtifactKind(name);
  setComputerPreviewArtifact({
    id: `computer-preview-${Date.now()}`,
    name,
    path,
    kind,
    tool: "file_explorer",
    timestamp: new Date().toISOString(),
    isAbsolute: true,
  });
}, []);
  // 队列变更 handlers 保持 memoized，避免 ChatComposer 在主输入框每次按键时重渲染。
  // sendFromQueue 闭包依赖下方定义的 streaming / handleSendMessage，故在其后声明。
  const removeFromQueue = useCallback((idx: number) => {
    setMessageQueue((prev) => prev.filter((_, i) => i !== idx));
  }, []);
  const editQueueItem = useCallback((idx: number, newText: string) => {
    setMessageQueue((prev) => prev.map((m, i) => (i === idx ? { ...m, text: newText } : m)));
  }, []);

  // 视口自适应 — 窗口尺寸变化时重算面板最小宽度
  // 对应 computeMainMinWidth / computeLeftMin 的分档逻辑。
  // 放在这里（useEffect 最前面），确保每次 resize 都更新。
  // 同时监听 ui-scale 事件：缩放会改变等效视口宽（innerWidth 不变），
  // 不重算的话，在窄窗口放大后三列总最小宽会超过真实可用宽度而撑破布局。
  useEffect(() => {
    const onResize = () => {
      setMainMinWidth(computeMainMinWidth(window.innerWidth));
      setLeftMin(computeLeftMin(window.innerWidth));
    };
    onResize(); // 初始化执行一次
    window.addEventListener("resize", onResize);
    window.addEventListener(UI_SCALE_EVENT, onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener(UI_SCALE_EVENT, onResize);
    };
  }, []);

  // ── 全局缩放快捷键（防死锁：缩放太大看不到菜单时用键盘恢复）──
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      if (e.key === "0") {
        e.preventDefault();
        applyScale(1);
        return;
      }
      const step = e.key === "=" || e.key === "+" ? 0.1 : e.key === "-" ? -0.1 : 0;
      if (step === 0) return;
      e.preventDefault();
      applyScale(readScale() + step);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const [activeSessionId, setActiveSessionIdRaw] = useState<string | null>(() => {
    try { return storage.getItem(ACTIVE_SESSION_KEY); } catch { return null; }
  });
  const setActiveSessionId = useCallback((id: string | null) => {
    setActiveSessionIdRaw(id);
    try { id ? storage.setItem(ACTIVE_SESSION_KEY, id) : storage.removeItem(ACTIVE_SESSION_KEY); } catch {}
  }, []);

  // sessions 的最新值镜像到 ref，给异步回调（onDone 后的
  // 500ms re-fetch）一个同步读 sessions 的入口。setState 之后 useEffect
  // 链上的 closure 会拿到陈旧值，但 ref 永远跟最新值同步。
  const sessionsRef = useRef<typeof sessions>([]);
  useEffect(() => { sessionsRef.current = sessions; }, [sessions]);

  // Mirror activeSessionId to a ref so callbacks like handleNewChat can read
  // the current value without depending on it in their dep array. Without
  // this, handleNewChat would be recreated on every activeSessionId change,
  // which — combined with the sessions dep — caused the "flicker between
  // sessions on new chat" bug.
  const activeSessionIdRef = useRef(activeSessionId);
  useEffect(() => { activeSessionIdRef.current = activeSessionId; }, [activeSessionId]);

  // 切换会话时立即把上下文用量重置为 null，避免上一个会话的后台 stream 事件或
  // 残留值污染当前会话的 context-usage 进度条。contextUsage / usageSnapshot 这两个
  // state 只在 WS 事件与 restoredCtx 处写入，没有任何切换重置；reset 后进度条在新
  // 会话首句事件到达前不渲染，到达后才显示该会话真实用量。
  useEffect(() => {
    setContextUsage(null);
    setUsageSnapshot(null);
  }, [activeSessionId]);

  const [sidebarOpen, setSidebarOpen] = useState(true);

  // 全局搜索浮层。从主导航摘掉"搜索"页后由标题栏图标触发。
  const [searchOpen, setSearchOpen] = useState(false);

  const [rightOpen, setRightOpen] = useState<boolean>(() => {
    try { return storage.getItem(RIGHT_OPEN_KEY) !== "false"; } catch { return false; }
  });
  useEffect(() => {
    try { storage.setItem(RIGHT_OPEN_KEY, String(rightOpen)); } catch {}
  }, [rightOpen]);

  // 右栏当前标签，可被外部控制（点目录 chip 直接切到「电脑」）。
  const [rightTab, setRightTab] = useState<RightTab>("products");

  // 三列临界视口 + 默认窗口宽（见上方注释块）。
  const THREE_COL_MIN_VIEWPORT = 1140;
  const DEFAULT_WINDOW_WIDTH = 1280;
  const appWindow = getCurrentWindow();

  // 启动引导：窗口高度适配 + 旧版 localStorage key 迁移 + dev 诊断 dump。
  // 具体副作用已抽到 ./app/useAppBootstrap，此处仅组合调用。
  useAppBootstrap(DEFAULT_WINDOW_WIDTH);

  // 切换右栏：视口 < 三列临界时，先 await 把窗口扩到能装下三列，再 setRightOpen(true)；
  // resize 路径仍由 viewport-aware effect 处理（装不下就关右栏）。
  // 必须在 setSize 完成后再开右栏：setSize 异步期间 window.innerWidth 仍 < 临界，
  // viewport-aware effect 看到 rightOpen=true + vw<临界会立刻掐掉，造成"点一下抖一下"。
  // setSize 失败时保持 rightOpen 不变并 console.warn，让用户手动拉宽窗口再点或重试点。
  const handleToggleRight = useCallback(async () => {
    if (rightOpen) {
      // 关闭动作 — 任何视口下都直接关。
      setRightOpen(false);
      return;
    }
    // 打开动作 — 视口不足时先扩窗.
    if (window.innerWidth < THREE_COL_MIN_VIEWPORT) {
      try {
        const outer = await appWindow.outerSize();
        const factor = await appWindow.scaleFactor();
        // outerSize / scaleFactor → logical px (setSize 用 logical).
        const logicalH = Math.max(1, Math.round(outer.height / factor));
        await appWindow.setSize(new LogicalSize(DEFAULT_WINDOW_WIDTH, logicalH));
      } catch (e) {
        // setSize 失败 — 保持 rightOpen 不动, 不开右栏, 避免"开了又
        // 立刻被掐"的抖动. 用户可以手动拉宽窗口再点.
        console.warn("[right] auto-expand window failed, right sidebar not opened:", e);
        return;
      }
    }
    setRightOpen(true);
  }, [rightOpen]);

  // 欢迎页（empty state）默认关闭产物栏。ChatView 切到 welcome 时通过 onEnterWelcome
  // 通知此 handler 做幂等关闭；ChatView 内部用 lastIsWelcomeRef 保证只在 false→true
  // 切换时触发，因此不会把用户在欢迎页手动开的右栏反复掐掉。
  const handleEnterWelcome = useCallback(() => {
    setRightOpen(false);
  }, []);

  // 左列 / 右列：viewport-aware 夹紧在下面那个 useEffect 里集中做（每次
  // window.resize、rightOpen 切换、右列宽度变化时重算），不再给
  // useResizable 传 dynamicMax — 那样做会让两个 useResizable 互为依赖，
  // 一旦 deps 抖动就会触发 React hook-order 报错（详见上一次提交后控制台
  // 里的 "change in the order of Hooks" 报告）。useResizable 现在只负责
  // 自己那列的 min/max 静态边界，动态夹紧走这里的单一 effect，单向数据
  // 流，没有循环依赖。
  const left = useResizable({
    storageKey: LEFT_KEY,
    defaultWidth: LEFT_DEFAULT,
    min: LEFT_MIN,
    max: LEFT_MAX,
    side: "left",
  });
  const right = useResizable({
    storageKey: RIGHT_KEY,
    defaultWidth: RIGHT_DEFAULT,
    min: RIGHT_MIN,
    max: RIGHT_MAX,
    side: "right",
  });
  const streamChatApi = useStreamChat(activeSessionId);
  // 仅保留 App 自身（标题栏/指示器/审批流）需要的字段；流状态展示已
  // 改由 ChatView 通过 StreamChatContext 直接读，不再逐层透传。
  const { streaming, streamingSessionId, setContextUsage, setUsageSnapshot, setStreamingStatus: setStreamingStatusImpl, send: sendStream, cancel: cancelStream, wsSendRaw } = streamChatApi;

  // Session message eviction — clear the *previous* session's
  // messages array (keeping message_count) when switching away. This solves
  // the "A→B smooth, B→A laggy" asymmetry: when A has many messages, React
  // must render all react-markdown + Prism.js instances, causing lag. After
  // eviction, switching back re-fetches from the server, but the peak memory
  // drops from max(A+B) to max(1). Must be declared AFTER streamingSessionId.
  const pendingEvictRef = useRef<string | null>(null);
  useEffect(() => {
    const prevId = prevActiveSessionIdRef.current;
    if (!prevId || prevId === activeSessionId) return;

    // Never evict a session that is still streaming — the onChunk /
    // onToolStart / onToolComplete / onDone callbacks all write into
    // s.messages. Clearing them mid-stream would lose all accumulated
    // text and tool events. Defer eviction until the stream finishes.
    if (streamingSessionId === prevId) {
      pendingEvictRef.current = prevId;
      return;
    }

    const evict = (s: typeof sessionsRef.current[number]) =>
      s.id === prevId && s.messages.length > 0
        ? { ...s, messages: [], message_count: s.message_count ?? s.messages.length }
        : s;
    setSessions((prev) => prev.map(evict));
    // Synchronous mirror so the getSessionMessages effect (which reads
    // sessionsRef.current) sees the eviction immediately, without
    // waiting for the next render cycle's useEffect to update the ref.
    sessionsRef.current = sessionsRef.current.map(evict);
  }, [activeSessionId, streamingSessionId]);

  // Deferred eviction: when a stream finishes (streamingSessionId → null),
  // check whether the previously-viewed session's messages are still in
  // memory and evict them. This handles the case where the user switched
  // away from a session WHILE it was streaming — the eviction was
  // skipped above, and this effect picks it up once the stream completes.
  useEffect(() => {
    if (pendingEvictRef.current && !streamingSessionId) {
      const evictId = pendingEvictRef.current;
      pendingEvictRef.current = null;
      const evict = (s: typeof sessionsRef.current[number]) =>
        s.id === evictId && s.messages.length > 0
          ? { ...s, messages: [], message_count: s.message_count ?? s.messages.length }
          : s;
      setSessions((prev) => prev.map(evict));
      sessionsRef.current = sessionsRef.current.map(evict);
    }
  }, [streamingSessionId]);

  // Per-send phase tracking. Set by the `onPhaseChange` callback in
  // `handleSendMessage` and scoped to the current `sessionId` so the
  // ChatView only shows the phase chip for the active session.
  const [streamPhase, setStreamPhase] = useState<StreamPhase | null>(null);
  const prevPhase = useRef<StreamPhase | null>(null);

  // ACUI: the `acui` hook + `acuiBus` useMemo are declared further
  // down (after handleApproveApproval / handleDenyApproval /
  // handleAnswerClarify) to keep the temporal dead zone happy. We
  // reference them inside `handleSendMessage` via closure.

  // Per-session streaming flag. The hook's `streaming` is just
  // `streamingSessionId !== null` for backward-compat callers, but the
  // ChatView needs the *scoped* answer: is *this* session the one
  // currently streaming? Without this, switching to another session
  // mid-stream would leak the "正在思考…" indicator into the new
  // session's view (the new session isn't actually streaming).
  const isStreamingActiveSession =
    streamingSessionId !== null && streamingSessionId === activeSessionId;

  /**
   * Most recent stream error, scoped to the active session.  Set when
   * the WebSocket closes before any chunk landed on the optimistic
   * assistant placeholder (typical when the backend was unreachable
   * or killed mid-handshake).  Cleared by ChatView after rendering
   * for ~6s, or whenever a new send starts.  The ChatView uses this
   * to surface a synthetic "⚠️ …" row at the tail of the message
   * list so the user sees something happen — without this the empty
   * optimistic row gets filtered out by the placeholder filter and
   * the chat just sits there silent (which is what the previous
   * version did).
   */
  /** Stream-level error surfaced to the chat, scoped per session.
   *  `message` is the friendly, localized summary; `detail` (optional)
   *  is the raw, un-localized backend error shown as a collapsible
   *  "技术详情" subtext so English provider errors don't pollute the
   *  main UI. Scoping by `sessionId` is what stops an error from
   *  one conversation from bleeding into the welcome screen or
   *  another conversation — without this, switching sessions
   *  displays a stale error from a previous failed send. */
  const [streamErrors, setStreamErrors] = useState<Map<string, {
    message: string;
    detail?: string;
    kind?: StreamErrorKind;
  }>>(() => new Map());
  // Sentinel key under which we stash errors that happen on the
  // welcome screen (activeSessionId is null, e.g. createSession
  // failed before any session was created). Without this slot,
  // first-send failures on the welcome screen would have nowhere
  // to land and the user would see nothing.
  const WELCOME_KEY = "__welcome__";
  const setStreamError = useCallback(
    (sessionId: string | null, err: { message: string; detail?: string; kind?: StreamErrorKind }) => {
      const key = sessionId ?? WELCOME_KEY;
      setStreamErrors((prev) => {
        const next = new Map(prev);
        next.set(key, err);
        return next;
      });
    },
    [],
  );
  const clearStreamError = useCallback(
    (sessionId: string | null) => {
      const key = sessionId ?? WELCOME_KEY;
      setStreamErrors((prev) => {
        if (!prev.has(key)) return prev;
        const next = new Map(prev);
        next.delete(key);
        return next;
      });
    },
    [],
  );

  /**
   * Per-session message-load errors. Cleared on success, set on any
   * fetch rejection. Lives only in memory — these are transient network
   * states, not user data. The `ChatView` reads the active session's
   * entry to decide between the empty-state and an inline error card
   * with a "重试" button.
   */
  const [messageLoadErrors, setMessageLoadErrors] = useState<Map<string, string>>(
    () => new Map(),
  );
  const setLoadError = useCallback((sessionId: string, message: string) => {
    setMessageLoadErrors((prev) => {
      const next = new Map(prev);
      next.set(sessionId, message);
      return next;
    });
  }, []);
  const clearLoadError = useCallback((sessionId: string) => {
    setMessageLoadErrors((prev) => {
      if (!prev.has(sessionId)) return prev;
      const next = new Map(prev);
      next.delete(sessionId);
      return next;
    });
  }, []);
  /**
   * Mutable ref to the *currently streaming* assistant message id.
   * Re-assigned whenever the agent pauses for an approval request —
   * the previous id is "frozen" (no more chunks land on it) and a
   * new empty assistant message takes over. This lets the inline
   * approval card sit at the exact slot the agent emitted it, with
   * the post-approval response continuing as a fresh message
   * immediately below.
   */
  const currentAssistantIdRef = useRef<string | null>(null);
  /**
   * The latest prefetch intent info from the WS stream. Set by
   * `onPrefetch` so ChatView can render a `PrefetchIntentChip`
   * below the streaming message.
   */
  const [prefetchIntent, setPrefetchIntent] = useState<{
    intent: string;
    confidence: number;
    results: import("./hooks/useStreamChat").PrefetchResultEntry[];
  } | null>(null);
  /**
   * Pending decision resolvers, keyed by approval request id. Populated
   * the moment the server emits `RequestApproval`; settled when the
   * user clicks allow/deny on the inline card. The settlement value is
   * the WebSocket payload that the stream layer echoes back via
   * `approval_response`.
   */
  const approvalResolversRef = useRef<
    Map<
      string,
      (decision: Exclude<ApprovalDecision, null>, remember?: boolean) => void
    >
  >(new Map());
  // Same shape, keyed by `batch_id`. Resolves the promise returned
  // by `onApprovalBatchRequest` when the user clicks Allow / Deny /
  // Always on a batched card.
  const approvalBatchResolversRef = useRef<
    Map<
      string,
      (decision: Exclude<ApprovalDecision, null>, remember?: boolean) => void
    >
  >(new Map());
  // Retry timers for approval responses — cleaned up when approval is resolved
  const approvalRetryTimersRef = useRef<Map<string, ReturnType<typeof setInterval>>>(new Map());
  // Frontend timeout timers — auto-mark as "expired" when the
  // server-emitted deadline passes without a user click. Without
  // this the card stays "等待你的决定" forever even though the
  // backend already auto-denied at 30s and the agent moved on.
  const approvalTimeoutTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  /**
   * 记录每个待审批请求归属的 sessionId（与 agent 循环压入请求的顺序一致，
   * JS Map 保证插入顺序）。REST 兜底 approve/deny 时用来定位正确的
   * `/v1/sessions/<sid>/approval` 端点——请求可能属于用户已切走的 session。
   */
  const pendingApprovalSessionsRef = useRef<Map<string, string>>(new Map());
  /**
   * 工具调用被拒绝（用户拒绝 / 超时 / 连接中断）时显示的短时顶部横幅。对应
   * agent 注入的"告诉用户"系统提示给出可见反馈，让用户知道调用为何没执行。
   * 30s 后自动消失；新的拒绝会替换当前横幅。`"policy"` 类拒绝是静默拦截，不进这里。
   */
  const [denialBanner, setDenialBanner] = useState<{ kind: string; tool: string } | null>(null);
  const denialBannerTimerRef = useRef<number | null>(null);
  const showDenialBanner = useCallback((kind: string, tool: string) => {
    setDenialBanner({ kind, tool });
    if (denialBannerTimerRef.current !== null) {
      window.clearTimeout(denialBannerTimerRef.current);
    }
    denialBannerTimerRef.current = window.setTimeout(() => {
      setDenialBanner(null);
      denialBannerTimerRef.current = null;
    }, 30_000);
  }, []);
  const dismissDenialBanner = useCallback(() => {
    if (denialBannerTimerRef.current !== null) {
      window.clearTimeout(denialBannerTimerRef.current);
      denialBannerTimerRef.current = null;
    }
    setDenialBanner(null);
  }, []);
  /**
   * Tracks running tool messages by their stable WS `tool_call_id`
   * so that `onToolChunk` / `onToolComplete` can find the right inline
   * card (by message `id`) even when multiple tools run concurrently.
   */
  const runningToolRef = useRef<Map<string, string>>(new Map());
  /**
   * Tracks the current streaming "phase" (reasoning / tools / text) so
   * we can detect phase transitions and push new unified timeline steps
   * at the right time. Reset to null at the start of each stream.
   */
  const streamPhaseRef = useRef<"reasoning" | "tools" | "text" | null>(null);
  // one TodoListCard per plan / group. State
  // (not ref) so ChatView re-renders when a new plan arrives.
  // Ordered: groups are stored in the order their group_started
  // event arrived. Items are keyed by id so upsert is a single
  // object property write.
  interface TodoGroup {
    groupId: string;
    groupTitle: string | null;
    anchorMessageId: string | null;
    itemsById: Record<string, TodoSnapshotItem>;
  }
  const [todoGroups, setTodoGroups] = useState<TodoGroup[]>([]);
  // Live sub-agent (delegate_task) state, keyed by `subAgentId`. Fed by
  // `onSubAgentEvent` from the WS stream and rendered by `SubAgentPanel`
  // inside ChatView. Cleared when the active session changes.
  const [subAgents, setSubAgents] = useState<Record<string, SubAgentState>>({});
  const todoGroupsByIdRef = useRef<Map<string, TodoGroup>>(new Map());
  /**
   * Shared mutable set between App.tsx and every TodoListCard. Card
   * writes the item id before an optimistic toggle-patch and removes
   * it when the patch settles (including on error — see
   * TodoListCard.tsx catch block).  The onTodoChange handler below
   * skips WS echo upserts for ids that are currently in this set,
   * preventing rapid-toggle races.
   */
  const justPatchedTodoRef = useRef<Set<string>>(new Set());
// needsNewAssistantRef removed. All text chunks in an
// agent loop now go to the SAME assistant message. Tool calls are
// tracked in the assistant's `execution_timeline` (rendered as a
// collapsible block inside the bubble) instead of separate
// `role: "tool"` messages. This makes streaming and reload
// paths produce identical visuals:
// [avatar] [text] [collapsible tools] [more text]
// instead of the old split where tools appeared above the avatar
// because empty assistant placeholders were filtered out.
  /**
   * Pending `clarify` callbacks — keyed by `request_id`. When the
   * server pushes a `ClarifyRequest` event we register a resolver
   * here, the user picks an option (or types a free-form answer) in
   * the inline card, and `handleAnswerClarify` resolves the promise.
   * The stream layer then sends the answer back as a
   * `clarify_response` WS frame.
   */
  const clarifyResolversRef = useRef<
    Map<string, ((answer: string) => void)[]>
  >(new Map());
  const { workspaceRoot, setWorkspaceRoot } = useWorkspaceRoot();

  // 工作区变更需同时广播到两个出口：(1) setWorkspaceRoot 推 global；(2) setSessions
  // 改 session.workspace 让 Sidebar useMemo 重算。否则 ComputerTab 读 useWorkspaceRoot
  // （global）而 Sidebar 只依赖 sessions/workspaces state，两边都不感知 Composer chip
  // 的修改，导致文件面板与对话分组不同步。
  // path=null（用户点"不绑定目录"）时也清掉 global workspaceRoot，避免下次开新
  // session 时残留上次根目录。
  const onWorkspaceChange = useCallback((sessionId: string | null | undefined, path: string | null) => {
    if (path) {
      setWorkspaceRoot(path);
    } else {
      setWorkspaceRoot(null);
    }
    if (sessionId) {
      setSessions((prev) => prev.map((s) => s.id === sessionId ? { ...s, workspace: path ?? undefined } : s));
    }
  }, [setWorkspaceRoot]);

  // 点目录 chip（已选工作区时）→ 打开右栏并切到「电脑」标签，
  // 直接把文件浏览器切到所选工作区目录，不再弹 chooser 列表。
  const openComputer = useCallback(() => {
    setRightTab("computer");
    setRightOpen(true);
  }, []);

  // Auto-collapse sidebar on small screens
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 768px)");
    const handler = (e: MediaQueryListEvent | MediaQueryList) => setSidebarOpen(!e.matches);
    handler(mq);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  // 切换会话时清空 todoGroups：否则上一个会话的 todo 会泄漏进新会话的聊天。
  // WS 监听器已按会话过滤，但同标签页内切换会话残留的旧 state 仍会保留。
  // 新会话由下一条 `todo_snapshot` WS 事件重新填充。
  // Note: this only triggers when `activeSessionId` actually
  // changes, not on every render. The `todoGroupsByIdRef` mirror is
  // cleared in the same pass so the dedup set never sees ghosts.
  const prevActiveSessionIdRef = useRef<string | null>(null);
  useEffect(() => {
    if (prevActiveSessionIdRef.current === activeSessionId) return;
    prevActiveSessionIdRef.current = activeSessionId;
    todoGroupsByIdRef.current.clear();
    setTodoGroups([]);
    // Clear live sub-agent state on session switch so a previous
    // session's "子代理" panel doesn't leak into the new one. The
    // next `sub_agent_event` WS events re-populate it.
    setSubAgents({});
  }, [activeSessionId]);

  // Viewport-aware layout: 三列共存阈值从 state 读取，不再硬编码 1140。
  // 视口装不下三列就关掉右栏，不挤不变形，也不弹 modal 遮罩。拉宽后不会自动重开，
  // 需用户手动点 TitleBar 上的右栏按钮。
  // 阈值从 state (leftMin + mainMinWidth + RIGHT_MIN) 实时计算，窗口缩小时主列和
  // 左列最小宽度都会缩，临界阈值随之降低，右栏可在更小窗口下保持打开。
  // recompute 触发条件: window.resize / rightOpen 切换 / 用户拖动 resizer.
  // 三列共存时再 clamp 各自列到静态边界 (LEFT_MAX / RIGHT_MIN..RIGHT_MAX).
  const leftWidthRef = useRef(left.width);
  const rightWidthRef = useRef(right.width);
  useEffect(() => { leftWidthRef.current = left.width; }, [left.width]);
  useEffect(() => { rightWidthRef.current = right.width; }, [right.width]);

  useEffect(() => {
    const recompute = () => {
      // 用等效视口宽度（物理视口 ÷ zoom）。缩放不改变 window.innerWidth，
      // 直接用 innerWidth 会在放大后误判"窗口很宽"，把右栏留在三列共存状态，
      // 而真实可用宽度已装不下三列 → 整行溢出被裁。
      const vw = window.innerWidth / currentZoom();
      // 三列共存下限 = state 中的 leftMin + mainMinWidth + RIGHT_MIN(360).
      // 动态计算：视口缩小时 leftMin 和 mainMinWidth 都缩,
      // 临界阈值随之降低, 右栏可以在更小的窗口下保持打开.
      const minThreeCol = leftMin + mainMinWidth + RIGHT_MIN;
      if (vw < minThreeCol && rightOpen) {
        setRightOpen(false);
        return;
      }
      // 三列共存: clamp 各自列到静态边界 (LEFT_MAX / RIGHT_MIN..RIGHT_MAX).
      // grid minmax 在 CSS 端兜底物理下限.
      if (rightOpen && rightWidthRef.current < RIGHT_MIN) {
        right.setWidth(RIGHT_MIN);
      }
      if (leftWidthRef.current > LEFT_MAX) {
        left.setWidth(LEFT_MAX);
      }
    };
    recompute();
    window.addEventListener("resize", recompute);
    window.addEventListener(UI_SCALE_EVENT, recompute);
    return () => {
      window.removeEventListener("resize", recompute);
      window.removeEventListener(UI_SCALE_EVENT, recompute);
    };
  }, [rightOpen, left.setWidth, right.setWidth, leftMin, mainMinWidth]);

  // Load sessions
  useEffect(() => { api.getSessions().then(setSessions).catch((e) => console.error("getSessions failed", e)); }, []);

  // Register built-in experts to backend ExpertRegistry on app start.
  // Best-effort: the UI fetches the catalog on first mount and falls back
  // to BUILTIN_EXPERTS if the backend is down, so a failure here is non-fatal.
  useEffect(() => { registerBuiltinsToBackend().catch((e) => { console.warn("[App] registerBuiltinsToBackend failed:", e); }); }, []);

  // Load messages for active session.
  // Why no `loadedSessionMessagesRef` here: a previous version added the
  // sessionId to a Set *before* dispatching the fetch, then `.catch(() => {})`
  // swallowed the error. If the first fetch failed (typical during a backend
  // restart where the desktop is up before the HTTP server is listening),
  // the Set entry would persist across every subsequent re-render and the
  // effect would silently never retry — user would see a permanently empty
  // session. The fix: only check `session.messages.length > 0` and let
  // transient failures re-attempt on the next `sessions` reference change
  // (which happens whenever the user does *anything* that triggers a
  // `setSessions`, including switching to another session and back). The
  // `cancelled` flag in the cleanup function prevents a stale response from
  // a previous mount of this effect from clobbering fresher data.
  useEffect(() => {
    if (!activeSessionId) return;
    // use sessionsRef to avoid re-running this effect on
    // every sessions change (e.g. during streaming). The previous deps
    // included `sessions`, which meant every streaming chunk re-triggered
    // this effect. Although the `messages.length > 0` guard prevented
    // re-fetching, the effect cleanup (`cancelled = true`) would fire
    // on every chunk, potentially cancelling an in-flight fetch from
    // a different session that had just been switched to.
    // Added `sessions.length` to deps to fix a startup race
    // condition. On app launch, `activeSessionId` is restored from
    // localStorage BEFORE `getSessions()` completes. The effect fires
    // immediately, but `sessionsRef.current` is empty, so it returns
    // without loading messages. When `getSessions()` completes later,
    // `sessions.length` changes from 0 to N, which re-triggers this
    // effect and loads the messages for the active session.
    // During streaming, `sessions.length` doesn't change (only the
    // messages within existing sessions change), so this effect doesn't
    // re-fire on every chunk — preserving the fix.
    if (sessions.length === 0) return;
    const session = sessionsRef.current.find((s) => s.id === activeSessionId);
    if (!session) { if (sessionsRef.current.length > 0) setActiveSessionId(null); return; }
    if (session.messages.length > 0) return;

    let cancelled = false;
    api.getSessionMessages(activeSessionId)
      .then(({ messages, contextUsage: restoredCtx }) => {
        if (cancelled) return;
        clearLoadError(activeSessionId);
        if (restoredCtx) setContextUsage(restoredCtx);
        setSessions((prev) => prev.map((s) =>
          s.id === activeSessionId ? { ...s, messages } : s
        ));
      })
      .catch((e) => {
        if (cancelled) return;
        const msg = e instanceof Error ? e.message : String(e);
        console.error(`[chat] getSessionMessages(${activeSessionId}) failed:`, msg);
        setLoadError(activeSessionId, msg);
      });
    return () => { cancelled = true; };
  }, [activeSessionId, sessions.length, setActiveSessionId, setLoadError, clearLoadError]);

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;

  const { rawArtifacts, artifacts, selectedArtifactId, setSelectedArtifactId, handlePathClick } = useArtifacts({
    activeSession,
    workspaceRoot,
    onOpenRightSidebar: () => setRightOpen(true),
  });

  /**
   * Number of tool messages whose `status === "running"` in the active
   * session. We surface this to `ChatView` so the post-stream
   * activity indicator can show "执行工具中…" when the LLM stream
   * settles but tool calls are still in flight (the user otherwise
   * sees a blank gap between "已完成" and the next turn's first
   * chunk, which reads as "Hermix is stuck"). Memoized off the
   * session reference so we don't re-walk messages on every render.
   */
  /**
   * Number of tool invocations currently in flight, tracked live by
   * `onToolStart` / `onToolComplete` (mirrors `runningToolRef.size`).
   *
   * fix: previously derived from `activeSession.messages` via
   * `m.role === "tool" && m.tool?.status === "running"` — but tool
   * events are now appended to the assistant's `execution_timeline`
   * instead of creating separate `role: "tool"` messages, so that
   * path was always empty and the "正在执行 N 个工具…" indicator
   * never showed. Now driven by the live stream callbacks.
   */
  const [runningToolCount, setRunningToolCount] = useState(0);
  // Reset the live counter when the active session changes (a fresh
  // stream may have left stale increments in the previous session).
  useEffect(() => {
    setRunningToolCount(runningToolRef.current.size);
  }, [activeSession?.id]);
  const pendingToolsCount = runningToolCount;

  /**
   * Auto-generate a session title from the FIRST user message of the
   * conversation, unless the existing title is already a meaningful one.
   *
   * fix: previously the title was recomputed whenever
   * `(s.message_count ?? s.messages.length) === 0`. But in-memory
   * `message_count` for in-app-created sessions stays 0 forever (it is only
   * ever set from the backend), so that condition was true on EVERY send —
   * and the `firstUserContent` passed in was actually the *current* (last)
   * message. The result: the sidebar title kept getting overwritten by the
   * last user message instead of the first.
   *
   * Now we only recompute while the title is still a placeholder, and we
   * derive the text from the FIRST user message found in `s.messages`
   * (falling back to the just-sent message only for a brand-new session's
   * very first turn). Once a real title exists it is preserved across turns.
   */
  const computeAutoTitle = (s: Session, fallbackContent: string): string => {
    const isPlaceholder =
      !s.title || s.title === "新建对话" || s.title === "新对话" || s.title === "New chat";
    if (!isPlaceholder) return s.title;
    const firstUserMsg = s.messages.find((m) => m.role === "user");
    const raw = firstUserMsg?.content ?? fallbackContent;
    const text = raw.replace(/\s+/g, " ").trim();
    return text.slice(0, 28) + (text.length > 28 ? "…" : "");
  };

  const handleNewChat = useCallback(async () => {
    // Do NOT cancel the active stream when creating a new chat.
    // The stream on the previous session should continue in the background.
    try { clearStreamError(activeSessionId); } catch (e) { console.warn("[App] clearStreamError failed:", e); }

    // New behavior (2026-07-30): "新建对话" only enters the welcome screen.
    // We do NOT create a server session or add a sidebar entry yet.
    // The real session is created when the user sends the first message
    // (see handleSendMessage), at which point we know the workspace/group
    // and can render a meaningful title.
    setActiveSessionId(null);
    setPage("chat");
  }, [clearStreamError, activeSessionId, setActiveSessionId]);

  // 从「自定义应用」页「创建新应用」按钮触发：进入新对话并预填创建指令。
  const startCreateAppFlow = useCallback(() => {
    setPendingPrefill("@app 创建新应用 应用要求：");
    setActiveSessionId(null);
    setPage("chat");
  }, [setActiveSessionId, setPage]);

  const handleSelectSession = useCallback(async (id: string) => {
    // Do NOT cancel the active stream when switching sessions.
    // The previous implementation called `cancelStream()` which closed the
    // WebSocket, killing any in-flight agent task on the old session.
    // Now the stream continues in the background — useStreamChat already
    // tracks streamingSessionId so the UI correctly scopes the thinking
    // indicator and phase chip to whichever session is actually streaming.
    // The Sidebar pulse dot also uses streamingSessionId so the user can
    // see that the old session is still working even after switching away.
    try { clearStreamError(activeSessionId); } catch (e) { console.warn("[App] clearStreamError failed:", e); }
    // Clear the phase chip — it belongs to the old session. If the
    // active stream is on the new session, onPhaseChange will
    // re-set it immediately; otherwise we dont want a stale chip.
    setStreamPhase(null);
    setActiveSessionId(id);
    setPage("chat");
    if (window.innerWidth < 768) setSidebarOpen(false);

    // The activeSessionId effect will fetch messages from the server
    // if the session doesn't have them in memory yet. We no longer
    // duplicate the fetch here — the effect is the single source of
    // truth for loading session messages, which avoids race conditions
    // between two concurrent fetches overwriting each other's results.
    // 若会话已有内存中的消息（如刚结束的流式对话），effect 跳过 fetch
    // 直接复用内存 state。
  }, [clearStreamError, activeSessionId, setActiveSessionId, setPage]);

  const handleDeleteSession = useCallback(async (id: string) => {
    // the previous implementation called `api.deleteSession`
    // with no `try/catch`, so a 5xx from the backend (e.g. the
    // `FOREIGN KEY constraint failed` we hit on sessions with
    // `compression_archives` rows) would propagate as an unhandled
    // rejection. React's unhandled-rejection handler is silent in our
    // Electron/Tauri build, so the user saw the click "do nothing"
    // and the session stayed in the Sidebar forever. Now: log to
    // console + surface a dialog so the user actually knows.
    try {
      await api.deleteSession(id);
      setSessions((prev) => prev.filter((s) => s.id !== id));
      if (activeSessionId === id) setActiveSessionId(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[deleteSession] failed for", id, err);
      window.alert(`删除会话失败：${msg}`);
    }
  }, [activeSessionId]);

  /**
   * Settle a pending approval card. Called by the inline card's
   * "允许" / "拒绝" buttons. Three things happen in sequence:
   *   1. resolve the promise the stream layer is awaiting (this is
   *      what unblocks the agent's `await on_pre_tool_dispatch`)
   *   2. mutate the approval message in place so the card transitions
   *      to its "已允许" / "已拒绝" terminal state immediately
   *   3. the stream layer will then send `approval_response` over the
   *      WebSocket; the server's `resolve_approval_message` updates
   *      the persisted row for replay across reloads
   *
   * If no resolver is registered — i.e. the user clicked
   * after the stream already settled (the 5-minute
   * `DEFAULT_APPROVAL_TIMEOUT` fired, the WebSocket closed,
   * or a duplicate click deleted the entry) — we do NOT
   * flip the card to `approved` / `denied`. The backend
   * already auto-denied via the timeout, and showing
   * "已允许" on a card whose decision the agent never
   * received is the exact race the owner reported on
   * ("I clicked allow, the card said 已允许, but
   * the LLM said denied"). Instead we surface a distinct
   * `expired` state so the user can see that their click
   * was a no-op and re-issue the prompt if they still want
   * the action to run.
   */
  const resolveApproval = useCallback(
    (
      requestId: string,
      decision: Exclude<ApprovalDecision, null>,
      remember?: boolean,
    ): ApprovalDecision | null => {
      // Helper: flip a single assistant message's
      // `kind: "approval_request"` step to the given decision. Used
      // in both the live (resolver present) and stale (resolver gone)
      // branches below.
      const stampDecision = (
        s: Session,
        decision: ApprovalDecision,
        remember?: boolean,
      ): Session => ({
        ...s,
        messages: s.messages.map((m) => {
          const steps = m.unified_steps;
          if (!steps || steps.length === 0) return m;
          let touched = false;
          const next = steps.map((step): TimelineStep => {
            if (step.kind !== "approval_request") return step;
            if (step.meta.requestId !== requestId) return step;
            if (step.meta.decision) {
              // Already decided (stale click path); leave as-is.
              return step;
            }
            touched = true;
            return {
              ...step,
              meta: {
                ...step.meta,
                decision,
                decidedAt: new Date().toISOString(),
                ...(remember ? { remembered: true } : {}),
              },
            };
          });
          return touched ? { ...m, unified_steps: next } : m;
        }),
      });

      const resolver = approvalResolversRef.current.get(requestId);
      if (resolver) {
        resolver(decision, remember);
        approvalResolversRef.current.delete(requestId);
        // also drop the session-ownership mapping kept for the REST fallback
        pendingApprovalSessionsRef.current.delete(requestId);
        // Clean up retry timer if approval is resolved
        const retryTimer = approvalRetryTimersRef.current.get(requestId);
        if (retryTimer) {
          clearInterval(retryTimer);
          approvalRetryTimersRef.current.delete(requestId);
        }
        // Clean up frontend timeout timer
        const timeoutTimer = approvalTimeoutTimersRef.current.get(requestId);
        if (timeoutTimer) {
          clearTimeout(timeoutTimer);
          approvalTimeoutTimersRef.current.delete(requestId);
        }
        setSessions((prev) => prev.map((s) => stampDecision(s, decision, remember)));
        return decision;
      }
      // Stale click — the resolver is already gone. The
      // backend has already auto-denied via the 5-minute
      // timeout. Do NOT flip the card to `approved` /
      // `denied`; mark it `expired` so the user can see
      // their click did not register, and can re-prompt.
      // Skip the downgrade if the card already has a real
      // decision from a previous live click.
      setSessions((prev) => prev.map((s) => stampDecision(s, "expired")));
      return null;
    },
    [],
  );

  const handleApproveApproval = useCallback(
    (requestId: string, remember?: boolean) => {
      const now = Date.now();
      console.info(`[APPROVAL-CLICK] id=${requestId} t=${now} remember=${remember === true}`);

      // removed the App.tsx `wsSendRaw(payload)` step that used
      // to live here. It was REDUNDANT with the main WS path in
      // `useStreamChat.ts:1175 liveWs.send(payload)`, which fires when the
      // `resolveApproval()` Promise below settles and which already guards
      // against stale WS references (only sends if `wsRef.current` is
      // OPEN). Sending the same payload from BOTH paths meant server-side
      // `ApprovalRegistry::resolve()` got called twice — first call
      // returned `matched=true` (the real success), second call returned
      // `matched=false` (idempotent miss) and surfaced as a confusing
      // red `[APPROVAL-REST] matched=false` in the console, which users
      // read as "the approval failed".
      // We now keep only:
      // 1. resolveApproval() → triggers the main WS path (liveWs.send
      // with proper wsRef OPEN guard)
      // 2. REST fallback 1s later → guarantees delivery even if the WS
      // silently dropped the message (the original "double insurance"
      // design intent). ApprovalRegistry::resolve() is idempotent so
      // REST-after-WS is harmless; REST-only success (matched=true
      // here) signals WS likely dropped the message and we want
      // that to be visible in logs so we can grep for it.
      // fixed two bugs in the REST path:
      // 1. PORT: was hardcoded to 9518 (wrong); the server listens on 3000
      // (api.ts:API_BASE). The old port was never in the Tauri CSP either,
      // so the fetch was silently blocked. Using api.apiUrl() guarantees the
      // correct host+port and stays in sync if the port ever changes.
      // 2. SESSION ID: was using activeSessionId (the session currently visible
      // in the chat pane), but the approval might belong to a different
      // session if the user switched tabs. Now reads the authoritative
      // session from pendingApprovalSessionsRef (set when the card was
      // created by onApprovalRequest), with activeSessionId as fallback.
      const restPayload = {
        request_id: requestId,
        // 「以后自动允许」→ wire `always` (server persists into the
        // durable allowlist); plain allow keeps the legacy string.
        decision: remember === true ? "always" : "approved",
      };
      const restSid =
        pendingApprovalSessionsRef.current.get(requestId) ?? activeSessionId;
      if (restSid) {
        setTimeout(async () => {
          try {
            const data = await api.fetchJSON<{ matched: boolean }>(
              `/v1/sessions/${encodeURIComponent(restSid)}/approval`,
              {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify(restPayload),
              },
            );
            // matched=true  → REST was the path that actually delivered
            // (bridge likely dropped the message — worth
            // keeping the loud console.error so we can
            // grep hermix-fresh.log / WebView2 console
            // to detect a recurrence).
            // matched=false → idempotent miss, the upstream channel
            // path already resolved the registry entry first. NOT
            // an error — log at info level so it
            // stops looking like a red failure in the
            // console.
            if (data.matched) {
              console.warn(
                `[APPROVAL-REST] id=${requestId} matched=true — REST delivered (upstream channel likely dropped the message)`,
              );
            } else {
              console.info(
                `[APPROVAL-REST] id=${requestId} matched=false — idempotent miss, upstream channel already resolved this request`,
              );
            }
          } catch (e) {
            console.error(`[APPROVAL-REST-FAIL] id=${requestId} error=${String(e)}`);
          }
        }, 1000);
      }

      // Resolve the promise immediately so the UI updates
      // AND triggers the main upstream channel path
      // (useStreamChat.ts connected-frame send).
      resolveApproval(requestId, "approved", remember);
    },
    [activeSessionId, resolveApproval],
  );

  const handleDenyApproval = useCallback(
    (requestId: string) => {
      console.info(`[APPROVAL-DENY] id=${requestId} t=${Date.now()}`);

      // mirror the approve-path cleanup — drop the redundant
      // wsSendRaw() step here too. The main upstream channel path in
      // useStreamChat.ts (the `.then(resp => ...)` after
      // `resolveApproval()`) handles sending `approval_response` over
      // the live `stream:<sid>` channel. The REST fallback below is
      // what actually catches silent message loss across the bridge.
      const restPayload = {
        request_id: requestId,
        decision: "denied",
      };
      const restSid =
        pendingApprovalSessionsRef.current.get(requestId) ?? activeSessionId;
      if (restSid) {
        setTimeout(async () => {
          try {
            const data = await api.fetchJSON<{ matched: boolean }>(
              `/v1/sessions/${encodeURIComponent(restSid)}/approval`,
              {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify(restPayload),
              },
            );
            if (data.matched) {
              console.error(
                `[APPROVAL-DENY-REST] id=${requestId} matched=true — REST delivered (upstream channel likely dropped the message)`,
              );
            } else {
              console.log(
                `[APPROVAL-DENY-REST] id=${requestId} matched=false — idempotent miss, upstream channel already resolved this request`,
              );
            }
          } catch (e) {
            console.error(`[APPROVAL-DENY-REST-FAIL] id=${requestId} error=${String(e)}`);
          }
        }, 1000);
      }

      // Resolve the promise immediately so the UI updates
      // AND triggers the main WS path (useStreamChat.ts:1175 liveWs.send
      // sends decision="denied" via the `.then(resp => ...)` chain).
      resolveApproval(requestId, "denied");
    },
    [activeSessionId, resolveApproval],
  );


  // -----------------------------------------------------------------
  // Batch approval resolution — counterpart of `resolveApproval`
  // for the WS-coalesced batch path. The single `decision` applies
  // to every item in the batch (no per-item granularity at the wire
  // level). The card's `meta.decision` flips in lock-step with the
  // server's per-item `resolve_approval_message` writes.
  // -----------------------------------------------------------------
  const resolveApprovalBatch = useCallback(
    (
      batchId: string,
      decision: Exclude<ApprovalDecision, null>,
      remember?: boolean,
    ): ApprovalDecision | null => {
      const stampBatchDecision = (
        s: Session,
        decision: ApprovalDecision,
        remember?: boolean,
      ): Session => ({
        ...s,
        messages: s.messages.map((m) => {
          const steps = m.unified_steps;
          if (!steps || steps.length === 0) return m;
          let touched = false;
          const next = steps.map((step): TimelineStep => {
            if (step.kind !== "approval_request_batch") return step;
            if (step.meta.batchId !== batchId) return step;
            if (step.meta.decision) return step;
            touched = true;
            return {
              ...step,
              meta: {
                ...step.meta,
                decision,
                decidedAt: new Date().toISOString(),
                ...(remember ? { remembered: true } : {}),
              },
            };
          });
          return touched ? { ...m, unified_steps: next } : m;
        }),
      });

      const resolver = approvalBatchResolversRef.current.get(batchId);
      if (resolver) {
        resolver(decision, remember);
        approvalBatchResolversRef.current.delete(batchId);
        pendingApprovalSessionsRef.current.delete(batchId);
        const retryTimer = approvalRetryTimersRef.current.get(batchId);
        if (retryTimer) {
          clearInterval(retryTimer);
          approvalRetryTimersRef.current.delete(batchId);
        }
        const timeoutTimer = approvalTimeoutTimersRef.current.get(batchId);
        if (timeoutTimer) {
          clearTimeout(timeoutTimer);
          approvalTimeoutTimersRef.current.delete(batchId);
        }
        setSessions((prev) =>
          prev.map((s) => stampBatchDecision(s, decision, remember)),
        );
        return decision;
      }
      // Stale click — batch already settled (timeout / resolve /
      // disconnect). Mark the card `expired` so the user sees the
      // honest "your click did not register" state.
      setSessions((prev) =>
        prev.map((s) => stampBatchDecision(s, "expired")),
      );
      return null;
    },
    [],
  );

  const handleApproveBatch = useCallback(
    (batchId: string, remember?: boolean) => {
      console.info(
        `[APPROVAL-BATCH-CLICK] id=${batchId} t=${Date.now()} remember=${remember === true}`,
      );
      // REST 1s fallback — same shape as the single-item path. The
      // backend `approve_via_rest` only reads `request_id`, so reuse it
      // with the batch id (resolved via the WS batch path; REST is a
      // benign idempotent miss if WS already handled it). Drop the old
      // `batch_id` key — it was rejected with HTTP 400 (missing request_id).
      const restPayload = {
        request_id: batchId,
        decision: remember === true ? "always" : "approved",
      };
      const restSid =
        pendingApprovalSessionsRef.current.get(batchId) ?? activeSessionId;
      if (restSid) {
        setTimeout(async () => {
          try {
            const data = await api.fetchJSON<{ matched?: boolean }>(
              `/v1/sessions/${encodeURIComponent(restSid)}/approval`,
              {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify(restPayload),
              },
            );
            if (data?.matched) {
              console.warn(
                `[APPROVAL-BATCH-REST] id=${batchId} matched=true — REST delivered`,
              );
            } else {
              console.info(
                `[APPROVAL-BATCH-REST] id=${batchId} matched=false — idempotent miss`,
              );
            }
          } catch (e) {
            console.error(
              `[APPROVAL-BATCH-REST-FAIL] id=${batchId} error=${String(e)}`,
            );
          }
        }, 1000);
      }
      resolveApprovalBatch(batchId, "approved", remember);
    },
    [activeSessionId, resolveApprovalBatch],
  );

  const handleDenyBatch = useCallback(
    (batchId: string) => {
      console.info(`[APPROVAL-BATCH-DENY] id=${batchId} t=${Date.now()}`);
      resolveApprovalBatch(batchId, "denied");
    },
    [resolveApprovalBatch],
  );

  /**
   * User answered a pending clarify card. Resolves the matching
   * `clarify_response` promise so the stream layer can send the
   * answer back over WebSocket, and stamps the card locally so the
   * resolved footer takes over without a server round-trip wait.
   */
  const handleAnswerClarify = useCallback((requestId: string, answer: string) => {
    const resolvers = clarifyResolversRef.current.get(requestId);
    if (resolvers && resolvers.length > 0) {
      resolvers.forEach((resolve) => resolve(answer));
      clarifyResolversRef.current.delete(requestId);
    }
    setSessions((prev) =>
      prev.map((s) => ({
        ...s,
        messages: s.messages.map((m) => {
          if (m.role !== "clarify" || m.clarify?.requestId !== requestId) return m;
          return {
            ...m,
            clarify: {
              ...(m.clarify ?? {
                requestId,
                question: m.content,
                choices: [],
              }),
              answer,
            },
          };
        }),
      })),
    );
  }, []);

  // ── ACUI directive state + decision bus ─────────────────────
  // The agent emits ACUI (Agent-Controlled UI) directives over the
  // streaming WebSocket (`{ type: "acui", directive: {...} }`). We
  // collect them into `acui.state`, then `ChatView` renders the active
  // cards via `<AcuiPanel state={acui.state} decide={acuiBus} />`.
  // The decision bus bridges approval/clarify card clicks back into
  // the same code path as inline cards: when an ACUI approval card is
  // clicked, `bus.approve(requestId)` calls `handleApproveApproval`,
  // which resolves the WS `RequestApproval` promise — the stream
  // layer then sends `approval_response` to the server.
  const acui = useAcuiState();
  const acuiBus = useMemo<AcuiDecisionBus>(
    () =>
      acui.bind({
        approve: (id) => handleApproveApproval(id),
        deny: (id) => handleDenyApproval(id),
        answer: (id, a) => handleAnswerClarify(id, a),
      }),
    // All three handlers are stable useCallbacks, so this useMemo
    // runs once. We list them for correctness in case a future
    // refactor swaps one for a non-memoized closure.
    [acui, handleApproveApproval, handleDenyApproval, handleAnswerClarify],
  );

  const handleSendMessage = useCallback(async (
    content: string,
    _projectId?: string,
    overrides?: import("./hooks/useStreamChat").ChatOverrides,
    attachments?: { images?: QueuedImage[]; attachedFilePaths?: string[] },
  ) => {
    let sessionId = activeSessionId;
    if (sessionId && !sessions.some((s) => s.id === sessionId)) { sessionId = null; setActiveSessionId(null); }
    // Clear any previous stream error for the *current* session — the
    // user is starting a new turn, so the old error is stale. If this
    // turn also fails the error will be re-set by the onError callback
    // (or the createSession catch below for the welcome path).
    clearStreamError(sessionId);
    if (!sessionId) {
      // createSession() calls the HTTP server; if the sidecar isn't
      // running (or hermix.exe failed to spawn — see sidecar.rs
      // path resolution) it throws. Wrap it so the user sees a
      // visible error instead of a click that "did nothing".
      const title = content.slice(0, 40) + (content.length > 40 ? "..." : "");
      let session;
      try {
        // pass the welcome-state draft workspace so the
        // server records it on the session row from creation
        // (instead of relying on a localStorage-only fallback that
        // vanishes on desktop restart).
        const draftWs = readDraftWorkspace();
        session = await api.createSession(title, draftWs ?? undefined);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        // 不要把所有失败都说成"后端未启动或不可达"：会话创建失败的成因
        // 至少有三类 —— 后端真不可达（网络）、请求被拒（4xx，协议/参数
        // 问题）、服务端异常（5xx）。把它们混成一句"未启动"会把一个
        // 确定性的 422 说成环境问题，用户按提示去查端口永远治不好。
        // 按分类给出对应处置，原文压进 detail 供复制排障。
        setStreamError(null, {
          message: localizeStreamError(msg),
          kind: classifyStreamErrorKind(msg),
          detail: msg,
        });
        return;
      }
      setSessions((prev) => [session, ...prev]);
      setActiveSessionId(session.id);
      sessionId = session.id;

      // Migrate welcome-state draft selections (expert / workspace)
      // into the new session's per-session storage. Without this, the
      // moment ChatView re-renders with the real sessionId, its
      // `useSessionExpert` / `useSessionWorkspace` hooks would read
      // from an empty `hermix.session.<id>.*` slot and the chip text
      // would flip from "专家：xxx" / "目录：xxx" back to the bare
      // placeholders — exactly the regression the user reported.
      try {
        const draftExpert = storage.getItem(STORAGE_KEYS.draftExpertId);
        if (draftExpert) {
          storage.setItem(sessionKey(session.id, "expertId"), draftExpert);
        }
      } catch {}
      try {
        const draftWorkspace = storage.getItem(STORAGE_KEYS.draftWorkspace);
        if (draftWorkspace) {
          storage.setItem(sessionKey(session.id, "workspace"), draftWorkspace);
        }
      } catch {}
    }

// Reset per-turn state.
runningToolRef.current.clear();
setRunningToolCount(0);
streamPhaseRef.current = null;

    // Surface any attached images in the user bubble. We reuse the
    // in-memory `blobUrl` from the attachment so the preview the user
    // just saw in the input box appears verbatim in the sent message.
    const userImages: ChatMessageImage[] | undefined = attachments?.images?.map((i) => ({
      blobUrl: i.blobUrl,
      mimeType: i.mimeType,
      name: i.name,
    }));
    const userMsg: ChatMessage = {
      id: `temp-${Date.now()}`,
      role: "user",
      content,
      timestamp: new Date().toISOString(),
      ...(userImages && userImages.length ? { images: userImages } : {}),
    };
    const assistantId = `reply-${Date.now()}`;
    // 上一个 assistant 消息的流已结束，先把它的 unified_steps 持久化，
    // 否则多轮 agent loop 中中间轮的 inline 内联顺序会在 reload 时回退成
    // buildFallbackSteps 的固定 reasoning→tools→text（工具被归到末尾）。
    const prevAssistantId = currentAssistantIdRef.current;
    if (prevAssistantId) {
      const prevMsg = sessionsRef.current
        .find((s) => s.id === sessionId)
        ?.messages.find((m) => m.id === prevAssistantId);
      if (prevMsg?.unified_steps && prevMsg.unified_steps.length > 0) {
        void api.saveUnifiedSteps(sessionId, prevMsg.unified_steps);
      }
    }
    currentAssistantIdRef.current = assistantId;

    // Persist the auto-generated title to the backend fire-and-forget, so
    // a session-list reload (e.g. on next app launch) keeps the title the
    // user just gave it via their first message — instead of resetting to
    // the placeholder "新建对话" the createSession() call seeded.
    const currentSession = sessions.find((s) => s.id === sessionId);
    if (currentSession) {
      const newTitle = computeAutoTitle(currentSession, content);
      if (newTitle !== currentSession.title) {
        api.renameSession(sessionId, newTitle).catch((e) =>
          console.error("renameSession failed", e)
        );
      }
    }

    // 根治 request_approval race：placeholder 创建必须 sync commit。
    // 旧逻辑 setSessions 创建 placeholder 后 await api.sendMessage，期间
    // React batching 把 setState 推迟 commit。server < 1ms 内回
    // request_approval → WS chunk 到达 → onApprovalRequest 跑
    // setSessions 的 prev 还没包含 assistantId → approval step 永远建不上。
    // flushSync 强制 React 同步 commit —— placeholder 立刻进 sessions state，
    // 任何后续 setSessions updater（onApprovalRequest 的 step 写入、appendChunk
    // 的文本写入）拿到的 prev 都包含这条 assistant message。
    // 这是用户发消息瞬间的一次额外同步 commit，< 1ms 用户不可感知。
    // flushSync 不触发 strict mode 双调用（它的二次执行只针对 render）。
    flushSync(() => {
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        // Insert user message + empty assistant placeholder
        return {
          ...s,
          messages: [
            ...s.messages,
            userMsg,
            { id: assistantId, role: "assistant", content: "", timestamp: new Date().toISOString() },
          ],
          updated_at: new Date().toISOString(),
          title: computeAutoTitle(s, content),
        };
      }));
    });

    // Fold attachment-derived overrides (image bytes + file paths) into
    // the overrides object sent to the backend. Centralised here so both
    // the direct-send and the queue-drain paths use the same conversion.
    const finalOverrides: import("./hooks/useStreamChat").ChatOverrides = { ...(overrides ?? {}) };
    if (attachments?.images?.length) {
      finalOverrides.images = await Promise.all(
        attachments.images.map(async (img) => ({
          bytes: Array.from(new Uint8Array(await img.file.arrayBuffer())),
          mimeType: img.mimeType,
          name: img.name,
        })),
      );
    }
    if (attachments?.attachedFilePaths?.length) {
      finalOverrides.attachedFilePaths = attachments.attachedFilePaths;
    }

    // Real-time stream: append each chunk to the *currently active*
    // assistant message. removed needsNewAssistantRef
    // turn-split logic — all text in an agent loop now goes to ONE
    // assistant message. Tool calls are tracked in the assistant's
    // `execution_timeline` (rendered as a collapsible block) instead
    // of separate `role: "tool"` messages. This ensures streaming
    // and reload produce identical visuals.
    const appendChunk = (chunk: string) => {
      const target = currentAssistantIdRef.current;
      if (!target) return;
      streamPhaseRef.current = "text";
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) =>
            m.id === target
              ? { ...m, content: m.content + chunk, unified_steps: appendTextStep(m.unified_steps, chunk) }
              : m
          ),
          updated_at: new Date().toISOString(),
        };
      }));
    };

    // finalizeToolMessage removed. Tool completion is now
    // tracked via `tool_complete` events in the assistant's
    // `execution_timeline`, not via separate `role: "tool"` messages.

    /**
     * The server has emitted a `RequestApproval` event. Three steps:
     *   1. Freeze the currently-streaming assistant message (just
     *      stop routing chunks to it — its content is what it is)
     *   2. Insert an approval message in the slot right after it,
     *      plus a fresh empty assistant placeholder that subsequent
     *      chunks will land on
     *   3. Return a promise that resolves when the user clicks
     *      allow/deny on the inline card. The stream layer
     *      immediately forwards the decision to the server.
     */
    const onApprovalRequest = (req: {
      requestId: string;
      tool: string;
      args: unknown;
      riskLevel?: string;
      /** Wall-clock deadline (Unix ms) from the server. Persisted
       *  into the approval meta and fed to `<InlineApprovalCard>`
       *  so it can render a live "剩余 4m 32s" countdown against
       *  the *original* server-side deadline (not a fresh
       *  re-derivation, which would briefly lie across a reload). */
      expiresAt?: number | null;
      /** LLM-supplied one-line rationale (terminal schema `reason`). */
      reason?: string;
      sessionId: string;
    }): Promise<{ decision: ApprovalDecision; remember?: boolean }> => {
      // The approval card belongs to the *current* assistant turn —
      // the agent is blocked waiting for the user, not starting a new
      // turn. We deliberately do NOT create a new assistant placeholder
      // here (that was the old behavior, and it produced a fresh
      // avatar + divider for every approval — 10 approvals = 10 new
      // conversation segments).
      //
      // Sequence-of-flow path (2026-08-18 refactor): the card is
      // rendered inline as a `kind: "approval_request"` step inside
      // the assistant's `unified_steps`, at the chronological
      // position of its `tool_call_id` (right after `tool_start`,
      // before `tool_complete`). `resolveApproval()` mutates the
      // same step's `meta.decision`/`decidedAt` to flip the card
      // to its resolved one-line summary.
      //
// NOTE: do NOT advance `currentAssistantIdRef` here — the
      // resumed LLM stream must target the same assistant.
      //
      // The ref points at the assistant placeholder of the current
      // turn. Set synchronously by `sendStream` (App.tsx:1230) BEFORE
      // any WS chunk can arrive. The placeholder itself is created
      // under `flushSync` (App.tsx:1247) so it is guaranteed to be
      // in `sessions` state by the time `request_approval` arrives
      // — no React batching race, no fallback needed.
      const previousAssistantId =
        currentAssistantIdRef.current
        ?? [...(sessions.find((s) => s.id === sessionId)?.messages ?? [])]
          .reverse()
          .find((m) => m.role === "assistant")?.id
        ?? null;
      if (!previousAssistantId) {
        // No assistant message target — the approval step would be
        // dropped. Keep the server blocked (never resolve the promise)
        // so the tool doesn't run without an explicit decision.
        return new Promise<{ decision: ApprovalDecision }>(() => {});
      }
      const meta: ApprovalMessageMeta = {
        requestId: req.requestId,
        tool: req.tool,
        args: req.args,
        riskLevel: req.riskLevel,
        decision: null,
        decidedAt: null,
        reason: req.reason ?? null,
        // Persist the server-emitted deadline so a page reload
        // re-renders the countdown against the *original*
        // server-side timeout (not a fresh `Date.now()+5min`
        // re-derivation, which would briefly lie — the back-end
        // may have already auto-denied while the page was
        // reloading). See `ApprovalMessageMeta::expiresAt` in
        // `types.ts`.
        expiresAt: req.expiresAt ?? null,
        linkedAssistantId: previousAssistantId,
      };
      const step: TimelineStep = {
        kind: "approval_request",
        meta,
        toolCallId: req.requestId,
        createdAt: new Date().toISOString(),
      };
      // Push the approval meta as a `kind: "approval_request"`
      // step into the assistant's existing `unified_steps`. Legacy
      // sessions persisted as a standalone `role: "approval"`
      // message keep their rendering through
      // `MessageBubble`'s backward-compat branch; new approvals
      // never create that standalone message.
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        const messages = s.messages.map((m) => {
          if (m.id !== previousAssistantId) return m;
          const existing = m.unified_steps ?? [];
          // De-dup — both live re-emits and accidental double-fires
          // fall through: appending another pending step for the
          // same requestId would double the rendered card.
          if (existing.some(
            (x) => x.kind === "approval_request" && x.meta.requestId === req.requestId,
          )) return m;
          return {
            ...m,
            unified_steps: [...existing, step],
          };
        });
        return {
          ...s,
          messages,
          updated_at: new Date().toISOString(),
        };
      }));

    // Set a frontend timeout to auto-mark as "expired" when the
    // server-emitted deadline passes. The backend already auto-denies
    // at 30s, but without this timer the card UI stays "等待你的决定"
    // forever — the user sees buttons that no longer do anything.
    if (req.expiresAt) {
      const delay = Math.max(0, req.expiresAt - Date.now());
      const timer = setTimeout(() => {
        approvalTimeoutTimersRef.current.delete(req.requestId);
        resolveApproval(req.requestId, "expired");
      }, delay);
      approvalTimeoutTimersRef.current.set(req.requestId, timer);
    }

    return new Promise<{ decision: ApprovalDecision; remember?: boolean }>((resolve) => {
      approvalResolversRef.current.set(
        req.requestId,
        (decision, remember) => resolve({ decision, remember }),
      );
      // record which session owns this request so the REST fallback hits the
      // right `/v1/sessions/<sid>/approval` endpoint
      pendingApprovalSessionsRef.current.set(req.requestId, sessionId);
    });
    };

    /**
     * Server coalesced N parallel approvals into one batch envelope
     * (see `crates/hermix-server::coalescer::ApprovalCoalescer`).
     * Render one card with N sub-items; resolve all with a single
     * `ApprovalBatchResponse` decision. Mirrors `onApprovalRequest`
     * but the meta carries a `batchId` instead of a single
     * `requestId`, and the deadline timer is taken from the EARLIEST
     * expires_at (any later item with shorter remaining time will
     * already have been auto-denied on the server; we render the
     * loosest deadline to match what the server commits to).
     */
    const onApprovalBatchRequest = (req: {
      batchId: string;
      items: import("./types").ApprovalRequestItem[];
      sessionId: string;
    }): Promise<{ decision: ApprovalDecision; remember?: boolean }> => {
      const previousAssistantId =
        currentAssistantIdRef.current
        ?? [...(sessions.find((s) => s.id === req.sessionId)?.messages ?? [])]
          .reverse()
          .find((m) => m.role === "assistant")?.id
        ?? null;
      if (!previousAssistantId) {
        // No assistant target — keep the server blocked.
        return new Promise<{ decision: ApprovalDecision }>(() => {});
      }
      const batchMeta: ApprovalBatchMessageMeta = {
        batchId: req.batchId,
        items: req.items,
        decision: null,
        decidedAt: null,
      };
      const step: TimelineStep = {
        kind: "approval_request_batch",
        meta: batchMeta,
        // Anchor the batch to the FIRST item's request_id for the
        // `toolCallId` linkage (used by the live cursor highlight
        // in the tool timeline). Not all items share the same id.
        toolCallId: req.items[0]?.requestId ?? req.batchId,
        createdAt: new Date().toISOString(),
      };
      setSessions((prev) => prev.map((s) => {
        if (s.id !== req.sessionId) return s;
        const messages = s.messages.map((m) => {
          if (m.id !== previousAssistantId) return m;
          const existing = m.unified_steps ?? [];
          if (existing.some(
            (x) =>
              x.kind === "approval_request_batch"
              && x.meta.batchId === req.batchId,
          )) return m;
          return { ...m, unified_steps: [...existing, step] };
        });
        return {
          ...s,
          messages,
          updated_at: new Date().toISOString(),
        };
      }));

      // Frontend timeout against the EARLIEST expires_at among items —
      // if any item would auto-deny server-side, the whole batch
      // collapses once we mark it `expired`. Server races will keep
      // the registry honest regardless; this is the UI-only honest
      // reflection.
      const earliestExpires = req.items
        .map((it) => it.expiresAt)
        .filter((v): v is number => typeof v === "number")
        .reduce<number | null>((acc, v) => (acc === null || v < acc ? v : acc), null);
      if (earliestExpires !== null) {
        const delay = Math.max(0, earliestExpires - Date.now());
        const timer = setTimeout(() => {
          approvalTimeoutTimersRef.current.delete(req.batchId);
          resolveApprovalBatch(req.batchId, "expired");
        }, delay);
        approvalTimeoutTimersRef.current.set(req.batchId, timer);
      }

      return new Promise<{ decision: ApprovalDecision; remember?: boolean }>((resolve) => {
        approvalBatchResolversRef.current.set(
          req.batchId,
          (decision, remember) => resolve({ decision, remember }),
        );
        pendingApprovalSessionsRef.current.set(req.batchId, req.sessionId);
      });
    };

    const onClarifyRequest = (req: {
      requestId: string;
      question: string;
      choices: string[];
      sessionId: string;
    }): Promise<{ answer: string }> => {
      // Defensive dedup: the backend clarify pump re-broadcasts pending
      // requests when new ones arrive, and WebSocket reconnects can replay
      // events. If we already have a card for this requestId, just wire the
      // new promise to the existing resolver(s) instead of spawning another
      // identical card.
      const existing = sessions.find((s) => s.id === sessionId)?.messages.some(
        (m) => m.role === "clarify" && m.clarify?.requestId === req.requestId
      );

      if (!existing) {
        // Same fix as onApprovalRequest: clarify cards belong to the
        // current assistant turn — do not spawn a fresh assistant
        // placeholder. Resumed LLM chunks continue on the existing
        // `currentAssistantIdRef.current`.
        // NOTE: do NOT advance `currentAssistantIdRef` here.

        const clarifyMsg: ChatMessage = {
          id: `clarify-${req.requestId}`,
          role: "clarify",
          content: req.question,
          timestamp: new Date().toISOString(),
          clarify: {
            requestId: req.requestId,
            question: req.question,
            choices: req.choices ?? [],
            answer: null,
          },
        };

        setSessions((prev) => prev.map((s) => {
          if (s.id !== sessionId) return s;
          return {
            ...s,
            messages: [...s.messages, clarifyMsg],
            updated_at: new Date().toISOString(),
          };
        }));
      }

      return new Promise<{ answer: string }>((resolve) => {
        const list = clarifyResolversRef.current.get(req.requestId) ?? [];
        list.push((answer) => resolve({ answer }));
        clarifyResolversRef.current.set(req.requestId, list);
      });
    };

    try {
      await sendStream(
        sessionId!,
        content,
        {
          onPhaseChange: (phase) => {
            // Only store the phase for the active session. If the user
            // switches sessions mid-stream, the new session shouldn't
            // inherit the old phase chip.
            if (sessionId === activeSessionId) {
              prevPhase.current = phase;
              setStreamPhase(phase);
            }
          },
          onChunk: appendChunk,
          onError: (err, detail, code) => {
            // The error frame arrives immediately (no API key,
            // config error, etc.) — React may batch the
            // setStreamingSessionId(sessionId)+null into one render
            // so the catch block below may be too late. Handle here.
            // (React 18 batching bug fix): capture
            // currentAssistantIdRef.current into a local variable
            // BEFORE calling setSessions. React 18's automatic
            // batching defers the setSessions updater to the next
            // render cycle, but `currentAssistantIdRef.current = null`
            // on the next line executes immediately. Without this
            // capture, the updater reads null and no message gets
            // the error text — the user sees the spinner disappear
            // with no error shown in the assistant bubble.
            const assistantIdAtError = currentAssistantIdRef.current;
            // Prefer the raw backend error over the canned "后端错误"
            // wording when it's available. `detail` carries the LLM
            // provider's actual response (401 "invalid api_key",
            // 402 "quota exceeded", 429 "rate limit", 5xx body, fetch
            // failure, etc.) — running it through `localizeStreamError`
            // extracts a clean Chinese hint like "API key 无效" or
            // "网络异常", giving the user a categorisable failure
            // instead of an undifferentiated banner.
            //
            // When the backend already sent a structured `Error` event,
            // `err` is the pre-localized Chinese message (e.g. "AI 服务
            // 过载: ...") and `detail` is the raw English provider
            // response. Re-localizing `detail` would overwrite the
            // friendly message, so we only run `localizeStreamError`
            // when `err` still looks like a raw provider/transport
            // error string.
            const rawForFriendly = err || detail || "";
            const looksRaw = /API error|LLM API error|unknown status code|overloaded|rate limit|timeout|connection refused|unauthorized|forbidden|401|403|429|5\d{2}/i.test(rawForFriendly);
            const friendly = looksRaw ? localizeStreamError(detail ?? err) : rawForFriendly;
            const errText = friendly;
            // Promote the (possibly coarse) `code` shipped by the
            // backend's `user_facing_agent_error` into the
            // StreamErrorCard's category taxonomy. When the backend
            // didn't classify (transport-only errors, WS onclose
            // paths, raw provider rejects) fall back to local
            // pattern matching on the human-readable strings so the
            // user always sees a meaningful category instead of
            // "对话因后端错误中断".
            const kind = normalizeStreamErrorKind(code, rawForFriendly);
            setStreamPhase(null);
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === assistantIdAtError
                    ? { ...m, content: m.content || errText, stopReason: "error" as const }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
            currentAssistantIdRef.current = null;
            if (sessionId === activeSessionId) {
              // Keep the friendly localized message as the primary
              // text; stash the raw backend error as `detail` so the
              // error card can show it as a collapsible "技术详情".
              setStreamError(sessionId, { message: friendly, detail, kind });
            }
            // Clear live sub-agent panel on error — the stream is
            // dead, no more sub-agent events will arrive, and
            // leaving stale "执行中" entries would confuse the user.
            setSubAgents({});
            // ★ FIX: clear the "正在执行 N 个工具…" indicator.
            // onDone's stranded-tools clear only fires on the normal
            // path; onError (backend reject / WS error) skipped it,
            // so any tool still in `runningToolRef` would leave the
            // counter stale and the indicator stuck at "执行中"
            // even after the stream is dead. Now we wipe both.
            runningToolRef.current.clear();
            setRunningToolCount(0);
          },
          onToolStart: ({ toolCallId, tool, content }) => {
            // Tool events are now appended to the
            // assistant's `execution_timeline` instead of creating
            // separate `role: "tool"` messages. This ensures:
            // 1. Tools always render BELOW the avatar (inside the
            // assistant bubble, not as separate messages above it)
            // 2. Streaming and reload produce identical visuals
            // 3. Tools can be rendered as a collapsible block
            if (runningToolRef.current.has(toolCallId)) {
              return;
            }
            runningToolRef.current.set(toolCallId, toolCallId);
            setRunningToolCount(runningToolRef.current.size);

            const target = currentAssistantIdRef.current;
            if (!target) return;
            streamPhaseRef.current = "tools";

            const event: import("./types").ExecutionTimelineEvent = {
              type: "tool_start",
              tool,
              arguments: content,
              tool_call_id: toolCallId,
              created_at: new Date().toISOString(),
            };

            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === target
                    ? {
                        ...m,
                        execution_timeline: [...(m.execution_timeline ?? []), event],
                        unified_steps: appendToolStep(m.unified_steps, event),
                      }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onToolChunk: ({ toolCallId, content }) => {
            // Append stdout to the assistant's
            // execution_timeline. We accumulate into a single
            // `tool_stdout` event per tool_call_id for efficiency.
            if (!content) return;
            const target = currentAssistantIdRef.current;
            if (!target) return;

            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target || !m.execution_timeline) return m;
                  const timeline = [...m.execution_timeline];
                  // Find the existing tool_stdout event for this tool_call_id
                  let found = false;
                  for (let i = timeline.length - 1; i >= 0; i--) {
                    if (timeline[i].type === "tool_stdout" && timeline[i].tool_call_id === toolCallId) {
                      timeline[i] = { ...timeline[i], content: (timeline[i].content ?? "") + content };
                      found = true;
                      break;
                    }
                  }
                  if (!found) {
                    timeline.push({
                      type: "tool_stdout",
                      tool_call_id: toolCallId,
                      content,
                      created_at: new Date().toISOString(),
                    });
                  }
                  return {
                    ...m,
                    execution_timeline: timeline,
                    unified_steps: updateToolStepStdout(m.unified_steps, toolCallId, content),
                  };
                }),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onToolComplete: ({ toolCallId, tool, content, denialKind, isError }) => {
            // 工具调用被拒绝时（用户拒绝 / 超时 / 连接中断）弹顶部横幅，
            // 对应 agent 注入的"告诉用户"提示。`"policy"` 是静默硬拦截，跳过。
            if (denialKind && denialKind !== "policy") {
              showDenialBanner(denialKind, tool);
            }
            // Append a `tool_complete` event to the
            // assistant's execution_timeline. No separate tool message,
            // no needsNewAssistantRef — all text stays in the same
            // assistant bubble.
            runningToolRef.current.delete(toolCallId);
            setRunningToolCount(runningToolRef.current.size);

            const target = currentAssistantIdRef.current;
            if (!target) return;

            const event: import("./types").ExecutionTimelineEvent = {
              type: "tool_complete",
              tool_call_id: toolCallId,
              content: content ?? "",
              created_at: new Date().toISOString(),
              // Server-authoritative failure flag; persisted so a reload
              // marks the card failed without inspecting the payload text.
              is_error: isError,
            };

            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === target
                    ? {
                        ...m,
                        execution_timeline: [...(m.execution_timeline ?? []), event],
                        unified_steps: appendToolStep(m.unified_steps, event),
                      }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onFileChange: ({ toolCallId, filePath, changeType, fileSummary }) => {
            // Append a `file_change` event to the currently-active
            // assistant message's `execution_timeline`. The chat UI
            // (FileChangeBlock in MessageBubble) reads this timeline
            // via extractFileChanges() and renders a WorkBuddy-style
            // "文件变更 (N个)" row inline in the conversation.
            const target = currentAssistantIdRef.current;
            if (!target) return;
            const event: import("./types").ExecutionTimelineEvent = {
              type: "file_change",
              tool: "file_change",
              file_path: filePath,
              change_type: changeType,
              file_summary: fileSummary,
              created_at: new Date().toISOString(),
            };
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  // De-dupe: same file_path + change_type only
                  // appears once even if the server re-emits.
                  const exists = timeline.some(
                    (e) =>
                      e.type === "file_change" &&
                      e.file_path === filePath &&
                      e.change_type === changeType,
                  );
                  if (exists) return m;
                  return {
                    ...m,
                    execution_timeline: [...timeline, event],
                  };
                }),
              };
            }));
            // toolCallId is accepted for symmetry with other event
            // handlers; file_change events are de-duped by path so
            // the id is informational only here.
            void toolCallId;
          },
          // append a `memory_update` event to the active
          // assistant message's `execution_timeline` (falling back to
          // the last assistant message when none is active — e.g. at
          // session end). The chat UI (MemoryUpdateBlock in
          // MessageBubble) reads this timeline via extractMemoryUpdates()
          // and renders an inline "记忆" row so the user can see what
          // the agent is remembering. De-dupe by (action, key) so a
          // re-emitted event doesn't produce a duplicate row.
          onMemoryUpdate: ({ action, memoryType, key, summary, count }) => {
            if (!summary && !key) return;
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              // Prefer the currently-streaming assistant message; if
              // none (session-end summary), use the last assistant msg.
              const target =
                currentAssistantIdRef.current ??
                [...s.messages].reverse().find((m) => m.role === "assistant")?.id ??
                null;
              if (!target) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  const exists = timeline.some(
                    (e) =>
                      e.type === "memory_update" &&
                      e.memory_action === action &&
                      e.memory_key === key,
                  );
                  if (exists) return m;
                  const event: import("./types").ExecutionTimelineEvent = {
                    type: "memory_update",
                    tool: "memory_update",
                    memory_action: action,
                    memory_type: memoryType,
                    memory_key: key,
                    memory_summary: summary,
                    memory_count: count,
                    created_at: new Date().toISOString(),
                  };
return {
                ...m,
                execution_timeline: [...timeline, event],
              };
            }),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          // 记忆召回 — 用"💭 想到 N 条"内联 chip 提示 agent 重新拉取了相关事实。
          // 按 (trigger, turn_number) 去重，因为漂移召回可能在相近时刻触发。
          onMemoryRecall: ({ trigger, count, totalChars, previewKeys, turnNumber }) => {
            if (count === 0) return;
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              const target =
                currentAssistantIdRef.current ??
                [...s.messages].reverse().find((m) => m.role === "assistant")?.id ??
                null;
              if (!target) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  // Same (trigger, turn) → idempotent (drift
                  // recalls can land in adjacent events).
                  const exists = timeline.some(
                    (e) =>
                      e.type === "memory_recall" &&
                      e.memory_recall_trigger === trigger &&
                      e.memory_recall_turn_number === turnNumber,
                  );
                  if (exists) return m;
                  const event: import("./types").ExecutionTimelineEvent = {
                    type: "memory_recall",
                    tool: "memory_recall",
                    memory_recall_trigger: trigger,
                    memory_recall_count: count,
                    memory_recall_total_chars: totalChars,
                    memory_recall_preview_keys: previewKeys,
                    memory_recall_turn_number: turnNumber,
                    created_at: new Date().toISOString(),
                  };
                  return {
                    ...m,
                    execution_timeline: [...timeline, event],
                  };
                }),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          // 追加一条 `section_gate` event 到当前 assistant 消息的
          // `execution_timeline`，使 trace 面板
          // can render the `SectionGateRow` inline with the
          // tool cards. Mirrors `onFileChange`'s shape: locate
          // the current assistant message via the ref, mutate
          // the session state, persist through `execution_timeline`.
          onSectionGate: (info) => {
            const target = currentAssistantIdRef.current;
            if (!target) return;
            const event: import("./types").ExecutionTimelineEvent = {
              type: "section_gate",
              tool: "section_gate",
              content: `SectionGate: patched=${info.patched.length} pending=${info.pending.length}`,
              created_at: new Date().toISOString(),
              section_gate_report: info,
            };
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  return {
                    ...m,
                    execution_timeline: [...timeline, event],
                  };
                }),
              };
            }));
          },
          onReasoningChunk: (piece) => {
            // Live `<think>...</think>` text from the LLM. Append to
            // the currently-active assistant message's
            // `reasoningContent` so the folding block in
            // MessageBubble can render it (collapsed by default).
            const target = currentAssistantIdRef.current;
            if (!target) return;
            streamPhaseRef.current = "reasoning";
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  // Retroactive text→reasoning conversion:
                  // When the backend detects untagged reasoning
                  // mid-stream (stream_collect.rs
                  // `looks_like_untagged_reasoning`), the first few
                  // chunks were already sent as `StreamEvent::Text`
                  // and accumulated as `text` steps in
                  // `unified_steps`. Now that a `StreamEvent::Thinking`
                  // has arrived, we know those text steps were actually
                  // reasoning. Convert the trailing text step(s) to a
                  // reasoning step so the UI folds them inside
                  // ProcessTimeline instead of showing as body text.
                  // Also move the text from `content` to
                  // `reasoningContent` so the persisted state matches.
                  let steps = m.unified_steps ? [...m.unified_steps] : [];
                  let contentFix = m.content;
                  let reasoningFix = m.reasoningContent ?? "";
                  // Walk backwards: convert trailing text steps to
                  // reasoning. Stop at the first non-text step (tools,
                  // existing reasoning) — those are real phase
                  // boundaries.
                  const convertedTexts: string[] = [];
                  for (let i = steps.length - 1; i >= 0; i--) {
                    if (steps[i].kind !== "text") break;
                    const textContent = (steps[i] as { content: string }).content;
                    convertedTexts.unshift(textContent);
                    // Remove this text step — it'll be re-added as
                    // reasoning via appendReasoningStep below.
                    steps.splice(i, 1);
                  }
                  if (convertedTexts.length > 0) {
                    // Rebuild the visible body from the text steps that
                    // REMAIN after the conversion instead of stripping a
                    // suffix off `content`.
                    //
                    // `appendChunk` writes every chunk to BOTH
                    // `message.content` and a `text` step, so the two are
                    // always the same string — but suffix (`endsWith`)
                    // matching silently failed whenever they diverged
                    // (sanitizer trims, interleaved tool steps), leaving
                    // the same text rendered as BOTH body and reasoning.
                    // At this point the converted steps have already been
                    // spliced out, so joining what's left is exact and
                    // needs no string matching at all.
                    contentFix = steps.reduce(
                      (acc, s) =>
                        s.kind === "text"
                          ? acc + (s as { content: string }).content
                          : acc,
                      "",
                    );
                    reasoningFix = convertedTexts.join("") + reasoningFix;
                  }
                  // Now append the new reasoning piece.
                  steps = appendReasoningStep(steps, piece);
                  return {
                    ...m,
                    content: contentFix,
                    reasoningContent: reasoningFix + piece,
                    unified_steps: steps,
                  };
                }),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onSubAgentEvent: (info: SubAgentEventPayload) => {
            // Accumulate live sub-agent state keyed by subAgentId so the
            // "子代理" panel can show each delegated task's phase in real
            // time (spawned → started → done / failed). `recentSteps`
            // (rolling tool/thinking log) replaces wholesale on each
            // event — the backend sends the full newest-last slice.
            if (!info?.subAgentId) return;
            setSubAgents((prev) => ({
              ...prev,
              [info.subAgentId]: {
                subAgentId: info.subAgentId,
                phase: info.phase,
                task: info.task,
                role: info.role,
                turns: info.turns,
                costUsd: info.costUsd,
                error: info.error,
                recentSteps: info.recentSteps,
                updatedAt: Date.now(),
              },
            }));
          },
          onApprovalRequest,
          onApprovalBatchRequest,
          onClarifyRequest,
          // 用户点击停止时（cancelStream）在 WS 关闭前触发：直接把仍在途的
          // 审批请求以 denied 否决发往后端，使 agent loop 立即解除阻塞，而非
          // 干等其 300s 审批超时。clarify 同理以空答案解除阻塞。
          onCancel: () => {
            approvalResolversRef.current.forEach((_resolver, requestId) => {
              const sent = wsSendRaw(
                JSON.stringify({
                  type: "approval_response",
                  request_id: requestId,
                  decision: "denied",
                }),
              );
              if (!sent) {
                console.error(`[CANCEL-APPROVAL] ws not open, drop deny id=${requestId}`);
              }
            });
            approvalResolversRef.current.clear();
            // Batch approvals: same "cancel ⇒ deny all" semantics
            // but wire a single batch frame so the server resolves
            // every member under that batch_id.
            approvalBatchResolversRef.current.forEach((_resolver, batchId) => {
              const sent = wsSendRaw(
                JSON.stringify({
                  type: "approval_responses_batch",
                  batch_id: batchId,
                  decision: "denied",
                }),
              );
              if (!sent) {
                console.error(`[CANCEL-APPROVAL-BATCH] ws not open, drop deny id=${batchId}`);
              }
            });
            approvalBatchResolversRef.current.clear();
            approvalRetryTimersRef.current.forEach((t) => clearInterval(t));
            approvalRetryTimersRef.current.clear();
            approvalTimeoutTimersRef.current.forEach((t) => clearTimeout(t));
            approvalTimeoutTimersRef.current.clear();
            pendingApprovalSessionsRef.current.clear();
            // clarify：直接以空答案解除后端 tool call 阻塞（不重发 WS，
            // 由后端超时兜底；仅清 map 防泄漏）。
            clarifyResolversRef.current.forEach((resolvers) =>
              resolvers.forEach((resolve) => resolve("")),
            );
            clarifyResolversRef.current.clear();
            acui.reset();
          },
          onAcuiDirective: (directive) => acui.apply(directive),
          // Todo (task list) mutations. Forward to the App-level
          // groups store so each plan renders as its own inline
          // card anchored to the originating message. The store
          // updates `todoGroupsByIdRef` synchronously and bumps
          // `todoGroups` state once per logical mutation (group
          // start / upsert / remove) so ChatView re-renders
          // exactly the affected card.
          onTodoChange: (change) => {
          // 客户端会话守卫：WS 已按连接过滤 session_id，但 TodoPanel 仍收到线路上
          // 所有事件；若将来某路径绕过过滤，错误会话的 plan 卡片会渲染到错误的
          // assistant 消息上，故此处再按 change.sessionId 二次校验。
            // `change.sessionId` is empty for legacy / non-todo
            // events — accept those (matches the server's
            // "None = legacy fan-out, always accept" policy).
            if (
              change.sessionId &&
              change.sessionId !== activeSessionIdRef.current
            ) {
              return;
            }
            setTodoGroups((prev) => {
              const next = prev.slice();
              const bump = (i: number, mut: (g: TodoGroup) => TodoGroup) => {
                const updated = { ...mut(next[i]) };
                next[i] = updated;
                todoGroupsByIdRef.current.set(updated.groupId, updated);
              };
              // server-sent `TodoSnapshot` 用权威 items 重建 groups，保留每组首次出现顺序。
              // 无 `group_id` 的旧事件归入 `_legacy` 桶仍能渲染。
              if (change.kind === "snapshot") {
                todoGroupsByIdRef.current.clear();
                next.length = 0;
                const order: string[] = [];
                const bucket = new Map<string, TodoSnapshotItem[]>();
                for (const it of change.items) {
                  const gid = it.group_id || "_legacy";
                  if (!bucket.has(gid)) {
                    bucket.set(gid, []);
                    order.push(gid);
                  }
                  bucket.get(gid)!.push(it);
                }
                for (const gid of order) {
                  const items = bucket.get(gid)!;
                  const itemsById: Record<string, TodoSnapshotItem> = {};
                  let title: string | null = null;
                  for (const it of items) {
                    itemsById[it.id] = it;
                    if (!title && it.group_title) title = it.group_title;
                  }
                  const g: TodoGroup = {
                    groupId: gid,
                    groupTitle: title,
                    anchorMessageId: null,
                    itemsById,
                  };
                  todoGroupsByIdRef.current.set(gid, g);
                  next.push(g);
                }
                return next;
              }
              if (change.kind === "group_started") {
                // Ignore duplicates for the same group_id.
                if (todoGroupsByIdRef.current.has(change.groupId)) {
                  const i = next.findIndex((g) => g.groupId === change.groupId);
                  if (i >= 0 && (next[i].groupTitle !== change.groupTitle || next[i].anchorMessageId !== change.messageId)) {
                    bump(i, (g) => ({ ...g, groupTitle: change.groupTitle, anchorMessageId: change.messageId }));
                  }
                  return next;
                }
                const g: TodoGroup = {
                  groupId: change.groupId,
                  groupTitle: change.groupTitle,
                  anchorMessageId: change.messageId,
                  itemsById: {},
                };
                todoGroupsByIdRef.current.set(change.groupId, g);
                next.push(g);
                return next;
              }
              if (change.kind === "upsert") {
                // 跳过正处于用户 toggle 进行中的 item 的 WS echo，
                // 否则快速双点会被首次点击的陈旧 echo 静默回退。
                if (justPatchedTodoRef.current.has(change.item.id)) {
                  return prev;
                }
                const i = next.findIndex((g) => g.groupId === (change.item.group_id || ""));
                if (i < 0) {
                  // No matching group — synthesize one with empty title
                  // so the item still renders. LLM should normally
                  // emit group_started first, but this is a safety net.
                  const g: TodoGroup = {
                    groupId: change.item.group_id || "_legacy",
                    groupTitle: change.item.group_title ?? null,
                    anchorMessageId: null,
                    itemsById: { [change.item.id]: change.item },
                  };
                  todoGroupsByIdRef.current.set(g.groupId, g);
                  next.push(g);
                  return next;
                }
                bump(i, (g) => ({
                  ...g,
                  itemsById: { ...g.itemsById, [change.item.id]: change.item },
                }));
                return next;
              }
              if (change.kind === "remove") {
                const i = next.findIndex((g) => change.id in g.itemsById);
                if (i < 0) return prev;
                bump(i, (g) => {
                  const items = { ...g.itemsById };
                  delete items[change.id];
                  return { ...g, itemsById: items };
                });
                return next;
              }
              return prev;
            });
          },
          onPrefetch: (info) => {
            setPrefetchIntent(info);
          },
          onDone: (text, stopReason, durationMs) => {
            setPrefetchIntent(null);
            setStreamPhase(null);
            // Multi-turn agent loop fix.
            // `text` here is `fullText` from useStreamChat — the
            // accumulated text from ALL `text` events across ALL LLM
            // turns in this stream. But `appendChunk` has already
            // routed text to the correct per-turn assistant
            // placeholders (via needsNewAssistantRef). Using `text`
            // to overwrite the last assistant's content would:
            // - Duplicate turn 1's text into the last assistant
            // - Or replace correctly-streamed text with a garbled
            // concatenation of all turns' text
            // New approach: trust the in-memory state built by
            // appendChunk. Only use `text` as a FALLBACK when the
            // last assistant's content is empty (e.g. the LLM
            // produced no visible text, or all text arrived in the
            // done event instead of via streaming chunks).
            const assistantIdAtDone = currentAssistantIdRef.current;
            const replaced = (() => {
              if (!assistantIdAtDone) return false;
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                const target = s.messages.find((m) => m.id === assistantIdAtDone);
                if (!target) return s;
                // Only use `text` as fallback if the assistant has
                // no content from streaming (appendChunk never ran
                // or produced empty). This covers the edge case
                // where the LLM's entire response arrived in the
                // done event instead of via text chunks.
                const needsFallbackText =
                  (!target.content || target.content.trim() === "") &&
                  text && text.trim().length > 0;
                if (target.stopReason === stopReason && !needsFallbackText) return s;
                return {
                  ...s,
                  messages: s.messages.map((m) =>
                    m.id === assistantIdAtDone
                      ? {
                          ...m,
                          content: needsFallbackText ? text : m.content,
                          stopReason,
                          // Wall-clock time the LLM took
                          // for this turn. Only set if we got a value
                          // (live stream always does; reloaded history
                          // never does, so we leave it undefined and
                          // the renderer shows "已完成" without a time).
                          ...(durationMs !== undefined ? { durationMs } : {}),
                        }
                      : m
                  ),
                  updated_at: new Date().toISOString(),
                };
              }));
              return true;
            })();
            if (!replaced) {
              // Fallback: stamp the trailing assistant row with
              // `stopReason` (preserves the old behavior when we
              // somehow don't have a target id).
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                const msgs = [...s.messages];
                for (let i = msgs.length - 1; i >= 0; i--) {
                  if (msgs[i].role === "assistant") {
                    msgs[i] = { ...msgs[i], stopReason } as ChatMessage;
                    break;
                  }
                }
                return { ...s, messages: msgs };
              }));
            }
            // 持久化最终 assistant 消息的 unified_steps（前端构建的内联时间线）。
            // reload 时 resolveSteps 会优先用它在真实顺序渲染，工具不再被归到末尾。
            if (assistantIdAtDone) {
              const finalMsg = sessionsRef.current
                .find((s) => s.id === sessionId)
                ?.messages.find((m) => m.id === assistantIdAtDone);
              if (finalMsg?.unified_steps && finalMsg.unified_steps.length > 0) {
                void api.saveUnifiedSteps(sessionId, finalMsg.unified_steps);
              }
            }
            // Removed the 500ms re-fetch that replaced the
            // entire messages array with server-persisted data.
            // Root cause of "工具结果突然消失" bug:
            // During streaming, onToolStart/onToolChunk/onToolComplete
            // build complete tool messages in memory. When onDone fires,
            // the 500ms re-fetch replaced these with server data. If the
            // server hadn't finished persisting tool results yet (race
            // condition), or if the execution_timeline was incomplete,
            // expandTimelineToMessages produced fewer tool messages than
            // what was already in memory — making tool results vanish.
            // The in-memory state from streaming callbacks IS the source
            // of truth for the active session. The server's persisted
            // data will be loaded on the next session switch
            // (getSessionMessages in the activeSessionId effect).

            // Mark any still-running tool groups as
            // "interrupted" in the execution_timeline. When the stream
            // settles (done/error/partial), any tool that received
            // `tool_start` but never `tool_complete` is stranded —
            // the backend either crashed mid-tool or the agent loop
            // exited without closing the tool. Without this synthetic
            // `tool_complete`, the tool card stays "running" with a
            // spinner forever, and on reload the stranded pass in
            // ToolEventCard shows "执行已中断（无完成事件）" — but
            // during the live session the user sees a stuck spinner
            // + "正在思考" with no explanation.
            if (assistantIdAtDone && runningToolRef.current.size > 0) {
              const strandedIds = new Set(runningToolRef.current.keys());
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                return {
                  ...s,
                  messages: s.messages.map((m) => {
                    if (m.id !== assistantIdAtDone) return m;
                    const timeline = m.execution_timeline ?? [];
                    if (timeline.length === 0) return m;
                    // Append a synthetic tool_complete for each
                    // stranded tool_call_id.
                    const now = new Date().toISOString();
                    const patched = [...timeline];
                    for (const id of strandedIds) {
                      // Only add if there's a tool_start but no
                      // tool_complete for this tool_call_id.
                      const hasComplete = timeline.some(
                        (e) => e.type === "tool_complete" && e.tool_call_id === id
                      );
                      if (!hasComplete) {
                        const startEv = timeline.find(
                          (e) => e.type === "tool_start" && e.tool_call_id === id
                        );
                        // Check if the tool produced any stdout output before
                        // the session ended. If it did, treat it as a normal
                        // completion rather than an interruption.
                        const hasStdout = timeline.some(
                          (e) => e.type === "tool_stdout"
                            && e.tool_call_id === id
                            && e.content
                            && String(e.content).trim().length > 0
                        );
                        // Friendly user-facing wording — keep "无完成事件"
                        // out of the UI. The user-facing "执行已中断"
                        // is enough; the underlying cause is already
                        // shown via the message's stop-reason chip
                        // ("interrupted" / "error") at the message
                        // header. This stays a soft error (no
                        // "[ERROR]" prefix) so it reads as "the agent
                        // stopped before the tool reported back", not
                        // a hard failure the user must debug.
                        const statusContent = hasStdout
                          ? ""
                          : "执行已中断 — 工具未汇报完成事件";
                        const extraFields: Record<string, any> = {};
                        if (statusContent) {
                          extraFields.content = statusContent;
                        }
                        patched.push({
                          type: "tool_complete",
                          tool: startEv?.tool,
                          tool_call_id: id,
                          created_at: now,
                          ...extraFields,
                        });
                      }
                    }
                    if (patched.length === timeline.length) return m;
                    return { ...m, execution_timeline: patched };
                  }),
                  updated_at: new Date().toISOString(),
                };
              }));
              runningToolRef.current.clear();
              setRunningToolCount(0);
            }

            // Stream is done — drop the assistant id ref so a stale
            // value can't route chunks into the wrong message if
            // the user fires off a new turn later.
            currentAssistantIdRef.current = null;
            // Reject any unresolved approval requests. This can
            // happen if the agent was interrupted (cancel / ws
            // close) mid-approval — without this the resolver map
            // would leak forever.
            approvalResolversRef.current.forEach((resolver) => resolver("denied"));
            approvalResolversRef.current.clear();
            // Batch resolvers share the same lifecycle — the WS
            // already closed, so the in-flight responses are moot;
            // mark them all `denied` (stale card → `expired` via the
            // missing-resolver branch in `resolveApprovalBatch`).
            approvalBatchResolversRef.current.forEach((resolver) => resolver("denied"));
            approvalBatchResolversRef.current.clear();
            // Clean up retry timers
            approvalRetryTimersRef.current.forEach((timer) => clearInterval(timer));
            approvalRetryTimersRef.current.clear();
            // Clean up frontend timeout timers
            approvalTimeoutTimersRef.current.forEach((timer) => clearTimeout(timer));
            approvalTimeoutTimersRef.current.clear();
            // clear the approval session-ownership map too — every pending
            // approval just got force-denied, so the REST fallback has nothing
            // to look up.
            pendingApprovalSessionsRef.current.clear();
            // Same deal for clarify — the agent stopped mid-question
            // (cancel, ws close) so we drop any pending resolver with
            // an empty answer; the server's tool call will time out
            // and the agent loop will surface the error.
            clarifyResolversRef.current.forEach((resolvers) =>
              resolvers.forEach((resolve) => resolve(""))
            );
            clarifyResolversRef.current.clear();
            // 清空 ACUI 卡片栈 — 仍在途的卡片属于刚结束的轮次，不应泄漏到下一次发送。
            acui.reset();
            // Clear live sub-agent panel after a short delay so the
            // user can see the final "已完成/失败" state briefly.
            // Without this the panel persists forever in the chat
            // — sub-agents are never removed from the state map
            // once they complete, only on session switch.
            setTimeout(() => setSubAgents({}), 3000);
          },
          onConnectionLost: () => {
            // Fires the instant the WS reconnect budget is exhausted
            // and the connector has nothing left to fall back to.
            // The user has been staring at "正在回复…" while the
            // server silently burned through its 180s hard timeout —
            // show a visible signal NOW so they know it's a network
            // issue, not the model still thinking.
            //
            // Both forms matter:
            // - toast is the dominant signal (most visible, survives
            //   the StreamingBubble being replaced)
            // - statusText routes through the existing streaming
            //   indicator so even users who missed the toast see
            //   the failure next to the model name.
            const label = activeSessionId === sessionId
              ? "网络中断，请稍候重试。"
              : "网络中断（后台对话），请稍候重试。";
            toastError(label);
            setStreamingStatusImpl("网络中断 — 已发送消息等待重新连接");
            // Tipping the phase into `reconnecting` keeps the
            // existing StreamingBubble in its non-error branch while
            // the connector runs its 3-step retry cascade; the
            // user-facing status text we just pushed is already
            // diagnostic enough without flipping the entire bubble
            // into the red-error state.
          },
          onConnectionRestored: () => {
            toastInfo("网络已恢复，正在继续接收回复。");
            setStreamingStatusImpl(null);
          },
        },
        finalOverrides,
      );
    } catch (e) {
      // (React 18 batching bug fix): same capture pattern
      // as onError above. By the time catch runs, onError may have
      // already set currentAssistantIdRef.current = null. Capture
      // first, use inside setSessions updater.
      const assistantIdAtCatch = currentAssistantIdRef.current;
      const errText = localizeStreamError(String(e));
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) =>
            m.id === assistantIdAtCatch
              ? { ...m, content: m.content || errText }
              : m
          ),
          updated_at: new Date().toISOString(),
        };
      }));
      currentAssistantIdRef.current = null;
      // Surface the error to the active session so ChatView can
      // render a synthetic error row if the optimistic assistant
      // placeholder was filtered out (e.g. the WS rejected the
      // handshake before any chunk landed).
      if (sessionId === activeSessionId) {
        const eMsg = e instanceof Error ? e.message : String(e);
        setStreamError(sessionId, {
          message: localizeStreamError(eMsg),
          kind: classifyStreamErrorKind(eMsg),
        });
      }
    }
  }, [activeSessionId, sessions, sendStream]);

  // pop a queued item out of turn and dispatch it
  // immediately. Defined here (after `handleSendMessage`) so the
  // useCallback deps array can reference it without tripping TSC's
  // "used before declaration" check.
  // !streaming → call handleSendMessage directly, then drop the
  // row from the queue. If the send throws, re-prepend
  // so the user can retry.
  // streaming  → drop the row, then re-prepend. The drain effect
  // below (keyed on `[streaming, messageQueue]`) will
  // pick it up the next tick `streaming` settles.
  const sendFromQueue = useCallback(
    async (idx: number) => {
      // Snapshot the queue item atomically inside the updater so we
      // don't race against any in-flight queue mutation (e.g. the
      // drain effect also calls setMessageQueue).
      let snapshot: QueuedMessage | undefined;
      setMessageQueue((prev) => {
        const found = prev[idx];
        if (found && found.text.trim()) snapshot = found;
        return prev;
      });
      const item = snapshot;
      if (!item || !item.text.trim()) return;

      // Pop the item first so the UI updates instantly.
      setMessageQueue((prev) => prev.filter((_, i) => i !== idx));
      if (streaming) {
        setMessageQueue((prev) => [item, ...prev]);
        return;
      }
      try {
        await handleSendMessage(
          item.text,
          undefined,
          undefined,
          item.images || item.attachedFilePaths
            ? { images: item.images, attachedFilePaths: item.attachedFilePaths }
            : undefined,
        );
      } catch {
        // Failure → put it back at the head of the queue so the
        // user can retry. We don't share the drain effect's retry
        // tracker because that's keyed on auto-drain failures, not
        // manual sends.
        setMessageQueue((prev) => [item, ...prev]);
      }
    },
    [streaming, handleSendMessage],
  );

  const retryLastMessage = useCallback(async () => {
    // Re-send the last user turn so the user can recover from a
    // stream-level error (e.g. provider 529 overloaded) without
    // retyping. handleSendMessage() clears the prior stream error
    // itself, but we clear it first so the banner disappears instantly.
    const lastUser = [...(activeSession?.messages ?? [])]
      .reverse()
      .find((m) => m.role === "user");
    if (!lastUser || !lastUser.content?.trim()) return;
    clearStreamError(activeSession?.id ?? null);
    try {
      await handleSendMessage(lastUser.content);
    } catch {
      // handleSendMessage surfaces its own failure via onError.
    }
  }, [activeSession, handleSendMessage, clearStreamError]);

  /**
   * 切换模型/Provider 时原子重置 in-flight 状态。
   *
   * 若只调 `setSelectedModel + PATCH sessions.model` 而不重置
   * `streamingSessionId / approvalResolvers / streamError / runningTool`
   * 等脏状态 → 新一轮 send 后端按错模型请求 → 400。
   *
   * 本回调不写 store：调用顺序由 ChatView 保证（先 onSwitchModel 再
   * 写 store）。session.model 的持久化也由 ChatView 内的
   * `setSelectedModelAndPersist` 走 PATCH。
   *
   * 必须放在所有 reset deps（clearStreamError / setStreamPhase /
   * approvalResolversRef / acui 等）声明之后——否则 TDZ 报错。
   */
  const handleSwitchModel = useCallback(
    (newId: string) => {
      // 同步更新本地 sessions state — 用户切模型后, activeSession.model
      // 必须立刻反映新值, 否则下次 ChatView 重挂载读 session.model 回灌
      // selectedModel 时, 用旧值(默认模型)覆盖用户选择 (主人反馈).
      if (activeSessionId) {
        setSessions((prev) =>
          prev.map((s) =>
            s.id === activeSessionId
              ? { ...s, model: newId, updated_at: new Date().toISOString() }
              : s,
          ),
        );
      }
      resetChatStateForModelSwitch({
        cancelStream,
        wsSendRaw,
        // chatStateReset was written for the legacy global streamError;
        // scope the clear to the active session so a model switch
        // mid-error doesn't leave another conversation's banner up.
        clearStreamError: () => clearStreamError(activeSessionId),
        setStreamPhase,
        setRunningToolCount,
        setSubAgents: setSubAgents as unknown as (
          updater: (prev: Record<string, unknown>) => Record<string, unknown>,
        ) => void,
        approvalResolversRef: approvalResolversRef as unknown as RefLike<
          Map<string, (decision: unknown) => void>
        >,
        approvalRetryTimersRef,
        approvalTimeoutTimersRef,
        pendingApprovalSessionsRef,
        clarifyResolversRef,
        runningToolRef,
        currentAssistantIdRef,
        acuiReset: acui.reset,
      });
    },
    [
      cancelStream,
      wsSendRaw,
      clearStreamError,
      activeSessionId,
      setSessions,
      setStreamPhase,
      setRunningToolCount,
      setSubAgents,
      acui.reset,
    ],
  );

  // Auto-send queued messages when streaming completes.
  // Lives here (after `handleSendMessage` is declared) because the
  // effect captures it in its closure and TSC's lexical analysis
  // would otherwise flag "used before declaration" — the React
  // runtime order is fine (useCallback runs before the effect's first
  // execution) but tsc only sees the source-order TDZ.
  // 原先放在 handleSendMessage 上方，且 deps 不含 handleSendMessage，靠
  // [streaming, messageQueue] 触发来重绑闭包。对 messageQueue（按索引读的 React state）
  // 碰巧可行，但是个隐患：若 handleSendMessage 将来闭包捕获了 streaming/messageQueue 之外的
  // 变化依赖（如流中 activeSessionId 翻转），effect 会调用陈旧闭包而丢弃队列消息。
  // 失败时 .catch() 重新入队（原先静默丢弃）。最多 3 次重试，用 ref 计数器防无限循环。
  const queueRetriesRef = useRef<Record<string, number>>({});
  useEffect(() => {
    if (!streaming && messageQueue.length > 0) {
      const next = messageQueue[0];
      setMessageQueue(prev => prev.slice(1));
      const queuedAttachments = next.images || next.attachedFilePaths
        ? { images: next.images, attachedFilePaths: next.attachedFilePaths }
        : undefined;
      // drain 必须传当前 selectedModel 作为 overrides.model，否则后端按
      // config.yaml 默认模型请求。activeSession.model 由
      // setSelectedModelAndPersist PATCH 同步写入——用户在 busy 期间切了
      // 模型的话，drain 用新模型。
      // 守 ChatComposer 同款 guard（line ~1843）：必须形如 "provider:model" 且非
      // "unknown" sentinel 才注入；否则让 backend 回落到其 configured default 而不报 400。
      const drainedModel = activeSession?.model;
      const drainedOverrides: import("./hooks/useStreamChat").ChatOverrides | undefined =
        drainedModel && drainedModel.includes(":") && drainedModel !== "unknown"
          ? { model: drainedModel }
          : undefined;
      handleSendMessage(next.text, undefined, drainedOverrides, queuedAttachments).catch((_cause) => {
        // Limit retries: track per-message so a permanently failing
        // backend doesn't spin forever.
        const key = next.text.slice(0, 60); // first 60 chars as key
        const retries = queueRetriesRef.current;
        if ((retries[key] ?? 0) >= 3) {
          console.warn("[queue] giving up on message after 3 retries", next.text);
          delete retries[key];
          return; // drop — user can retype
        }
        retries[key] = (retries[key] ?? 0) + 1;
        setMessageQueue(prev => [next, ...prev]);
      });
    }
  }, [streaming, messageQueue, handleSendMessage]);

  // /undo: soft-delete the most recent user→assistant turn, then re-fetch
  // the active session so the UI reflects the change immediately. The
  // backend is the source of truth for `messages.active`; we just need
  // to re-pull the now-shorter list.
  const handleUndo = useCallback(async () => {
    if (!activeSessionId) return;
    const res = await api.undoSession(activeSessionId, 1);
    if (!res) return;
    try {
      const { messages, contextUsage: restoredCtx } = await api.getSessionMessages(activeSessionId);
      if (restoredCtx) setContextUsage(restoredCtx);
      setSessions((prev) => prev.map((s) => s.id === activeSessionId ? { ...s, messages } : s));
    } catch {}
  }, [activeSessionId]);

  // /restore: re-activate the most recently deactivated block. Same
  // refresh pattern as handleUndo.
  const handleRestore = useCallback(async () => {
    if (!activeSessionId) return;
    const res = await api.restoreSession(activeSessionId);
    if (!res) return;
    try {
      const { messages, contextUsage: restoredCtx } = await api.getSessionMessages(activeSessionId);
      if (restoredCtx) setContextUsage(restoredCtx);
      setSessions((prev) => prev.map((s) => s.id === activeSessionId ? { ...s, messages } : s));
    } catch {}
  }, [activeSessionId]);

  // 路由上下文：chat 页面 props 复杂，预先渲染为 chatNode 元素传入路由表，
  // 其余页面由 ./app/routes 的 renderRoutes 按 page 分发。

  // Derive the error to render from the per-session Map:
  // - when a session is active, show *its* error (or the welcome
  //   sentinel fallback so createSession failures aren't lost);
  // - when on the welcome screen, show the welcome sentinel.
  // The Map itself is the source of truth — `setStreamError` /
  // `clearStreamError` are scoped per-session, and only this view
  // projection knows which entry should be visible.
  const streamErrorForView = activeSessionId
    ? (streamErrors.get(activeSessionId) ?? streamErrors.get(WELCOME_KEY) ?? null)
    : (streamErrors.get(WELCOME_KEY) ?? null);

  const chatViewNode = (
    <ChatView
      session={activeSession}
      onSendMessage={handleSendMessage}
      onNewChat={handleNewChat}
      pendingPrefill={pendingPrefill}
      onConsumePrefill={() => setPendingPrefill(null)}
      // inline todo cards in the message stream.
      // App owns the groups store; ChatView renders one card per
      // group right after the originating assistant message.
      todoGroups={todoGroups}
      subAgents={subAgents}
      onTodoPatch={async (id, patch) => {
        try {
          await api.patchTodo(id, patch);
        } catch (e) {
          console.error('[App] patchTodo failed', e);
          throw e;
        }
      }}
      justPatchedTodoRef={justPatchedTodoRef}
      onOpenExperts={() => setPage("experts")}
      onOpenSettings={openSettings}
      onOpenComputer={openComputer}
      onUndo={handleUndo}
      onRestore={handleRestore}
      onSwitchModel={handleSwitchModel}
      streamingPhase={streamPhase ?? (isStreamingActiveSession ? "streaming" : null)}
      pendingToolsCount={pendingToolsCount}
      environmentLabel="本地Hermix"
      isSandbox={false}
      branch="main"
      workspaceRoot={workspaceRoot}
      onWorkspaceChange={onWorkspaceChange}
      onApproveApproval={handleApproveApproval}
      onDenyApproval={handleDenyApproval}
      onApproveBatchApproval={handleApproveBatch}
      onDenyBatchApproval={handleDenyBatch}
      onAnswerClarify={handleAnswerClarify}
      acuiState={acui.state}
      acuiBus={acuiBus}
      acuiStats={acui.stats}
      messageQueue={messageQueue}
      onAddToQueue={(msg) => {
        // 同文本不重复入队(防快速按 Enter 多次),
        // 但仍返回新数组引用让 React 触发 re-render — 否则
        // 重复添加相同文本时 Composer 看不到 queue 长度变化。
        setMessageQueue(prev => {
          if (prev.some((m) => m.text === msg.text)) return [...prev];
          return [...prev, msg];
        });
      }}
      onClearQueue={() => setMessageQueue([])}
      onRemoveFromQueue={removeFromQueue}
      onEditQueueItem={editQueueItem}
      onSendFromQueue={sendFromQueue}
      messageLoadError={
        activeSessionId ? messageLoadErrors.get(activeSessionId) ?? null : null
      }
      onRetryLoad={() => {
        if (activeSessionId) {
          void handleSelectSession(activeSessionId);
        }
      }}
      streamError={streamErrorForView}
      onClearStreamError={clearStreamError}
      onRetryLastMessage={retryLastMessage}
      prefetchIntent={prefetchIntent}
      rightOpen={rightOpen}
      onToggleRight={handleToggleRight}
      sidebarCollapsed={!sidebarOpen}
      onExpandSidebar={() => setSidebarOpen(true)}
      onOpenSearch={() => setSearchOpen(true)}
      onEnterWelcome={handleEnterWelcome}
      onPathClick={handlePathClick}
      pendingFileAttachment={pendingFileAttachment}
      onConsumeFileAttachment={() => setPendingFileAttachment(null)}
    />
  );
  const routesCtx: RouteContext = {
    sessions,
    handleSelectSession,
    startCreateAppFlow,
    chatNode: chatViewNode,
  };

  return (
    <AppProviders value={streamChatApi}>
    <div className="app-shell">
      <TitleBar />
      <SessionPickerDialog
        open={searchOpen}
        sessions={sessions}
        onSelect={handleSelectSession}
        onClose={() => setSearchOpen(false)}
      />
      {denialBanner && (() => {
        const sub =
          denialBanner.kind === "user_denied"
            ? "你明确拒绝了该操作，未执行。"
            : denialBanner.kind === "timed_out"
              ? "确认超时（你在限定时间内没有做出决定），未执行。"
              : denialBanner.kind === "channel_closed"
                ? "连接中断（WS 断开 / 服务端重启 / 会话重载），请求丢失。"
                : "未执行。";
        return (
          <div
            className="denial-banner"
            role="status"
            aria-live="polite"
            onClick={dismissDenialBanner}
          >
            <span className="denial-banner__icon">!</span>
            <div className="denial-banner__body">
              <div className="denial-banner__title">
                工具 <code>{denialBanner.tool}</code> 的调用被拒绝
              </div>
              <div className="denial-banner__sub">{sub}</div>
            </div>
            <button
              className="denial-banner__close"
              aria-label="关闭"
              onClick={(e) => { e.stopPropagation(); dismissDenialBanner(); }}
            >×</button>
          </div>
        );
      })()}
      <div
        className="app-body"
        data-left-open={sidebarOpen ? "true" : "false"}
        data-right-open={rightOpen ? "true" : "false"}
        style={{
          "--ab-left-w": `${left.width}px`,
          "--ab-right-w": rightOpen ? `${right.width}px` : "0px",
          "--pane-main-min": `${mainMinWidth}px`,
          "--left-min": `${leftMin}px`,
        } as React.CSSProperties}
      >
        {sidebarOpen && <div className="pane-overlay md:hidden" onClick={() => setSidebarOpen(false)} />}

        {/* Left sidebar — grid column 1 controls width; wrapper handles mobile slide */}
        <div
          className={`${sidebarOpen ? "translate-x-0" : "-translate-x-full"} transition-transform duration-slow absolute md:relative z-20 h-full min-h-0 overflow-hidden`}
        >
          <ErrorBoundary name="侧边栏">
          <Sidebar
            page={page}
            onPageChange={setPage}
            sessions={sessions}
            activeSessionId={activeSessionId}
            streamingSessionId={streamingSessionId}
            onNewChat={handleNewChat}
            onSelectSession={handleSelectSession}
            onDeleteSession={handleDeleteSession}
            onNewChatInWorkspace={(workspacePath) => {
              storage.setItem(STORAGE_KEYS.draftWorkspace, workspacePath);
              void handleNewChat();
            }}
            onOpenSettings={openSettings}
            onCollapseSidebar={() => setSidebarOpen(false)}
            onOpenSearch={() => setSearchOpen(true)}
          />
          </ErrorBoundary>
          <div
            data-tauri-no-drag
            onMouseDown={left.onMouseDown}
            className="pane-resizer pane-resizer-left hidden md:block"
          />
        </div>

        {/* Main — flex: 1 + min-width: 0 lets the right panel claim its
            full width first; if the window can't fit, the right panel
            is auto-closed (see recompute() < 1140px), and the main
            column takes the full remaining width. Main column always
            has `min-w-0` so inner content scrolls instead of getting
            pushed below usable width. Three-column layout never wraps —
            it auto-collapses to two columns at 1140px. */}
        <ErrorBoundary name="主内容区">
        <main className="pane pane-main min-w-0 flex-1 flex flex-col">
          {/* 侧栏折叠后的展开入口。必须放在主区：折叠按钮本身在侧栏内部，
              侧栏一 translate 滑出视口就跟着没了，没有这个按钮用户再也打不开。
              分两种形态（用户拍板「与对话标题同一行、不重叠」）：
              - 对话页：不渲染此行 —— 按钮直接嵌进 ChatView 的 ModelStatusBar
                标题行（props: sidebarCollapsed/onExpandSidebar/onOpenSearch）；
              - 其余页面：主区顶部渲染一条真实动作行，页面内容被推到行下方，
                不与「专家」「自动化」等页头重叠。
              main 因此改为 flex column：路由内容包在弹性容器里占剩余高度，
              动作行出现时页面整体下移，不会把内容顶出可视区。 */}
          {!sidebarOpen && page !== "chat" && (
            <SidebarRevealRow
              onExpand={() => setSidebarOpen(true)}
              onOpenSearch={() => setSearchOpen(true)}
            />
          )}
          {/* 必须是 flex column：chat-surface/AutomationView 靠 flex-1 撑满，
              AgentsView/AnalyticsView 靠 h-full —— 两者都要求本容器高度确定
              且为 flex 上下文，缺 flex 会让对话区塌成内容高（输入框被顶上去）。 */}
          <div className="min-h-0 flex-1 flex flex-col">
            {renderRoutes(page, routesCtx)}
          </div>
        </main>
        </ErrorBoundary>        {/* Right sidebar — grid column 3; 0px when closed (collapsed), full
            width when open. Grid ensures main content (column 2, 1fr) is
            never squeezed — it gets all remaining space after fixed columns. */}
        {rightOpen && (
          <div className="h-full flex border-l border-border-subtle overflow-hidden min-w-0 min-h-0">
            <div
              data-tauri-no-drag
              onMouseDown={right.onMouseDown}
              className="pane-resizer pane-resizer-right hidden md:block"
            />
            <ErrorBoundary name="右侧栏">
            <RightSidebar
              open={rightOpen}
              onClose={() => setRightOpen(false)}
              artifacts={artifacts}
              rawCount={rawArtifacts.length}
              selectedId={selectedArtifactId}
              onSelectArtifact={(id) => setSelectedArtifactId(id)}
              workspaceRoot={workspaceRoot}
              activeTab={rightTab}
              onTabChange={setRightTab}
              onAddFileToChat={handleAddFileToChat}
              onPreviewFile={handlePreviewFileFromExplorer}
            />
            </ErrorBoundary>
          </div>
        )}
      </div>

      {/* ─── Artifact preview modal ───
          Full-screen modal that overlays the entire App. Completely
          independent of the artifact sidebar — closing only via X / ESC /
          backdrop click. */}
      {/* Artifact preview from the "产物" tab */}
      {selectedArtifactId && !computerPreviewArtifact && (() => {
        const artifact = artifacts.find((a) => a.id === selectedArtifactId) ?? null;
        if (!artifact) return null;
        return (
          <ArtifactPreviewModal
            artifact={artifact}
            workspaceRoot={workspaceRoot}
            onClose={() => setSelectedArtifactId(null)}
          />
        );
      })()}
      {/* File preview from the "电脑" tab file explorer */}
      {computerPreviewArtifact && (
        <ArtifactPreviewModal
          artifact={computerPreviewArtifact}
          workspaceRoot={workspaceRoot}
          onClose={() => setComputerPreviewArtifact(null)}
        />
      )}
      <ToastContainer />
      <ImageLightbox />
      {showConfigModal && (
        <SystemConfigPanel initialCategory={configCategory} onClose={() => setShowConfigModal(false)} />
      )}
    </div>
    </AppProviders>
  );
}
