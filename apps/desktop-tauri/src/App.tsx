import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { Sidebar } from "./components/Sidebar";
import { ChatView } from "./components/ChatView";
import { AutomationView } from "./components/AutomationView";
import { SettingsView } from "./components/SettingsView";
import { SearchView } from "./components/SearchView";
import { PluginsView } from "./components/PluginsView";
import { AgentsView } from "./components/AgentsView";
import { GroupChatView } from "./components/GroupChatView";
import { AnalyticsView } from "./components/AnalyticsView";
import { RightSidebar, PreviewPanel } from "./components/RightSidebar";
import { TitleBar } from "./components/TitleBar";
import { useStreamChat } from "./hooks/useStreamChat";
import type { StreamPhase } from "./hooks/useStreamChat";
import { useResizable } from "./hooks/useResizable";
import { useWorkspaceRoot } from "./hooks/useWorkspaceRoot";
import { useAcuiState, type AcuiDecisionBus } from "./acui";
import { extractArtifacts, type Artifact, type ArtifactKind } from "./utils/artifacts";
import { tryArtifactPathExists } from "./utils/artifactReader";
import * as api from "./api";
import { registerBuiltinsToBackend } from "./data/builtinExperts";
import type { NavPage, Session, ChatMessage, ApprovalDecision } from "./types";
import { expandTimelineToMessages, detectToolError } from "./components/messageParser";
import { storage, STORAGE_KEYS, sessionKey } from "./utils/storageCompat";

const LEFT_KEY = STORAGE_KEYS.sidebarWidth;/**
 * Map known English backend error strings to Chinese for a better
 * desktop user experience. Falls through to English for anything
 * unrecognised.
 */
function localizeStreamError(err: string): string {
  // Normalize: strip redundant wrappers ("LLM API error:" repeated, etc.)
  const normalized = err
    .replace(/^LLM\s+API\s+error:\s*/gi, "")
    .replace(/^LLM\s+API\s+error:\s*/gi, "")
    .replace(/^API\s+error:\s*/gi, "")
    .trim();

  // Try to extract a human-readable message from JSON error bodies
  // e.g. {"error":{"message":"unknown model..."}} or {"type":"error","error":{...}}
  let extracted = normalized;
  try {
    const parsed = JSON.parse(normalized);
    const msg =
      parsed?.error?.message ??
      parsed?.error?.error?.message ??
      parsed?.message ??
      null;
    if (msg && typeof msg === "string") {
      extracted = msg;
    }
  } catch {
    // not JSON — keep normalized as-is
  }

  // No API key configured for provider
  if (
    /no\s+api\s+key/i.test(extracted) ||
    /api[\s_]*key[\s:]*(not\s+found|missing|empty|not\s+configured)/i.test(extracted)
  ) {
    return "LLM API 密钥未配置。请前往「设置 → API Keys」添加 API 密钥后重试。";
  }
  // LLM provider URL not configured
  if (
    /no\s+llm_provider_url/i.test(extracted) ||
    /provider.*url.*not\s+configured/i.test(extracted)
  ) {
    return "LLM 服务地址未配置。请检查 Settings → API Keys 中的服务地址配置。";
  }
  // Connection refused / unreachable
  if (/(refused|unreachable|connection\s+refused|ECONNREFUSED)/i.test(extracted)) {
    return "无法连接到后端服务，请确认 hermix 进程是否正常运行。";
  }
  // Timeout
  if (/(timeout|timed\s*out)/i.test(extracted)) {
    return "请求超时，请检查网络连接或稍后重试。";
  }
  // auth / token / 401 / 403
  if (/(401|403|unauthorized|forbidden|invalid\s+(api\s*)?key|auth\s+failed)/i.test(extracted)) {
    return "API 密钥认证失败，请检查 Settings → API Keys 中的密钥是否正确。";
  }
  // Rate limit
  if (/(rate\s*limit|429|too\s+many\s+requests)/i.test(extracted)) {
    return "请求过于频繁，请稍后重试。";
  }
  // 400 / bad request — model not found (common MiniMax error)
  if (/(400|bad\s+request|unknown\s+model|invalid\s+model|model.*not\s+found)/i.test(extracted)) {
    // Extract the model name if present
    const modelMatch = extracted.match(/model\s+['"]?([^\s'"]+)['"]?/i);
    const modelName = modelMatch ? modelMatch[1] : null;
    return modelName
      ? `模型「${modelName}」不可用，请在输入框左侧切换模型后重试。`
      : "模型不可用或参数错误，请检查模型配置后重试。";
  }
  // Fallback: return the extracted message (already normalized)
  return extracted || err;
}
const RIGHT_KEY = STORAGE_KEYS.rightWidth;
const RIGHT_OPEN_KEY = STORAGE_KEYS.rightOpen;
const ACTIVE_SESSION_KEY = STORAGE_KEYS.activeSession;

const LEFT_MIN = 200;
const LEFT_MAX = 360;
// v7: 左列默认从 232 减到 200，把 32px 让给主列，让默认三列共存时主列更宽。
// vw=1440 时主列 = 1440 - 200 - 640 = 800 (v11: 右列默认 640 替代 480).
// PLAN_UI_ENHANCEMENT §2.1 (2026-06-22): 菜单栏加宽 200→280，让工作区/群聊名称更舒展。
// 已存在会话的 localStorage width 不会自动升级（用户拖动过的尊重保留），仅默认提高。
const LEFT_DEFAULT = 280;
// v11: 右列默认 640, 拖动范围 360-840.
// 主人反馈 480 装不下"节假日危险品重大危险源..."这样的中文长标题分类,
// 标题被截断成"节假日危险品重大危险源是级管控专题培训会...".
// 640 = 480 + 160, 足以显示完整中文文件名 + 扩展 chip, 不挤压主列.
const RIGHT_MIN = 360;
const RIGHT_MAX = 840;
const RIGHT_DEFAULT = 640;

// 对话主列的"舒适最小宽度"。当主列宽度 ≥ MAIN_MIN_WIDTH 时，chat 气泡的
// 内容（消息头像 + 文本 + 内边距）有充裕的呼吸感，气泡不显拥挤。
//
// v11: 主人反馈右列 480 装不下中文长文件名分类, 改 RIGHT_DEFAULT 480→640.
// 为避免 1440 视口三列总宽溢出 (200+720+640=1560>1440), 同步把
// MAIN_MIN_WIDTH 720→580. 主列 580 仍能装 50+ 中文字符, 平衡两端.
//
// 三列共存临界视口 (v11): LEFT_MIN(200) + MAIN_MIN_WIDTH(580) + RIGHT_MIN(360) = 1140.
// 默认视口 1440 时: 左 200 + 主 580 + 右 640 = 1420 (剩 20 给 splitter, OK).
// 默认视口 1680 时: 左 200 + 主 840 + 右 640 = 1680.
//
// v23: "想看右栏就自动给空间" — 视口 < 1140 + 用户点 TitleBar 产物按钮想开右栏
// 时, 把 Tauri 窗口 outer 宽扩到 DEFAULT_WINDOW_WIDTH (1280 = tauri.conf.json
// 默认), 高度保留当前 outer. 装下三列后 setRightOpen(true). 跟 v22 的
// "装不下就少一列" 不冲突: resize 拖小窗口仍然自动关, 用户**主动**想看才扩.
// appWindow setSize 失败时降级 (仍 setRightOpen(true), 让 layout 走 v22 退化路径).
// 默认视口 1920 时: 左 200 + 主 1080 + 右 640 = 1920.
const MAIN_MIN_WIDTH = 580;

export default function App() {
  const [page, setPageRaw] = useState<NavPage>("chat");
  // 包装 setPage: 切到非 chat 页时主动关闭产物栏 (右栏是 chat 专属 UI)
  // 2026-06-18 主人要求: 在右栏展开时切到 agents/experts/settings/plugins/
  // automation/search/groupchat 任何非对话页 → 自动 setRightOpen(false)
  const setPage = useCallback((p: NavPage) => {
    setPageRaw(p);
    if (p !== "chat") setRightOpen(false);
  }, []);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [messageQueue, setMessageQueue] = useState<string[]>([]);
  // Queue mutation handlers — kept memoized so ChatComposer doesn't
  // re-render on every keystroke in the main textarea.
  // 2026-06-28: added per-item remove / edit / send-now callbacks
  // alongside the pre-existing bulk `onClearQueue`, so the queue
  // panel can show its contents and let the user act on each row.
  //
  // NOTE: `sendFromQueue` closes over `streaming` and
  // `handleSendMessage`, both defined further down, so it's
  // declared after them (see below).
  const removeFromQueue = useCallback((idx: number) => {
    setMessageQueue((prev) => prev.filter((_, i) => i !== idx));
  }, []);
  const editQueueItem = useCallback((idx: number, newText: string) => {
    setMessageQueue((prev) => prev.map((m, i) => (i === idx ? newText : m)));
  }, []);
  const [selectedRoomId, setSelectedRoomId] = useState<string | null>(null);
  const isCreatingSessionRef = useRef(false);

  // One-shot: copy any `hermes.*` localStorage entries forward to
  // `hermix.*` so users on a pre-rename install keep their settings.
  // Idempotent: gated by `hermix.compat.migrated` flag inside the helper.
  useEffect(() => {
    storage.migrateLegacyKeys([
      STORAGE_KEYS.theme,
      STORAGE_KEYS.sidebarWidth,
      STORAGE_KEYS.rightWidth,
      STORAGE_KEYS.rightOpen,
      STORAGE_KEYS.activeSession,
      STORAGE_KEYS.agents,
      STORAGE_KEYS.recentExperts,
      STORAGE_KEYS.permissionLevel,
      STORAGE_KEYS.modelConfig,
      STORAGE_KEYS.modelCache,
      STORAGE_KEYS.workspaceRoot,
      STORAGE_KEYS.workspaces,
      STORAGE_KEYS.groupChatRooms,
      STORAGE_KEYS.groupChatMessages,
      STORAGE_KEYS.mcpServers,
      STORAGE_KEYS.agentsServerUrl,
      STORAGE_KEYS.agentsApiKey,
    ]);

    // 主人反馈：重启后欢迎界面的「目录/专家」选择状态不应保留。
    // draft 槽位（hermix.desktop.draft.*）用于无 session 时的欢迎界面，
    // 持久化会导致上次的选择跨重启残留。每次启动时清除，让用户
    // 在新会话中从头选择。
    try { storage.removeItem(STORAGE_KEYS.draftExpertId); } catch {}
    try { storage.removeItem(STORAGE_KEYS.draftWorkspace); } catch {}

    // 2026-06-25 主人反馈"进历史对话，目录/模型/默认模型都消失"诊断。
    // 临时挂一个启动诊断：把所有 hermix.* key 打到 console，方便主人在
    // devtools 直接看 localStorage 实际状态 — 区分是"数据没写"还是
    // "数据写了没读"。**仅在 dev build 跑**（import.meta.env.DEV）：
    // 生产构建时 Vite 直接 dead-code-eliminate，不影响产物。
    // 可读 localStorage 失败时静默，避免控制台噪音。
    if (import.meta.env.DEV) {
      try {
        const dump: Record<string, string> = {};
        for (let i = 0; i < window.localStorage.length; i++) {
          const k = window.localStorage.key(i);
          if (k && k.startsWith("hermix.")) {
            const v = window.localStorage.getItem(k) ?? "";
            dump[k] = v.length > 200 ? v.slice(0, 200) + `…(${v.length} chars)` : v;
          }
        }
        console.info("[hermix-boot] localStorage dump:", dump);
        // Also expose a console helper so the user can re-dump at any time
        // after interacting with Settings (without a hard reload).
        (window as unknown as { __hermixDumpLs?: () => void }).__hermixDumpLs = () => {
          const out: Record<string, string> = {};
          for (let i = 0; i < window.localStorage.length; i++) {
            const k = window.localStorage.key(i);
            if (k && k.startsWith("hermix.")) {
              const v = window.localStorage.getItem(k) ?? "";
              out[k] = v.length > 200 ? v.slice(0, 200) + `…(${v.length} chars)` : v;
            }
          }
          console.info("[hermix-ls] dump:", out);
          return out;
        };
      } catch {}
    }
  }, []);

  const [activeSessionId, setActiveSessionIdRaw] = useState<string | null>(() => {
    try { return storage.getItem(ACTIVE_SESSION_KEY); } catch { return null; }
  });
  const setActiveSessionId = useCallback((id: string | null) => {
    setActiveSessionIdRaw(id);
    try { id ? storage.setItem(ACTIVE_SESSION_KEY, id) : storage.removeItem(ACTIVE_SESSION_KEY); } catch {}
  }, []);

  const [sidebarOpen, setSidebarOpen] = useState(true);

  const [rightOpen, setRightOpen] = useState<boolean>(() => {
    try { return storage.getItem(RIGHT_OPEN_KEY) !== "false"; } catch { return false; }
  });
  useEffect(() => {
    try { storage.setItem(RIGHT_OPEN_KEY, String(rightOpen)); } catch {}
  }, [rightOpen]);

  // v23: 三列临界视口 + 默认窗口宽. 见上方注释块.
  const THREE_COL_MIN_VIEWPORT = 1140;
  const DEFAULT_WINDOW_WIDTH = 1280;
  const appWindow = getCurrentWindow();

  // v24: 切换右栏时, 视口 < 1140 + 用户在"开"动作上 → 先 await 扩窗到能装三列,
  // 扩窗完成后再 setRightOpen(true). resize 路径仍然走 v22 (装不下关右栏).
  //
  // v23 死循环修复: 之前是 await setSize + 同步 setRightOpen((v)=>!v), 但 setSize
  // 异步完成期间 window.innerWidth 仍是 < 1140, line 415 的 viewport-aware effect
  // 看到 rightOpen=true + vw<1140 立刻 setRightOpen(false), 用户视觉上"点一下抖
  // 一下". 现在 await setSize 完成后再开, 那时 vw 已经是 ≥1140, effect 不会再掐.
  //
  // setSize 失败处理: 不再降级到 v22 退化路径 (v22 退化 = setRightOpen(true) 后
  // 立刻被 effect 掐 = 还是抖). setSize 失败就保持 rightOpen 不变 + console.warn.
  // 用户能再点一次, 或先手动拉宽窗口.
  const handleToggleRight = useCallback(async () => {
    if (rightOpen) {
      // 关闭动作 — 任何视口下都直接关, 跟 v22 一致.
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
        // setSize 失败 — 保持 rightOpen 不动, 不开右栏. 避免 v23 那种"开了又
        // 立刻被掐"的抖动. 用户可以手动拉宽窗口再点.
        console.warn("[right] auto-expand window failed, right sidebar not opened:", e);
        return;
      }
    }
    setRightOpen(true);
  }, [rightOpen]);

  // 2026-06-25 主人反馈："欢迎页默认是关闭产物栏的"。
  // 当 ChatView 从有消息 / 有 session 切到 empty state（欢迎页）时，
  // 由 ChatView 通过 onEnterWelcome 回调通知此 handler — 此处只做
  // 幂等的关闭动作。ChatView 内部用 lastIsWelcomeRef 保证只在
  // false→true 切换时触发，所以这里即使 setRightOpen(false) 也是
  // 一次性的，不会把用户在欢迎页手动开的右栏反复掐掉。
  const handleEnterWelcome = useCallback(() => {
    setRightOpen(false);
  }, []);

  const [selectedArtifactId, setSelectedArtifactId] = useState<string | null>(null);

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
  const { streaming, streamingSessionId, streamingStatus, compactionStatus, send: sendStream, cancel: cancelStream } = useStreamChat();

  // Per-send phase tracking. Set by the `onPhaseChange` callback in
  // `handleSendMessage` and scoped to the current `sessionId` so the
  // ChatView only shows the phase chip for the active session.
  const [streamPhase, setStreamPhase] = useState<StreamPhase | null>(null);
  const prevPhase = useRef<StreamPhase | null>(null);

  // M5 ACUI: the `acui` hook + `acuiBus` useMemo are declared further
  // down (after handleApproveApproval / handleDenyApproval /
  // handleAnswerClarify) to keep the temporal dead zone happy. We
  // reference them inside `handleSendMessage` via closure.

  // Per-session streaming flag. The hook's `streaming` is just
  // `streamingSessionId !== null` for backward-compat callers, but the
  // ChatView needs the *scoped* answer: is *this* session the one
  // currently streaming? Without this, switching to another session
  // mid-stream would leak the "正在思考…" indicator into the new
  // session's view (the new session isn't actually streaming).
  const isStreamingActiveSession = streamingSessionId === activeSessionId;

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
  const [streamError, setStreamError] = useState<string | null>(null);
  const clearStreamError = useCallback(() => setStreamError(null), []);

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
    Map<string, (decision: ApprovalDecision) => void>
  >(new Map());
  /**
   * Tracks running tool messages by their stable WS `tool_call_id`
   * so that `onToolChunk` / `onToolComplete` can find the right inline
   * card (by message `id`) even when multiple tools run concurrently.
   */
  const runningToolRef = useRef<Map<string, string>>(new Map());
  /**
   * When true, the next text chunk should land on a FRESH assistant
   * placeholder instead of the current one.  Set by `onToolComplete` so
   * that after a tool finishes and the agent loop starts its next LLM
   * turn, the new turn's text renders as a visually separate bubble
   * interleaved WITH (not before) the completed tool cards.
   *
   * Without this split, every LLM turn in an agent loop appends to the
   * same assistant message and all tool cards pile up at the bottom —
   * forcing the user to scroll past a wall of tools to read the final
   * reply text.
   */
  const needsNewAssistantRef = useRef(false);
  /**
   * Pending `clarify` callbacks — keyed by `request_id`. When the
   * server pushes a `ClarifyRequest` event we register a resolver
   * here, the user picks an option (or types a free-form answer) in
   * the inline card, and `handleAnswerClarify` resolves the promise.
   * The stream layer then sends the answer back as a
   * `clarify_response` WS frame.
   */
  const clarifyResolversRef = useRef<
    Map<string, (answer: string) => void>
  >(new Map());
  const { workspaceRoot } = useWorkspaceRoot();

  // Auto-collapse sidebar on small screens
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 768px)");
    const handler = (e: MediaQueryListEvent | MediaQueryList) => setSidebarOpen(!e.matches);
    handler(mq);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  // Viewport-aware layout: 视口 < 1140 → 自动关右栏, 退回两列.
  //
  // 主人反馈 v20: "拖动边框缩小时右栏换行/弹遮罩". 改用最简方案 —
  // 视口装不下三列就关掉右栏, 不挤不变形, 也不弹 modal 遮罩 (跟 workbuddy
  // "三列就三列, 装不下就少一列" 的行为一致). 拉宽后不会自动重开 —
  // 用户需要手动点 TitleBar 上的右栏按钮.
  //
  // 阈值: 1140 = LEFT_MIN(200) + MAIN_MIN_WIDTH(580) + RIGHT_MIN(360).
  // 视口 < 1140 → setRightOpen(false) → grid 退化成两列
  // ([data-right-open="false"] CSS 把第三列 track 归零, 主列吃满).
  //
  // recompute 触发条件: window.resize / rightOpen 切换 / 用户拖动 resizer.
  // 三列共存时再 clamp 各自列到静态边界 (LEFT_MAX / RIGHT_MIN..RIGHT_MAX).
  const leftWidthRef = useRef(left.width);
  const rightWidthRef = useRef(right.width);
  useEffect(() => { leftWidthRef.current = left.width; }, [left.width]);
  useEffect(() => { rightWidthRef.current = right.width; }, [right.width]);

  useEffect(() => {
    const recompute = () => {
      const vw = window.innerWidth;
      // 视口 < 1140 → 自动关右栏, 退回两列布局.
      // 三列物理下限 200 + 580 + 360 = 1140, 装不下就关, 不挤不变形.
      if (vw < 1140 && rightOpen) {
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
    return () => window.removeEventListener("resize", recompute);
  }, [rightOpen, left.setWidth, right.setWidth]);

  // Load sessions
  useEffect(() => { api.getSessions().then(setSessions).catch(() => {}); }, []);

  // Register built-in experts to backend ExpertRegistry on app start.
  // Best-effort: the UI fetches the catalog on first mount and falls back
  // to BUILTIN_EXPERTS if the backend is down, so a failure here is non-fatal.
  useEffect(() => { registerBuiltinsToBackend().catch(() => {}); }, []);

  // Load messages for active session.
  //
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
    const session = sessions.find((s) => s.id === activeSessionId);
    if (!session) { if (sessions.length > 0) setActiveSessionId(null); return; }
    if (session.messages.length > 0) return;

    let cancelled = false;
    api.getSessionMessages(activeSessionId)
      .then((rawMessages) => {
        if (cancelled) return;
        clearLoadError(activeSessionId);
        const messages = expandTimelineToMessages(rawMessages);
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
  }, [activeSessionId, sessions, setActiveSessionId, setLoadError, clearLoadError]);

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;

  /**
   * Number of tool messages whose `status === "running"` in the active
   * session. We surface this to `ChatView` so the post-stream
   * activity indicator can show "执行工具中…" when the LLM stream
   * settles but tool calls are still in flight (the user otherwise
   * sees a blank gap between "已完成" and the next turn's first
   * chunk, which reads as "Hermix is stuck"). Memoized off the
   * session reference so we don't re-walk messages on every render.
   */
  const pendingToolsCount = useMemo(() => {
    if (!activeSession) return 0;
    let n = 0;
    for (const m of activeSession.messages) {
      if (m.role === "tool" && m.tool?.status === "running") n++;
    }
    return n;
  }, [activeSession]);

  /**
   * Auto-generate a session title from the first user message, unless the
   * existing title is already a meaningful one. Mirrors the previous
   * in-place logic; the only change is that we now expose it as a helper
   * so the persistence call below can reuse the same value.
   */
  const computeAutoTitle = (s: Session, firstUserContent: string): string => {
    const isPlaceholder =
      !s.title || s.title === "新建对话" || s.title === "新对话" || s.title === "New chat";
    return isPlaceholder || s.messages.length === 0
      ? firstUserContent.replace(/\s+/g, " ").trim().slice(0, 28) +
        (firstUserContent.length > 28 ? "…" : "")
      : s.title;
  };

  // Artifacts derived from the active session's messages. The workspaceRoot
  // is passed through so the Tauri read command can anchor relative paths
  // the LLM mentions in body text (e.g. "tmp/xx/方案.md").
  //
  // Two-phase approach:
  //   1. `rawArtifacts` — synchronous extraction from message text (cheap)
  //   2. `artifacts`     — async existence filter via Tauri IPC
  //      This prevents showing files that the LLM mentioned but don't
  //      actually exist on disk (e.g. /.hermix/skills/SKILL.md paths).
  const rawArtifacts = useMemo(() => {
    if (!activeSession) return [];
    return extractArtifacts(activeSession.messages, workspaceRoot);
  }, [activeSession, workspaceRoot]);

  const [verifiedArtifacts, setVerifiedArtifacts] = useState<Artifact[]>([]);
  useEffect(() => {
    const raw = rawArtifacts;
    if (raw.length === 0) { setVerifiedArtifacts([]); return; }
    let cancelled = false;
    // Batch-check all artifact paths in parallel. Each probe walks
    // the retry chain (workspaceRoot → drive root → ancestors) so a
    // path on a different drive than the workspace still gets found.
    Promise.all(
      raw.map(async (a) => {
        const exists = await tryArtifactPathExists(a.path, workspaceRoot);
        return { a, exists };
      }),
    ).then((results) => {
      if (cancelled) return;
      const next = results.filter((r) => r.exists).map((r) => r.a);
      setVerifiedArtifacts(next);
      // Lightweight diagnostic — gated so it's invisible unless
      // explicitly enabled, but immediately useful when a file
      // "looks like an artifact" but the panel is empty.
      if (typeof window !== "undefined" && window.localStorage?.getItem("hermixDebugArtifacts") === "1") {
        const dropped = results.filter((r) => !r.exists).map((r) => r.a.path);
        // eslint-disable-next-line no-console
        console.debug(
          "[artifacts] raw=%d verified=%d workspaceRoot=%s",
          raw.length, next.length, workspaceRoot ?? "(null)",
        );
        if (dropped.length > 0) {
          // eslint-disable-next-line no-console
          console.debug("[artifacts] dropped paths:", dropped);
        }
      }
    });
    return () => { cancelled = true; };
  }, [rawArtifacts, workspaceRoot]);

  // Alias so downstream consumers don't need to know about the two-phase trick.
  const artifacts = verifiedArtifacts;

  /** Programmatic file open from inside a markdown body. We:
   *   1. find (or invent) an artifact record for this path
   *   2. select it
   *   3. open the right sidebar
   *   4. bump the preview token so RightSidebar swaps into preview mode
   *
   * The "invent" step matters: a markdown body may quote a path
   * that the artifact extractor missed (e.g. a deep cell.value
   * reference that was never written by an LLM tool call). We use
   * the same `classify` helper as the extractor to assign a
   * `kind`, and we push it into `verifiedArtifacts` so the panel
   * stays in sync. */
  const handlePathClick = useCallback(
    (path: string) => {
      const existing = artifacts.find((a) => a.path === path || a.path.endsWith(path));
      if (existing) {
        setSelectedArtifactId(existing.id);
      } else {
        // Build a synthetic artifact so the panel has something to
        // select. The id is just the path itself; we re-derive the
        // kind from the extension here (mirrors `classify` inside
        // `artifacts.ts`, but kept local to avoid exporting an
        // implementation detail from a module that doesn't already
        // re-export it).
        const ext = path.split(".").pop()?.toLowerCase() ?? "";
        const synth: Artifact = {
          id: `path:${path}`,
          name: path.split(/[\\/]/).pop() ?? path,
          path,
          kind: classifyFromExtension(ext),
          tool: "inline",
          timestamp: new Date().toISOString(),
          isAbsolute: /^[A-Za-z]:[\\/]/.test(path) || path.startsWith("/"),
        };
        setVerifiedArtifacts((prev) => [...prev, synth]);
        setSelectedArtifactId(synth.id);
      }
      setRightOpen(true);
    },
    [artifacts],
  );

  /** Mirror of the `classify` logic in `utils/artifacts.ts` — kept
   * local so we don't widen that module's public surface just to
   * classify a synthetic artifact on the path-click path. */
  function classifyFromExtension(ext: string): ArtifactKind {
    const CODE_EXTS = new Set(["ts", "tsx", "js", "jsx", "mjs", "rs", "py", "go", "java", "kt", "c", "cpp", "cxx", "h", "hpp", "rb", "php", "sh", "bash", "css", "html", "vue", "sql", "json", "yaml", "yml", "toml", "xml", "md", "txt", "log", "ini"]);
    const DOC_EXTS = new Set(["docx", "doc", "pdf", "rtf", "odt"]);
    const DATA_EXTS = new Set(["csv", "tsv", "xlsx", "xls", "xlsm", "parquet", "sqlite", "db"]);
    const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp"]);
    const OFFICE_EXTS = new Set(["docx", "doc", "xlsx", "xls", "pptx", "ppt", "odt", "ods", "odp"]);
    if (IMAGE_EXTS.has(ext)) return "image";
    if (OFFICE_EXTS.has(ext)) return "office";
    if (CODE_EXTS.has(ext)) return "code";
    if (DATA_EXTS.has(ext)) return "data";
    if (DOC_EXTS.has(ext)) return "doc";
    return "file";
  }

  const handleNewChat = useCallback(async () => {
    if (isCreatingSessionRef.current) return;
    try { cancelStream(); } catch (_) { /* WS may already be closed — swallow */ }
    try { clearStreamError(); } catch (_) { /* swallow */ }

    // Find an existing session with no messages (empty session).
    //
    // We CANNOT use `s.messages.length === 0` here: the frontend
    // always seeds `messages: []` for every session returned by
    // `GET /v1/sessions` (see api.ts::toSession). The real emptiness
    // signal is `message_count === 0` — the backend-reported count
    // from the same endpoint. Without this distinction, clicking
    // "新建对话" was always reusing an existing *non-empty* session
    // (the first one in the list), which the user perceived as
    // "clicking 新建对话 but landing on an old chat".
    const emptySession = sessions.find(
      (s) => (s.message_count ?? s.messages.length) === 0,
    );

    if (emptySession) {
      // Reuse the existing empty session
      setActiveSessionId(emptySession.id);
      setPage("chat");
      return;
    }

    isCreatingSessionRef.current = true;
    try {
      const session = await api.createSession("新建对话");
      setSessions((prev) => [session, ...prev]);
      setActiveSessionId(session.id);
      setPage("chat");
    } finally {
      isCreatingSessionRef.current = false;
    }
  }, [sessions]);

  const handleSelectSession = useCallback(async (id: string) => {
    console.log("[nav] handleSelectSession id=", id, "page-before=", page);
    try { cancelStream(); } catch (_) { /* WS may already be closed — swallow */ }
    try { clearStreamError(); } catch (_) { /* swallow */ }
    // Always advance to the chat view and select the session,
    // even if cancelStream/clearStreamError threw.
    setActiveSessionId(id);
    setPage("chat");
    if (window.innerWidth < 768) setSidebarOpen(false);
    // The `useEffect` above is the authoritative loader — it watches
    // `activeSessionId` + `sessions` and will trigger a fetch if the
    // session's `messages` is still empty. But on a *re-click* of the
    // same session (e.g. user pressing the "重试" button in the inline
    // error card after a failed load) the effect's guard
    // `session.messages.length > 0` won't fire because messages is
    // still empty — and `activeSessionId` + `sessions` references don't
    // change, so the effect won't re-run. The unconditional fetch below
    // is the explicit-user-click path: it always re-fetches, regardless
    // of whether the effect would have already kicked off a load.
    //
    // Two parallel fetches (one from the effect, one from this handler)
    // for the same `id` are harmless — the responses are identical and
    // `setSessions` is a no-op on duplicate writes.
    try {
      const rawMessages = await api.getSessionMessages(id);
      clearLoadError(id);
      const messages = expandTimelineToMessages(rawMessages);
      setSessions((prev) => prev.map((s) => s.id === id ? { ...s, messages } : s));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error(`[chat] handleSelectSession(${id}) failed:`, msg);
      setLoadError(id, msg);
    }
  }, [setLoadError, clearStreamError, cancelStream, setActiveSessionId, setPage]);

  const handleDeleteSession = useCallback(async (id: string) => {
    // 2026-06-26: the previous implementation called `api.deleteSession`
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
   * 2026-06-14 ("I clicked allow, the card said 已允许, but
   * the LLM said denied"). Instead we surface a distinct
   * `expired` state so the user can see that their click
   * was a no-op and re-issue the prompt if they still want
   * the action to run.
   */
  const resolveApproval = useCallback(
    (
      requestId: string,
      decision: Exclude<ApprovalDecision, null>,
    ): ApprovalDecision | null => {
      const resolver = approvalResolversRef.current.get(requestId);
      if (resolver) {
        resolver(decision);
        approvalResolversRef.current.delete(requestId);
        setSessions((prev) =>
          prev.map((s) => ({
            ...s,
            messages: s.messages.map((m) => {
              if (m.role !== "approval" || m.approval?.requestId !== requestId) return m;
              return {
                ...m,
                approval: {
                  ...(m.approval ?? {
                    requestId,
                    tool: "tool",
                    riskLevel: "caution",
                  }),
                  decision,
                  decidedAt: new Date().toISOString(),
                },
              };
            }),
          })),
        );
        return decision;
      }
      // Stale click — the resolver is already gone. The
      // backend has already auto-denied via the 5-minute
      // timeout. Do NOT flip the card to `approved` /
      // `denied`; mark it `expired` so the user can see
      // their click did not register, and can re-prompt.
      // Skip the downgrade if the card already has a real
      // decision from a previous live click.
      setSessions((prev) =>
        prev.map((s) => ({
          ...s,
          messages: s.messages.map((m) => {
            if (m.role !== "approval" || m.approval?.requestId !== requestId) return m;
            if (m.approval?.decision) return m;
            return {
              ...m,
              approval: {
                ...(m.approval ?? {
                  requestId,
                  tool: "tool",
                  riskLevel: "caution",
                }),
                decision: "expired" as ApprovalDecision,
                decidedAt: new Date().toISOString(),
              },
            };
          }),
        })),
      );
      return null;
    },
    [],
  );

  const handleApproveApproval = useCallback(
    (requestId: string) => {
      resolveApproval(requestId, "approved");
    },
    [resolveApproval],
  );

  const handleDenyApproval = useCallback(
    (requestId: string) => {
      resolveApproval(requestId, "denied");
    },
    [resolveApproval],
  );

  /**
   * User answered a pending clarify card. Resolves the matching
   * `clarify_response` promise so the stream layer can send the
   * answer back over WebSocket, and stamps the card locally so the
   * resolved footer takes over without a server round-trip wait.
   */
  const handleAnswerClarify = useCallback((requestId: string, answer: string) => {
    const resolver = clarifyResolversRef.current.get(requestId);
    if (resolver) {
      resolver(answer);
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

  // ── M5: ACUI directive state + decision bus ─────────────────────
  //
  // The agent emits ACUI (Agent-Controlled UI) directives over the
  // streaming WebSocket (`{ type: "acui", directive: {...} }`). We
  // collect them into `acui.state`, then `ChatView` renders the active
  // cards via `<AcuiPanel state={acui.state} decide={acuiBus} />`.
  //
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

  const handleSendMessage = useCallback(async (content: string, _projectId?: string, overrides?: import("./hooks/useStreamChat").ChatOverrides) => {
    // Clear any previous stream error — the user is starting a new turn,
    // so the old error is stale.  If this turn also fails the error
    // will be re-set by the onError callback.
    setStreamError(null);
    let sessionId = activeSessionId;
    if (sessionId && !sessions.some((s) => s.id === sessionId)) { sessionId = null; setActiveSessionId(null); }
    if (!sessionId) {
      // createSession() calls the HTTP server; if the sidecar isn't
      // running (or hermix.exe failed to spawn — see sidecar.rs
      // path resolution) it throws. Wrap it so the user sees a
      // visible error instead of a click that "did nothing".
      const title = content.slice(0, 40) + (content.length > 40 ? "..." : "");
      let session;
      try {
        session = await api.createSession(title);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setStreamError(
          `后端服务未启动或不可达 (${msg})。` +
          `请检查 hermix.exe 是否在 target/debug/ 下、端口 3000 是否被占用。`,
        );
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

    const userMsg: ChatMessage = { id: `temp-${Date.now()}`, role: "user", content, timestamp: new Date().toISOString() };
    const assistantId = `reply-${Date.now()}`;
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

    // Real-time stream: append each chunk to the *currently active*
    // assistant message. The ref indirection is what lets us switch
    // over to a new message mid-stream when an approval card needs
    // to be inserted, or when a tool completes and the next agent-loop
    // turn should render as an interleaved bubble (see
    // needsNewAssistantRef below).
    const appendChunk = (chunk: string) => {
      // ── Turn-split logic ──────────────────────────────────────
      // If the previous event was a tool_complete, the agent loop is
      // about to start its next LLM turn.  Instead of appending to the
      // now-frozen previous assistant (which already contains that
      // turn's text), we spin up a fresh empty placeholder so the new
      // turn's text renders in its own bubble — interleaved after the
      // completed tool cards.
      if (needsNewAssistantRef.current) {
        needsNewAssistantRef.current = false;
        const nextAssistantId = `reply-${Date.now()}-turn-${Math.random().toString(36).slice(2, 8)}`;
        currentAssistantIdRef.current = nextAssistantId;
        setSessions((prev) => prev.map((s) => {
          if (s.id !== sessionId) return s;
          const newAsst: ChatMessage = {
            id: nextAssistantId,
            role: "assistant",
            content: "",
            timestamp: new Date().toISOString(),
          };
          return { ...s, messages: [...s.messages, newAsst], updated_at: new Date().toISOString() };
        }));
      }
      const target = currentAssistantIdRef.current;
      if (!target) return;
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) =>
            m.id === target ? { ...m, content: m.content + chunk } : m
          ),
          updated_at: new Date().toISOString(),
        };
      }));
    };

    /**
     * Mark a `role: "tool"` message (created in `onToolStart`) as
     * completed. Pulled out so both the "tool_complete arrived before
     * tool_start" and "happy path" branches share the same state
     * mutation — which is what was previously broken: when the
     * `runningToolRef` lookup missed, the card stayed stuck on
     * "执行中" forever.
     */
    const finalizeToolMessage = (
      toolMsgId: string,
      content: string,
    ) => {
      // Detect error output so the badge shows "失败" instead of
      // "完成". Delegates to `detectToolError` (the same helper
      // `messageParser.ts` uses for `expandTimelineToMessages` /
      // `groupTimeline`) so the live-stream and reload paths stay
      // in lockstep.
      //
      // The previous inline regex (`"stderr":` substring) was
      // over-greedy: any JSON payload *with* a `stderr` field —
      // even an empty `"stderr":""` like the one Python's
      // `execute_code` tool emits on success — was mis-flagged as
      // an error. `detectToolError` parses the JSON, checks both
      // the `error` and `stderr` fields for non-empty strings, and
      // honours `exit_code !== 0`.
      const looksLikeError = detectToolError(content) !== null;

      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) => {
            if (m.id !== toolMsgId || m.role !== "tool") return m;
            return {
              ...m,
              content: content ?? m.content,
              tool: m.tool
                ? {
                    ...m.tool,
                    status: (looksLikeError ? "error" as const : "done" as const),
                    output: content ?? m.tool.output,
                    completedAt: new Date().toISOString(),
                  }
                : undefined,
            };
          }),
          updated_at: new Date().toISOString(),
        };
      }));
    };

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
       *  into the approval message and fed to `<InlineApprovalCard>`
       *  so it can render a live "剩余 4m 32s" countdown against
       *  the *original* server-side deadline (not a fresh
       *  re-derivation, which would briefly lie across a reload). */
      expiresAt?: number | null;
      sessionId: string;
    }): Promise<{ decision: ApprovalDecision }> => {
      const previousAssistantId = currentAssistantIdRef.current;
      const nextAssistantId = `reply-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
      currentAssistantIdRef.current = nextAssistantId;

      const approvalMsg: ChatMessage = {
        id: `approval-${req.requestId}`,
        role: "approval",
        content: "",
        timestamp: new Date().toISOString(),
        approval: {
          requestId: req.requestId,
          tool: req.tool,
          args: req.args,
          riskLevel: req.riskLevel,
          decision: null,
          decidedAt: null,
          // Persist the server-emitted deadline so a page reload
          // re-renders the countdown against the *original*
          // server-side timeout (not a fresh `Date.now()+5min`
          // re-derivation, which would briefly lie — the back-end
          // may have already auto-denied while the page was
          // reloading). See the comment on
          // `ApprovalMessageMeta::expiresAt` in `types.ts`.
          expiresAt: req.expiresAt ?? null,
          linkedAssistantId: previousAssistantId,
        },
      };
      const newAssistantMsg: ChatMessage = {
        id: nextAssistantId,
        role: "assistant",
        content: "",
        timestamp: new Date().toISOString(),
      };

      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        // Insert approval + new assistant placeholder *after* the
        // previous (now-frozen) assistant message. The approval card
        // is the only place in the timeline where the order is
        // literally "assistant, approval, assistant" — a rare but
        // correct shape.
        return {
          ...s,
          messages: [...s.messages, approvalMsg, newAssistantMsg],
          updated_at: new Date().toISOString(),
        };
      }));

    return new Promise<{ decision: ApprovalDecision }>((resolve) => {
      approvalResolversRef.current.set(req.requestId, (decision) => resolve({ decision }));
    });
    };

    const onClarifyRequest = (req: {
      requestId: string;
      question: string;
      choices: string[];
      sessionId: string;
    }): Promise<{ answer: string }> => {
      const nextAssistantId = `reply-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
      currentAssistantIdRef.current = nextAssistantId;

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
      const newAssistantMsg: ChatMessage = {
        id: nextAssistantId,
        role: "assistant",
        content: "",
        timestamp: new Date().toISOString(),
      };

      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: [...s.messages, clarifyMsg, newAssistantMsg],
          updated_at: new Date().toISOString(),
        };
      }));

      return new Promise<{ answer: string }>((resolve) => {
        clarifyResolversRef.current.set(req.requestId, (answer) => resolve({ answer }));
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
          onError: (err) => {
            // The error frame arrives immediately (no API key,
            // config error, etc.) — React may batch the
            // setStreamingSessionId(sessionId)+null into one render
            // so the catch block below may be too late. Handle here.
            const errText = `⚠️ ${localizeStreamError(err)}`;
            setStreamPhase(null);
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === currentAssistantIdRef.current
                    ? { ...m, content: m.content || errText, stopReason: "error" as const }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
            currentAssistantIdRef.current = null;
            if (sessionId === activeSessionId) {
              setStreamError(localizeStreamError(err));
            }
          },
          onToolStart: ({ toolCallId, tool, content }) => {
            const toolMsgId = `tool-${toolCallId}`;
            runningToolRef.current.set(toolCallId, toolMsgId);

            const toolMsg: ChatMessage = {
              id: toolMsgId,
              role: "tool",
              content: "",
              timestamp: new Date().toISOString(),
              tool: {
                tool,
                args: content,
                status: "running",
                startedAt: new Date().toISOString(),
                toolCallId,
                linkedAssistantId: currentAssistantIdRef.current ?? undefined,
              },
            };

            // Insert the tool message without creating a new assistant
            // placeholder. The existing streaming assistant (pointed to
            // by currentAssistantIdRef) continues to accumulate chunks,
            // so tools appear *within* the assistant's response rather
            // than before any reply text. The user sees:
            //   [Hermix streaming "我来帮你写…"]
            //   [已写入 tool card]
            //   [Hermix streaming "这是代码内容…"]
            // instead of:
            //   [已写入 tool card]
            //   [Hermix streaming "这是代码内容…"]
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              // Insert after the current streaming assistant.
              const idx = currentAssistantIdRef.current
                ? s.messages.findIndex((m) => m.id === currentAssistantIdRef.current)
                : -1;
              if (idx >= 0) {
                const msgs = [...s.messages];
                msgs.splice(idx + 1, 0, toolMsg);
                return { ...s, messages: msgs, updated_at: new Date().toISOString() };
              }
              // Fallback: append to end
              return {
                ...s,
                messages: [...s.messages, toolMsg],
                updated_at: new Date().toISOString(),
              };
            }));

            // NOTE: we deliberately do NOT mirror tool_start into
            // the streaming assistant's `execution_timeline` here.
            // The inline `role: "tool"` message we just inserted is
            // the single source of truth during the live stream.
            // Mirroring caused AssistantMessage to render
            // `ToolTimelineBlock` *in addition to* the inline card,
            // i.e. two rows per tool call (the "一件事三行" bug).
            // On reload, the server returns the persisted assistant
            // message with its full `execution_timeline`, and
            // `expandTimelineToMessages` converts that into the same
            // inline cards — so the persisted path doesn't need
            // streaming-side mirroring either.
          },
          onToolChunk: ({ toolCallId, content }) => {
            if (!content) return;
            const targetId = runningToolRef.current.get(toolCallId);
            // Even if we can't find the inline card (e.g. the chunk
            // arrived before tool_start, or the ref was wiped), still
            // record the chunk in the timeline so the tool rail sees
            // the live stdout.
            if (targetId) {
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                return {
                  ...s,
                  messages: s.messages.map((m) => {
                    if (m.id !== targetId || m.role !== "tool") return m;
                    return {
                      ...m,
                      tool: m.tool
                        ? { ...m.tool, output: (m.tool.output ?? "") + content }
                        : undefined,
                    };
                  }),
                  updated_at: new Date().toISOString(),
                };
              }));
            }
            // Stdout is NOT mirrored into the streaming assistant's
            // `execution_timeline`. The inline `role: "tool"` card
            // owns the live render path. Mirroring would re-create
            // the "ToolTimelineBlock + inline card" duplicate that
            // caused the user to see "一件事三行". The server
            // already persists a complete `execution_timeline` per
            // assistant message, which `expandTimelineToMessages`
            // turns into inline cards on reload — so we don't need
            // (and shouldn't add) streaming-side duplicates.
          },
          onToolComplete: ({ toolCallId, content }) => {
            // `tool_complete` sometimes arrives without a matching
            // `tool_start` (server retries, or a tool that was kicked
            // off on a previous turn but the WS just re-delivered the
            // close event). In that case the ref-based lookup misses
            // and the card would stay stuck on "执行中" forever. Fall
            // back to the conventional id and try to find the inline
            // card directly; if it doesn't exist we still want to
            // record the close event in the timeline so the rail
            // doesn't get stranded.
            let targetId = runningToolRef.current.get(toolCallId);
            if (targetId) {
              runningToolRef.current.delete(toolCallId);
            } else {
              targetId = `tool-${toolCallId}`;
            }
            if (targetId) {
              finalizeToolMessage(targetId, content ?? "");
            }

            // Signal that the next LLM turn's text should start a fresh
            // assistant bubble so tool cards and text interleave visually:
            //   [asst_text_1] [tool_card] [asst_text_2] [tool_card] ...
            // instead of:
            //   [asst_ALL_TEXT] [tool_card] [tool_card] ...
            needsNewAssistantRef.current = true;

            // tool_complete is NOT mirrored into the streaming
            // assistant's `execution_timeline` for the same reason
            // as onToolChunk above: it would cause the rail to
            // render the same tool call twice (once as the inline
            // card, once inside ToolTimelineBlock). The server's
            // persisted timeline is the source of truth on reload.
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
          onReasoningChunk: (piece) => {
            // Live `<think>...</think>` text from the LLM. Append to
            // the currently-active assistant message's
            // `reasoningContent` so the folding block in
            // MessageBubble can render it (collapsed by default).
            const target = currentAssistantIdRef.current;
            if (!target) return;
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === target
                    ? { ...m, reasoningContent: (m.reasoningContent ?? "") + piece }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onApprovalRequest,
          onClarifyRequest,
          onAcuiDirective: (directive) => acui.apply(directive),
          onPrefetch: (info) => {
            setPrefetchIntent(info);
          },
          onDone: (text, stopReason) => {
            setPrefetchIntent(null);
            setStreamPhase(null);
            // CRITICAL: `run_stream` may push multiple assistant
            // messages into the session across one turn (empty
            // placeholder from a reasoning-only attempt + the real
            // reply), and the in-flight `appendChunk` path accumulates
            // raw content from stream chunks — including any
            // `<think>...</think>` blocks the server didn't strip.
            //
            // The most reliable source of truth is what the server
            // actually persisted to the session, so on `done` we:
            //   1. Replace the optimistic assistant placeholder's
            //      `content` with the server's terminal `text`
            //      (which `parseContent` will clean up at render time).
            //   2. Re-fetch the session messages and replace the
            //      whole `messages` array. This collapses the
            //      duplicate empty + real assistant rows that
            //      `run_stream` produced into the server's view of
            //      the conversation, which is the one we'll be
            //      looking at on next reload anyway.
            //
            // If the re-fetch fails (transient network), we fall
            // back to the in-memory update so the user still sees
            // the terminal `text` rather than an empty "已完成"
            // bubble.
            const assistantIdAtDone = currentAssistantIdRef.current;
            const replaced = (() => {
              if (!text || !assistantIdAtDone) return false;
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                // Find the target message in-memory
                const target = s.messages.find((m) => m.id === assistantIdAtDone);
                // Only update if content actually changed
                // (avoids re-render flash when onDone text === streamed text)
                if (target && target.content === text && target.stopReason === stopReason) return s;
                return {
                  ...s,
                  messages: s.messages.map((m) =>
                    m.id === assistantIdAtDone
                      ? { ...m, content: text, stopReason }
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
              // somehow don't have a target id — e.g. the optimistic
              // insert was overridden by a re-fetch).
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
            // INTENTIONALLY OMITTED: the previous re-fetch via
            // api.getSessionMessages(sessionId) caused a visible
            // flash on completion because:
            //   1. persisted is a brand-new array of brand-new
            //      message objects (the server re-serializes), so
            //      React sees new references and remounts
            //      MessageBubble / ReactMarkdown even when the
            //      content is byte-for-byte identical.
            //   2. If the server append_turn had not finished
            //      persisting yet, persisted could be shorter than
            //      in-memory; the previous length guard masked that
            //      symptom but not the underlying re-mount cost.
            // The in-memory state already has the full terminal
            // content (via appendChunk + the target.content === text
            // guard above) and the full reasoning monologue
            // (via onReasoningChunk). It is the single source of
            // truth for the active session; the server is the source
            // of truth on next session reload, handled by the
            // useEffect watching activeSessionId.
            //
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
            // Same deal for clarify — the agent stopped mid-question
            // (cancel, ws close) so we drop any pending resolver with
            // an empty answer; the server's tool call will time out
            // and the agent loop will surface the error.
            clarifyResolversRef.current.forEach((resolver) => resolver(""));
            clarifyResolversRef.current.clear();
            // M5: clear the ACUI card stack — any cards still in
            // flight belong to the now-finished turn and shouldn't
            // bleed into the next send.
            acui.reset();
          },
        },
        overrides,
      );
    } catch (e) {
      const errText = `⚠️ ${localizeStreamError(String(e))}`;
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) =>
            m.id === currentAssistantIdRef.current
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
        setStreamError(localizeStreamError(e instanceof Error ? e.message : String(e)));
      }
    }
  }, [activeSessionId, sessions, sendStream]);

  // 2026-06-28: pop a queued item out of turn and dispatch it
  // immediately. Defined here (after `handleSendMessage`) so the
  // useCallback deps array can reference it without tripping TSC's
  // "used before declaration" check.
  //
  //   !streaming → call handleSendMessage directly, then drop the
  //                row from the queue. If the send throws, re-prepend
  //                so the user can retry.
  //   streaming  → drop the row, then re-prepend. The drain effect
  //                below (keyed on `[streaming, messageQueue]`) will
  //                pick it up the next tick `streaming` settles.
  const sendFromQueue = useCallback(
    async (idx: number) => {
      // Snapshot the queue atomically inside the updater so we
      // don't race against any in-flight queue mutation (e.g. the
      // drain effect also calls setMessageQueue). Use `undefined`
      // as the initial value so TS keeps the union alive across
      // the updater's optional assignment — `null` would narrow
      // to `never` after the assignment, breaking `.trim()` below.
      let snapshot: string | undefined;
      setMessageQueue((prev) => {
        const found = prev[idx];
        if (typeof found === "string") snapshot = found;
        return prev;
      });
      const item = snapshot;
      if (!item || !item.trim()) return;

      // Pop the item first so the UI updates instantly.
      setMessageQueue((prev) => prev.filter((_, i) => i !== idx));
      if (streaming) {
        setMessageQueue((prev) => [item, ...prev]);
        return;
      }
      try {
        await handleSendMessage(item);
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

  // Auto-send queued messages when streaming completes.
  //
  // Lives here (after `handleSendMessage` is declared) because the
  // effect captures it in its closure and TSC's lexical analysis
  // would otherwise flag "used before declaration" — the React
  // runtime order is fine (useCallback runs before the effect's first
  // execution) but tsc only sees the source-order TDZ.
  //
  // 2026-06-26 v2 fix: was originally placed above `handleSendMessage`
  // without `handleSendMessage` in the deps array, relying on the
  // `[streaming, messageQueue]` triggers to re-bind the closure. That
  // happens to work for `messageQueue` (a React state read by index)
  // but is a footgun: if `handleSendMessage` ever closes over a
  // dep that changes without `streaming`/`messageQueue` changing
  // (e.g. `activeSessionId` flip mid-stream), the effect would call
  // a stale closure and drop the queued message.
  //
  // 2026-06-26 v3 fix: .catch() re-enqueue on failure (previously
  // silently dropped the message). Max 3 retries via ref-based
  // fallback tracker to prevent infinite drain loop.
  const queueRetriesRef = useRef<Record<string, number>>({});
  useEffect(() => {
    if (!streaming && messageQueue.length > 0) {
      const next = messageQueue[0];
      setMessageQueue(prev => prev.slice(1));
      handleSendMessage(next).catch(() => {
        // Limit retries: track per-message so a permanently failing
        // backend doesn't spin forever.
        const key = next.slice(0, 60); // first 60 chars as key
        const retries = queueRetriesRef.current;
        if ((retries[key] ?? 0) >= 3) {
          console.warn("[queue] giving up on message after 3 retries", next);
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
      const rawMessages = await api.getSessionMessages(activeSessionId);
      const messages = expandTimelineToMessages(rawMessages);
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
      const rawMessages = await api.getSessionMessages(activeSessionId);
      const messages = expandTimelineToMessages(rawMessages);
      setSessions((prev) => prev.map((s) => s.id === activeSessionId ? { ...s, messages } : s));
    } catch {}
  }, [activeSessionId]);

  return (
    <div className="app-shell">
      <TitleBar
        onToggleSidebar={() => setSidebarOpen((v) => !v)}
      />
      <div
        className="app-body"
        data-left-open={sidebarOpen ? "true" : "false"}
        data-right-open={rightOpen ? "true" : "false"}
        style={{
          "--ab-left-w": `${left.width}px`,
          "--ab-right-w": rightOpen ? `${right.width}px` : "0px",
          "--pane-main-min": `${MAIN_MIN_WIDTH}px`,
        } as React.CSSProperties}
      >
        {sidebarOpen && <div className="pane-overlay md:hidden" onClick={() => setSidebarOpen(false)} />}

        {/* Left sidebar — grid column 1 controls width; wrapper handles mobile slide */}
        <div
          className={`${sidebarOpen ? "translate-x-0" : "-translate-x-full"} transition-transform duration-200 absolute md:relative z-20 h-full`}
        >
          <Sidebar
            page={page}
            onPageChange={setPage}
            sessions={sessions}
            activeSessionId={activeSessionId}
            streamingSessionId={streamingSessionId}
            onNewChat={handleNewChat}
            onSelectSession={handleSelectSession}
            onDeleteSession={handleDeleteSession}
            isCreatingSession={isCreatingSessionRef.current}
            onRoomSelect={setSelectedRoomId}
          />
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
        <main className="pane pane-main min-w-0 flex-1">
          {page === "chat" && (
            <ChatView
              session={activeSession}
              onSendMessage={handleSendMessage}
              onNewChat={handleNewChat}
              onOpenExperts={() => setPage("experts")}
              onUndo={handleUndo}
              onRestore={handleRestore}
              onStop={cancelStream}
              isStreaming={isStreamingActiveSession}
              streamingPhase={streamPhase ?? (isStreamingActiveSession ? "streaming" : null)}
              streamingStatus={streamingStatus}
              compactionStatus={compactionStatus}
              pendingToolsCount={pendingToolsCount}
              environmentLabel="本地Hermix"
              isSandbox={false}
              branch="main"
              workspaceRoot={workspaceRoot}
              onApproveApproval={handleApproveApproval}
              onDenyApproval={handleDenyApproval}
              onAnswerClarify={handleAnswerClarify}
              acuiState={acui.state}
              acuiBus={acuiBus}
              acuiStats={acui.stats}
              messageQueue={messageQueue}
              onAddToQueue={(msg) => setMessageQueue(prev => [...prev, msg])}
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
              streamError={streamError}
              onClearStreamError={clearStreamError}
              prefetchIntent={prefetchIntent}
              rightOpen={rightOpen}
              onToggleRight={handleToggleRight}
              onEnterWelcome={handleEnterWelcome}
              onPathClick={handlePathClick}
            />
          )}
          {page === "search" && <SearchView sessions={sessions} onSelectSession={handleSelectSession} />}
          {page === "plugins" && <PluginsView />}
          {page === "automation" && <AutomationView />}
          {page === "agents" && (
            <AgentsView
              activeSessionId={activeSessionId}
              onAfterHire={async () => {
                if (!activeSessionId) {
                  try {
                    const session = await api.createSession("新建对话");
                    setSessions((prev) => [session, ...prev]);
                    setActiveSessionId(session.id);
                  } catch { /* fall through */ }
                }
                // Same UX as the dedicated "experts" page: hiring
                // a builtin expert should land the user back in the
                // chat composer with the expert already adopted.
                // Without this, setExpert() updates the sidebar
                // chip but the user is still staring at the agent
                // catalog wondering why the click "did nothing".
                setPage("chat");
              }}
            />
          )}
          {page === "groupchat" && <GroupChatView initialRoomId={selectedRoomId} />}
          {page === "experts" && (
            <AgentsView
              initialTab="builtin"
              activeSessionId={activeSessionId}
              onAfterHire={async () => {
                // Same UX as the old ExpertsView.onBack: ensure there's an
                // active session, then jump back to the chat.
                if (!activeSessionId) {
                  try {
                    const session = await api.createSession("新建对话");
                    setSessions((prev) => [session, ...prev]);
                    setActiveSessionId(session.id);
                  } catch {
                    /* fall through */
                  }
                }
                setPage("chat");
              }}
            />
          )}
          {page === "analytics" && <AnalyticsView />}
          {page === "settings" && <SettingsView />}
        </main>

        {/* Right sidebar — grid column 3; 0px when closed (collapsed), full
            width when open. Grid ensures main content (column 2, 1fr) is
            never squeezed — it gets all remaining space after fixed columns. */}
        {rightOpen && (
          <div className="h-full flex border-l border-border-subtle overflow-hidden min-w-0">
            <div
              data-tauri-no-drag
              onMouseDown={right.onMouseDown}
              className="pane-resizer pane-resizer-right hidden md:block"
            />
            <RightSidebar
              open={rightOpen}
              onClose={() => setRightOpen(false)}
              artifacts={artifacts}
              rawCount={rawArtifacts.length}
              selectedId={selectedArtifactId}
              onSelectArtifact={(id) => setSelectedArtifactId(id)}
              workspaceRoot={workspaceRoot}
            />
          </div>
        )}
      </div>

      {/* ─── Artifact preview modal (主人原话 2026-06-22) ───
          "阅读界面直接弹出界面显示吧,遮罩主界面。
           不要对产物栏有任何关联,包括返回按钮什么的,
           直接显示一个窗口。"
          当用户在产物栏点文件 / 在 markdown 里点产物链接,
          这里渲染一个全屏 modal,完全遮罩整个 App,
          与右栏没有任何视觉关联——关闭只看 X / ESC / 遮罩点击。 */}
      {selectedArtifactId && (() => {
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
    </div>
  );
}

/**
 * Full-window modal that displays an artifact's content.
 *
 * Mounted at the App root so it overlays everything — sidebar,
 * right sidebar, titlebar — with a backdrop click + ESC + close
 * button for dismissal. Keeps the preview pane **completely
 * independent** of the artifact sidebar: closing the modal does
 * not touch the sidebar's state, and the sidebar's state does not
 * leak into the modal.
 */
function ArtifactPreviewModal({
  artifact,
  workspaceRoot,
  onClose,
}: {
  artifact: Artifact;
  workspaceRoot?: string | null;
  onClose: () => void;
}) {
  // ESC closes the modal. Effect attaches the listener only while
  // the modal is mounted; cleanup removes it on unmount so we don't
  // leak handlers when the user navigates to other views.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-[60] flex items-stretch justify-center"
      // Backdrop click closes the modal. We deliberately use a
      // direct `onClick` (not a stopPropagation wrapper) — child
      // clicks bubble up through the panel but we check `e.target
      // === e.currentTarget` so panel-internal clicks don't
      // accidentally close it.
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      // Two-layer backdrop: a 65% black scrim for contrast + a soft
      // 2px blur of the underlying app, so the dialog reads as a
      // genuine "floating" surface rather than a hard pop. The blur
      // is small on purpose — the desktop UI behind is the user's
      // own chat, over-blurring it would feel like the app froze.
      style={{
        background: "rgba(0, 0, 0, 0.65)",
        backdropFilter: "blur(2px)",
        WebkitBackdropFilter: "blur(2px)",
      }}
    >
      <div
        // Modal box — premium frame:
        //  • width: 1100px cap (92vw on narrow screens)
        //  • height: 88vh so the desktop titlebar is still visible
        //    for drag/resize affordance
        //  • 16px radius (xl) + 1px border + 2px outline (subtle
        //    light-mode highlight) + heavy shadow
        //  • background uses theme bg-primary (auto light/dark)
        //  • outline gives the panel a hairline highlight that
        //    survives any bg color, so light mode reads "lifted off"
        //    not "floating void"
        className="flex flex-col shadow-2xl
                   w-[min(1100px,92vw)] h-[88vh] mt-[5vh] mb-[5vh]
                   rounded-2xl overflow-hidden
                   border border-[color:var(--color-border-subtle)]
                   outline outline-2 outline-[color:color-mix(in_srgb,var(--color-text-primary)_6%,transparent)]
                   outline-offset-[-2px]"
        style={{
          background: "var(--color-bg-primary)",
          color: "var(--color-text-primary)",
          boxShadow:
            "0 24px 64px -12px rgba(0, 0, 0, 0.45), 0 8px 24px -8px rgba(0, 0, 0, 0.25)",
        }}
      >
        <PreviewPanel
          artifact={artifact}
          workspaceRoot={workspaceRoot}
          onBack={onClose}
        />
      </div>
    </div>
  );
}
