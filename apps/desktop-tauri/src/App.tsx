import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { flushSync } from "react-dom";
import { getCurrentWindow, LogicalSize, currentMonitor } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { Sidebar } from "./components/Sidebar";
import { currentZoom, computeMainMinWidth, computeLeftMin } from "./app/viewport";
import { SidebarRevealRow } from "./components/SidebarRevealRow";
import { ChatView } from "./components/ChatView";
import { ImageLightbox } from "./components/ImageLightbox";
import { GlobalContextMenu } from "./components/GlobalContextMenu";
import { PreviewBar, BROWSER_TAB_ID, REVIEW_TAB_ID, SUBAGENT_TAB_ID, CANVAS_TAB_ID, type SideTab } from "./components/PreviewBar";
import { docCanvasStore } from "@hermix/preview";
import { TitleBar } from "./components/TitleBar";
import { SessionPickerDialog } from "./components/SessionPickerDialog";
import { useApprovalBridge } from "./hooks/useApprovalBridge";
import { useStreamChat } from "./hooks/useStreamChat";
import { AppProviders } from "./app/AppProviders";
import {
  renderRoutes,
  type RouteContext,
} from "./app/routes";
import { SettingsDialog, type ConfigCategory } from "./components/SettingsDialog";

import { useAppBootstrap } from "./app/useAppBootstrap";
import {
  readDraftWorkspace,
  localizeStreamError,
  classifyStreamErrorKind,
  normalizeStreamErrorKind,
  type StreamErrorKind,
  appendToolStep,
  applyReasoningChunk,
  closeOpenReasoning,
} from "./app/streamEvents";
import type { StreamPhase, TodoSnapshotItem } from "./hooks/useStreamChat";
import type { SubAgentState, SubAgentEventPayload, MemberEventPayload, MemberTicketState } from "./types";
import { usePolling } from "./hooks/usePolling";
import { useResizable } from "./hooks/useResizable";
import { useWorkspaceRoot } from "./hooks/useWorkspaceRoot";
import { useArtifacts } from "./hooks/useArtifacts";
import { resetChatStateForModelSwitch } from "./lib/chatStateReset";
import {
  acknowledgePlan,
  appendApprovalBatchStep,
  appendApprovalRequestStep,
  appendAssistantChunk,
  appendChatMessage,
  appendNewSessions,
  appendTimelineEvent,
  applyToolStdoutChunks,
  beginAssistantTurn,
  computeAutoTitle,
  evictSessionMessages,
  finalizeAssistantStopReason,
  markAssistantError,
  prependOlderMessages,
  resolveClarify,
  stampApprovalDecision,
  stampBatchApprovalDecision,
  stampTrailingAssistantStopReason,
} from "./lib/sessionUpdaters";
import { updateSession } from "./lib/sessionUpdates";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ToastContainer } from "./components/ToastContainer";
import { toastError, toastInfo } from "./hooks/useToast";
import { useAcuiState, type AcuiDecisionBus } from "./acui";
import type { Artifact } from "./utils/artifacts";
import { canonicalPath, classifyArtifactKind } from "./utils/artifacts";
import * as api from "./api";
import { wireArrayToChatMessages, type WireMessage } from "./protocolAdapter";
import { detectToolError } from "./components/messageParser";
import { registerBuiltinsToBackend } from "./data/builtinExperts";
import type {
  NavPage,
  Session,
  ChatMessage,
  ApprovalDecision,
  QueuedMessage,
  QueuedImage,
  QueuedFile,
  ChatMessageImage,
  ApprovalMessageMeta,
  ApprovalBatchMessageMeta,
  TimelineStep,
} from "./types";
import { storage, STORAGE_KEYS, sessionKey } from "./utils/storageCompat";
import { sendNotification, setKeepAwake, KEEP_AWAKE_CHANGED_EVENT, setTrayTaskCount } from "./desktopBridge";
import { applyScale, readScale, UI_SCALE_EVENT } from "./utils/uiScale";
import { CLOSE } from "./components/strings";
import { reportError } from "./lib/reportError";
import { cleanSessionTitle } from "./lib/sessionTitle";

const LEFT_KEY = STORAGE_KEYS.sidebarWidth;

/** 后台 run 活动计数轮询间隔。保活信号丢一次无害，取值偏保守。 */
const KEEP_AWAKE_POLL_MS = 10_000;

/**
 * 窗口注意力心跳间隔。后端 TTL 给到 90 秒（3 倍），丢一两次心跳不会误判离开。
 */
const PRESENCE_HEARTBEAT_MS = 30_000;

const RIGHT_KEY = STORAGE_KEYS.rightWidth;
const RIGHT_OPEN_KEY = STORAGE_KEYS.rightOpen;
const ACTIVE_SESSION_KEY = STORAGE_KEYS.activeSession;

const LEFT_MIN = 200;
const LEFT_MAX = 360;
// 左列默认 280，给主列留出空间，使三列共存时主列更宽、工作区名称更舒展。
// 已存在会话的 localStorage 宽度不自动升级（用户拖动过的尊重保留），仅默认提高。
// 2026-09-30：280 → 224。对齐 cc-haha 的窄侧栏。
// 2026-10-15：224 → 240 → 280。用户对照参考（CodeBuddy 类侧栏 ≈264px）
// 反馈偏窄；240 只 +16px 察觉不到，直接取 280（回到首发值，但语义不同：
// 当时是"收窄实验"的起点，现在是"对齐参考"的终点）。侧栏可拖，上限 360。
const LEFT_DEFAULT = 280;
// 右列默认 640，拖动范围 360-840；vw=1440 时主列 = 1440 - 200 - 640 = 800。
// 640 足以完整显示中文长标题分类（如"节假日危险品重大危险源…"），
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
// 用户主动开右栏（标题栏按钮 / 点文件 / 打开浏览器 / 画布 / 目录 chip）时统一走
// openRightPane：视口装不下三列就**把窗口向右扩**（左边缘不动，缺口 = 左列 +
// 主列舒适最小宽 + 右列 − 当前视口），主列不被挤扁；只有最大化或贴着屏幕右缘
// 时右侧真的没有空间，才退回挤压/自动关闭的旧行为。
// setSize 失败也仍打开：宁可挤压，也不能让用户点了没反应。
// 默认视口 1920 时: 左 200 + 主 1080 + 右 640 = 1920.
// 视口缩放计算已提取到 ./app/viewport（B6-2，含分档注释与测试）。

/**
 * 「不是工具的工具名」。`tool_call` 是 stream_collect 在模型没给出工具名时
 * 的桥接占位名（见 `stream_collect.rs` 的 bridge tool），`tool` / `function`
 * 是解析兜底值 —— 它们进「刚才发生」只会在流水里留下几行同名的废话。
 */
export default function App() {
  const [page, setPageRaw] = useState<NavPage>("chat");
  // 设置落点分类。默认「模型」：配置入口绝大多数是"没配模型/想换模型"，
  // 此前硬编码落在「工具」→ 用户点了"配置模型"却看到插件页。
  const [configCategory, setConfigCategory] = useState<ConfigCategory>("model");
  // 设置是整幅浮层（2026-10-19 用户拍板）：开着时背景页保持原样，
  // 关闭即回到原视图，不再走 NavPage 整页切换。
  const [settingsOpen, setSettingsOpen] = useState(false);

  // 「自定义应用」页「创建新应用」按钮跳转时注入的预填指令（一次性消费）。
  const [pendingPrefill, setPendingPrefill] = useState<string | null>(null);
  // 视口自适应面板最小宽度 — 窗口缩小时缩小主列/左列最小宽
  const [mainMinWidth, setMainMinWidth] = useState(() => computeMainMinWidth(window.innerWidth));
  const [leftMin, setLeftMin] = useState(() => computeLeftMin(window.innerWidth));
  // 包装 setPage: 右栏是 chat 专属 UI，切到任意非对话页时主动关闭产物栏
  // （agents/experts/plugins/automation/search）。
  const setPage = useCallback((p: NavPage) => {
    setPageRaw(p);
    if (p !== "chat") setRightOpen(false);
  }, []);

  /** 打开设置浮层。`category` 让调用方指定落点（如聊天输入框的
   *  "未配置模型"入口应直达「模型」），缺省「模型」。
   *
   *  浮层不切页：背景保持当前对话/工作台，右侧产物栏也不动 —— 调完设置
   *  关闭即回到原视图，这是 2026-10-19 从整页改回弹窗的核心理由。 */
  const openSettings = useCallback((category: ConfigCategory = "model") => {
    setConfigCategory(category);
    setSettingsOpen(true);
  }, []);
const [sessions, setSessions] = useState<Session[]>([]);
const OUTBOX_STORAGE_KEY = "hermix.outbox.v1";
const [messageQueue, setMessageQueue] = useState<QueuedMessage[]>(() => {
  // 待发队列持久化：重启后仍能看到上次没发出去的消息。
  // 带 File 句柄的条目（图片 / 文档）无法序列化，写入时会被过滤掉。
  try {
    const raw = storage.getItem(OUTBOX_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as QueuedMessage[];
    return Array.isArray(parsed)
      ? parsed.filter((m) => m && typeof m.text === "string" && !!m.id)
      : [];
  } catch {
    return [];
  }
});
useEffect(() => {
  const persistable = messageQueue
    .filter((m) => !m.images?.length && !m.attachedFiles?.length)
    .map((m) => (m.status === "sending" ? { ...m, status: "queued" as const } : m));
  try {
    storage.setItem(OUTBOX_STORAGE_KEY, JSON.stringify(persistable));
  } catch {
    /* 配额满：仅内存态，不影响发送 */
  }
}, [messageQueue]);
// 右栏"加入对话"右键菜单触发外部文件附件。
const [pendingFileAttachment, setPendingFileAttachment] = useState<string | null>(null);
const handleAddFileToChat = useCallback((path: string) => {
setPendingFileAttachment(path);
// Ensure we're on the chat page so the user sees the attachment chip.
setPage("chat");
}, []);

  // 队列变更 handlers 保持 memoized，避免 ChatComposer 在主输入框每次按键时重渲染。
  // sendFromQueue 闭包依赖下方定义的 streaming / handleSendMessage，故在其后声明。
  const removeFromQueue = useCallback((id: string) => {
    setMessageQueue((prev) => prev.filter((m) => m.id !== id));
  }, []);
  const editQueueItem = useCallback((id: string, newText: string) => {
    setMessageQueue((prev) => prev.map((m) => (m.id === id ? { ...m, text: newText } : m)));
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
      // 2026-09-30：侧栏搜索行上的快捷键提示原本是假的 —— 没有任何键绑定。
      // 补上 Ctrl/Cmd+K，提示才成立。
      if (e.key === "k" || e.key === "K") {
        // 设置浮层内 Ctrl/Cmd+K 归它自己的设置搜索框；这里抢走会把全局
        // 会话搜索盖在设置层之上。
        if (settingsOpen) return;
        e.preventDefault();
        setSearchOpen(true);
        return;
      }
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
  }, [settingsOpen]);

  const [activeSessionId, setActiveSessionIdRaw] = useState<string | null>(() => {
    try { return storage.getItem(ACTIVE_SESSION_KEY); } catch { return null; }
  });
  const setActiveSessionId = useCallback((id: string | null) => {
    setActiveSessionIdRaw(id);
    try { id ? storage.setItem(ACTIVE_SESSION_KEY, id) : storage.removeItem(ACTIVE_SESSION_KEY); } catch (e) { reportError("app", e); }
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

  /**
   * 进出工作台时接管主对话的落点。
   *
   * 工作台的对话栏复用宿主那份 ChatView，要看哪个工作台的对话就得把
   * `activeSessionId` 指过去 —— 那是**覆盖**不是切换：离开工作台页面
   * 回到主对话时指针还停在工作台的会话上，主对话凭空跳进别人家的历史。
   * 进门前记住、出门时还回去，两边各留各的位置。
   *
   * 放在 effect 里而不是塞进 `setPage`：恢复要读 `activeSessionId` 的当前
   * 值，而它的 ref 声明在后面，写进 setPage 会撞上 TDZ。
   */
  const chatSessionBeforeWorkbenchRef = useRef<string | null>(null);
  const prevPageRef = useRef(page);
  useEffect(() => {
    const prev = prevPageRef.current;
    prevPageRef.current = page;
    if (prev !== "workbench" && page === "workbench") {
      chatSessionBeforeWorkbenchRef.current = activeSessionIdRef.current;
    } else if (prev === "workbench" && page !== "workbench") {
      const back = chatSessionBeforeWorkbenchRef.current;
      chatSessionBeforeWorkbenchRef.current = null;
      // 只在指针还停在工作台的会话上时才收回 —— 用户中途自己点过别的
      // 会话，那个选择更新，不该被覆盖。
      if (back) setActiveSessionId(back);
    }
  }, [page, setActiveSessionId]);

  // 切换会话时立即把上下文用量重置为 null，避免上一个会话的后台 stream 事件或
  // 残留值污染当前会话的 context-usage 进度条。contextUsage / usageSnapshot 这两个
  // state 只在 WS 事件与 restoredCtx 处写入，没有任何切换重置；reset 后进度条在新
  // 会话首句事件到达前不渲染，到达后才显示该会话真实用量。
  useEffect(() => {
    setContextUsage(null);
    setUsageSnapshot(null);
  }, [activeSessionId]);

  const [sidebarOpen, setSidebarOpen] = useState(true);
  // 设置页是专注视图：侧栏让位，把整幅宽度交给设置内容，点「返回」才回主界面。
  // 刻意**不改 sidebarOpen** —— 那是用户的全局布局偏好，若在这里把它置 false，
  // 从设置返回后侧栏就永久收起了，得用户自己去拉回来。这里只做派生。
  const sidebarVisible = sidebarOpen;

  // 全局搜索浮层。从主导航摘掉"搜索"页后由标题栏图标触发。
  const [searchOpen, setSearchOpen] = useState(false);

  const [rightOpen, setRightOpen] = useState<boolean>(() => {
    try { return storage.getItem(RIGHT_OPEN_KEY) !== "false"; } catch { return false; }
  });
  useEffect(() => {
    try { storage.setItem(RIGHT_OPEN_KEY, String(rightOpen)); } catch (e) { reportError("app", e); }
  }, [rightOpen]);

  // 打开右栏的唯一入口在下方（需要 left/right 当前宽度才知缺口），这里先放
  // 一个稳定 ref：点文件 / 开浏览器 / 画布 / 目录 chip 等早期回调都经由它，
  // 不再各写一份"要不要扩窗"的判断。
  const openRightPaneRef = useRef<() => void>(() => {});
  // 扩窗后到布局稳定之间的时间窗：期间 viewport-aware effect 不得拿旧的
  // innerWidth 把刚打开的右栏掐掉。
  const rightOpenGuardUntil = useRef(0);

  // ── 侧栏：一屏多标签，手动关闭 ──────────────────────────────
  // 点对话里的文件路径 / 文件浏览器里的文件都会在这里开一个标签；同一
  // 文件只开一个（按 canonicalPath 去重，不是字符串相等 —— `F:\a\b.md`
  // 与 `F:/a/b.md` 是同一个文件），重复点击只是切过去。
  // 「浏览工作区」是一个单例标签，与文件标签并存 —— 不再是互斥的模式开关。
  const [previewTabs, setPreviewTabs] = useState<SideTab[]>([]);
  const [activePreviewId, setActivePreviewId] = useState<string | null>(null);

  const openPreviewArtifact = useCallback((artifact: Artifact) => {
    const id = `file:${canonicalPath(artifact.path)}`;
    setPreviewTabs((prev) =>
      prev.some((t) => t.id === id) ? prev : [...prev, { id, type: "file", artifact }],
    );
    setActivePreviewId(id);
    openRightPaneRef.current();
  }, []);

  const openBrowserTab = useCallback(() => {
    setPreviewTabs((prev) =>
      prev.some((t) => t.id === BROWSER_TAB_ID)
        ? prev
        : [...prev, { id: BROWSER_TAB_ID, type: "browser" }],
    );
    setActivePreviewId(BROWSER_TAB_ID);
    openRightPaneRef.current();
  }, []);

  // 审查 / 辅助对话同构：单例标签 + 幂等切换。两个都只有一份聚合视图，
  // 不需要像文件那样按路径多开。
  const openSingletonTab = useCallback((id: string, type: "review" | "subagent") => {
    setPreviewTabs((prev) =>
      prev.some((t) => t.id === id) ? prev : [...prev, { id, type } as SideTab],
    );
    setActivePreviewId(id);
    openRightPaneRef.current();
  }, []);

  // 文档画布（单例）：marker 命中时由下方的 store 订阅自动打开。
  // 滑入用 rAF 延后一帧（流畅性约定：画布打开不打断对话气泡的追加动画）。
  const openCanvasTab = useCallback(() => {
    setPreviewTabs((prev) =>
      prev.some((t) => t.id === CANVAS_TAB_ID) ? prev : [...prev, { id: CANVAS_TAB_ID, type: "canvas" }],
    );
    setActivePreviewId(CANVAS_TAB_ID);
    openRightPaneRef.current();
  }, []);

  // 订阅画布元信息：画布开始流式增长时自动开画布 tab。两个触发边沿：
  //  - geometry null→有值（marker 首次命中，本会话第一份画布）
  //  - closed true→false（close() 后新一轮回复又开了画布——第二份、
  //    第三份…也要把面板带回用户眼前）
  // 只挂一次；打开动作本身走 rAF 延帧。
  useEffect(() => {
    const initial = docCanvasStore.getMeta();
    let wasIdle = initial.geometry === null || initial.closed;
    return docCanvasStore.onMeta((m) => {
      const idle = m.geometry === null || m.closed;
      if (!idle && wasIdle) {
        requestAnimationFrame(() => openCanvasTab());
      }
      wasIdle = idle;
    });
  }, [openCanvasTab]);

  const openReviewTab = useCallback(
    () => openSingletonTab(REVIEW_TAB_ID, "review"),
    [openSingletonTab],
  );
  const openSubagentTab = useCallback(
    () => openSingletonTab(SUBAGENT_TAB_ID, "subagent"),
    [openSingletonTab],
  );

  const closePreviewTab = useCallback((id: string) => {
    setPreviewTabs((prev) => {
      const idx = prev.findIndex((t) => t.id === id);
      if (idx === -1) return prev;
      const next = prev.filter((t) => t.id !== id);
      setActivePreviewId((cur) => {
        if (cur !== id) return cur;
        // 落点取邻居：优先右侧（VS Code 行为），右侧没有再退到左侧。
        // 原来这里直接跳 `next[next.length - 1]`（列表最末），关中间一个
        // 标签会让焦点飞到另一头，用户得重新扫一遍标签条才找得到自己在哪。
        return next[idx]?.id ?? next[idx - 1]?.id ?? null;
      });
      return next;
    });
  }, []);

  // 文件浏览器 → 预览：合成一个产物交给预览栏（分类走统一 classify）。
  // 这里的 id 只是满足 Artifact 必填字段 —— 标签身份由 openPreviewArtifact
  // 从 canonicalPath 重新算出，不用它。
  const handlePreviewFileFromExplorer = useCallback(
    (path: string, name: string) => {
      openPreviewArtifact({
        id: `file:${path}`,
        name,
        path,
        kind: classifyArtifactKind(name),
        tool: "file_explorer",
        timestamp: new Date().toISOString(),
        isAbsolute: true,
      });
    },
    [openPreviewArtifact],
  );

  // 默认窗口宽（启动高度适配用）。
  const DEFAULT_WINDOW_WIDTH = 1280;

  // 缩放护栏：最大化 / 还原 / 拖边是整窗重排最重的时刻，期间给根节点挂
  // .is-resizing（CSS 关掉毛玻璃、动画、过渡），停手 120ms 后摘掉。
  useEffect(() => {
    const root = document.documentElement;
    let timer = 0;
    const onResize = () => {
      root.classList.add("is-resizing");
      window.clearTimeout(timer);
      timer = window.setTimeout(() => root.classList.remove("is-resizing"), 120);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      window.clearTimeout(timer);
      root.classList.remove("is-resizing");
    };
  }, []);

  // 启动引导：窗口高度适配 + 旧版 localStorage key 迁移 + dev 诊断 dump。
  // 具体副作用已抽到 ./app/useAppBootstrap，此处仅组合调用。
  useAppBootstrap(DEFAULT_WINDOW_WIDTH);

  // 切换右栏：关闭直接关；打开走统一入口（见下方 openRightPane，
  // 视口不足时先向右扩窗再开）。
  const handleToggleRight = useCallback(() => {
    if (rightOpen) {
      setRightOpen(false);
      return;
    }
    openRightPaneRef.current();
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
  // 旧默认值迁移走 useResizable 的 legacyDefaults（state 初始化时命中即
  // 采用新默认）：不要在 effect 里改 storage —— useResizable 的持久化
  // effect 会把旧 state 原样写回，迁移被原地覆盖（2026-10-15 踩过）。
  const left = useResizable({
    storageKey: LEFT_KEY,
    defaultWidth: LEFT_DEFAULT,
    min: LEFT_MIN,
    max: LEFT_MAX,
    side: "left",
    legacyDefaults: ["280", "224", "240"],
  });
  const right = useResizable({
    storageKey: RIGHT_KEY,
    defaultWidth: RIGHT_DEFAULT,
    min: RIGHT_MIN,
    max: RIGHT_MAX,
    side: "right",
  });

  /**
   * 打开右栏：优先**向右扩窗**，而不是把主列挤扁。
   *
   * 自然三列宽度 = 左列 + 主列舒适最小宽 + 右列。窗口不够时把窗口本身向右
   * 扩（左/上边缘不动），主列宽度保持不变；只有屏幕右侧真的没有空间
   * （最大化、贴着屏幕右缘）才退回挤压布局 —— 即"最大化时才向左挤"。
   */
  const openRightPane = useCallback(async () => {
    if (rightOpen) return;
    const need = Math.ceil(left.width + mainMinWidth + right.width);
    const vw = window.innerWidth / currentZoom();
    if (vw >= need) {
      setRightOpen(true);
      return;
    }
    try {
      const win = getCurrentWindow();
      if (await win.isMaximized()) {
        // 最大化：没有向右的空间，接受挤压。
        setRightOpen(true);
        return;
      }
      const [factor, outer, pos, monitor] = await Promise.all([
        win.scaleFactor(), win.outerSize(), win.outerPosition(), currentMonitor(),
      ]);
      const area = monitor?.workArea;
      if (!area) {
        setRightOpen(true);
        return;
      }
      // 物理像素：屏幕可用区右缘 − 当前窗口右缘 − 12px 边距。
      const room = area.position.x + area.size.width - (pos.x + outer.width) - 12;
      // 缺口按等效视口算，再换算回逻辑像素（缩放 ≠ 1 时不会扩得不够）。
      const grow = Math.min((need - vw) * currentZoom() * factor, room);
      if (grow > 8) {
        // 扩窗是异步的：先立起护栏，避免 viewport effect 用旧 innerWidth
        // 把刚打开的右栏立刻掐掉。
        rightOpenGuardUntil.current = Date.now() + 800;
        // setSize 理论上是即时的，但 Tauri 卡住时不能把"打开右栏"一起拖死。
        await Promise.race([
          win.setSize(new LogicalSize((outer.width + grow) / factor, outer.height / factor)),
          new Promise((resolve) => setTimeout(resolve, 600)),
        ]);
      }
    } catch (e) {
      // 扩窗失败不能阻止用户看内容：退回挤压布局。
      console.warn("[right] auto-expand window failed:", e);
    }
    setRightOpen(true);
  }, [rightOpen, left.width, mainMinWidth, right.width]);

  openRightPaneRef.current = () => { void openRightPane(); };
  const streamChatApi = useStreamChat(activeSessionId);
  // 仅保留 App 自身（标题栏/指示器/审批流）需要的字段；流状态展示已
  // 改由 ChatView 通过 StreamChatContext 直接读，不再逐层透传。
  const { streaming, streamingSessionId, setContextUsage, setUsageSnapshot, setStreamingStatus: setStreamingStatusImpl, send: sendStream, cancel: cancelStream, wsSendRaw } = streamChatApi;

  // 「助手阿黎」（Twin）功能已暂停（2026-11-16）：独立发送器、任务面板、
  // 心跳巡查与自动干预整体移除。`twin:ali` 会话前缀约定保留在后端，
  // 恢复功能时从 git 历史整批找回。





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

    // Synchronous mirror so the getSessionMessages effect (which reads
    // sessionsRef.current) sees the eviction immediately, without
    // waiting for the next render cycle's useEffect to update the ref.
    setSessions((prev) => evictSessionMessages(prev, prevId));
    sessionsRef.current = evictSessionMessages(sessionsRef.current, prevId);
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
      setSessions((prev) => evictSessionMessages(prev, evictId));
      sessionsRef.current = evictSessionMessages(sessionsRef.current, evictId);
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

  // 「Hermix 有任务在跑时保持电脑唤醒」（自动化页开关 × 两路活动信号）。
  //
  // 两路信号都要计入，且**不能互相顶掉**：
  //   - 会话流式：用户正在对话里等结果；
  //   - 后台 run：定时任务到点在跑，此刻没有任何 UI 会话在流式。
  // 早期实现只看 streamingSessionId，于是笔记本合盖/休眠会让到点任务
  // 错过执行窗口——而定时任务恰恰是最需要保活的场景（无人盯着）。
  //
  // 合并而非择一：流式结束但后台任务仍在跑时必须继续保持唤醒，因此
  // 条件是逻辑或，释放条件是两个信号同时为假。
  const [activeRuns, setActiveRuns] = useState(0);

  // 保活巡检（P2-4：共享 usePolling —— 隐藏页不发请求、恢复可见立即补一次）。
  // 轮询而非 WS 推送：后端 AutomationTriggered 事件当前没有前端消费者，
  // 且保活信号丢一次无害（下次 tick 自愈），不值得为它新建一条 WS 消息类型。
  usePolling(async () => {
    setActiveRuns(await api.getActiveAutomationRuns());
  }, KEEP_AWAKE_POLL_MS);

  // 「运行会话时保持电脑唤醒」开关的状态镜像 + 手机是否在跑。
  // 开关本身存在 localStorage（自动化页写入），这里跟着事件同步，
  // 只为了决定「要不要去探测移动端活动」——不探测就别开定时器。
  const [keepAwakeEnabled, setKeepAwakeEnabled] = useState(() => {
    try {
      return storage.getItem(STORAGE_KEYS.keepAwake) === "true";
    } catch {
      return false;
    }
  });
  const [mobileActive, setMobileActive] = useState(false);

  useEffect(() => {
    const sync = () => {
      try {
        setKeepAwakeEnabled(storage.getItem(STORAGE_KEYS.keepAwake) === "true");
      } catch {
        setKeepAwakeEnabled(false);
      }
    };
    window.addEventListener(KEEP_AWAKE_CHANGED_EVENT, sync);
    return () => window.removeEventListener(KEEP_AWAKE_CHANGED_EVENT, sync);
  }, []);

  // 手机端在跑的时候也要防休眠：桌面自己的流式状态看不到移动端，
  // 否则一轮任务做到一半电脑就睡了，手机那头表现为「一直在跑，然后没了」。
  useEffect(() => {
    if (!keepAwakeEnabled) {
      setMobileActive(false);
      return;
    }
    let cancelled = false;
    const tick = () => {
      // 窗口在后台/最小化时跳过：这条轮询只为防休眠，用户不看屏幕时
      // 每 15s 一次 IPC + 后端查一次数据，纯白烧。
      if (document.hidden) return;
      void api.getMobileActivity().then((on) => {
        if (!cancelled) setMobileActive(on);
      });
    };
    tick();
    const timer = window.setInterval(tick, 15_000);
    const onVisible = () => {
      if (!document.hidden) tick();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [keepAwakeEnabled]);

  useEffect(() => {
    setKeepAwake(
      keepAwakeEnabled && (streamingSessionId !== null || activeRuns > 0 || mobileActive),
    );
  }, [keepAwakeEnabled, streamingSessionId, activeRuns, mobileActive]);

  // 上报「人在不在电脑前」。审批的超时按「可送达时间」计费，靠的就是这个信号：
  // 人不在时倒计时暂停，回来还能批，不必让那一轮任务白跑。
  // **必须主动上报**——后端若拿请求流量推断，本组件每 10 秒的防休眠轮询会把
  // 「进程活着」误当成「人在看」，暂停就永远不会生效。
  useEffect(() => {
    let heartbeat: number | undefined;
    const report = (active: boolean) => {
      void api.postPresence(active);
    };
    const start = () => {
      report(true);
      if (heartbeat) window.clearInterval(heartbeat);
      heartbeat = window.setInterval(() => report(true), PRESENCE_HEARTBEAT_MS);
    };
    const stop = () => {
      if (heartbeat) {
        window.clearInterval(heartbeat);
        heartbeat = undefined;
      }
      report(false);
    };
    const sync = () => {
      if (document.hasFocus() && document.visibilityState === "visible") start();
      else stop();
    };
    sync();
    window.addEventListener("focus", sync);
    window.addEventListener("blur", sync);
    document.addEventListener("visibilitychange", sync);
    return () => {
      window.removeEventListener("focus", sync);
      window.removeEventListener("blur", sync);
      document.removeEventListener("visibilitychange", sync);
      if (heartbeat) window.clearInterval(heartbeat);
      // 卸载时明确报「离开」，别让后端停在「有人」。
      report(false);
    };
  }, []);

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
  /**
   * 审批桥接：resolver / 四类定时器 / REST 兜底簿记，已整体搬进
   * `hooks/useApprovalBridge.ts`（App.tsx 拆分第一刀）。各表的语义注释
   * 在 hook 里；这里是组件级单例，unmount 清理语义不变。
   */
  const approvalBridge = useApprovalBridge();
  /**
   * 工具调用被拒绝（用户拒绝 / 超时 / 连接中断）时显示的短时顶部横幅。对应
   * agent 注入的"告诉用户"系统提示给出可见反馈，让用户知道调用为何没执行。
   * 30s 后自动消失；新的拒绝会替换当前横幅。`"policy"` 类拒绝是静默拦截，不进这里。
   */
  const [denialBanner, setDenialBanner] = useState<{ kind: string; tool: string } | null>(null);
  const denialBannerTimerRef = useRef<number | null>(null);
  /**
   * Deferred sub-agent panel clear after a turn finishes. Tracked like
   * `denialBannerTimerRef` so a NEW turn started inside the 3s window
   * isn't wiped by the previous turn's timer, and so the timer doesn't
   * survive unmount.
   */
  const subAgentsClearTimerRef = useRef<number | null>(null);
  const clearSubAgentsAfter = useCallback((sessionId: string) => {
    if (subAgentsClearTimerRef.current !== null) {
      window.clearTimeout(subAgentsClearTimerRef.current);
    }
    subAgentsClearTimerRef.current = window.setTimeout(() => {
      // Ownership check: if the user already moved on to another
      // session, that session's state is not ours to clear.
      if (activeSessionIdRef.current === sessionId) {
        setSubAgents({});
      }
      subAgentsClearTimerRef.current = null;
    }, 3000);
  }, []);
  // Unmount: drop every timer this component owns, so none of them
  // calls setState on a torn-down window.
  useEffect(() => {
    const subTimer = subAgentsClearTimerRef;
    const restTimers = approvalBridge.restTimers;
    return () => {
      if (subTimer.current !== null) {
        window.clearTimeout(subTimer.current);
        subTimer.current = null;
      }
      restTimers.forEach((id) => window.clearTimeout(id));
      restTimers.clear();
    };
  }, [approvalBridge]);
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
  // 成员工单实时状态（总管方案 P4.4）：MemberEvent 流驱动，MemberPanel 渲染。
  const [memberTickets, setMemberTickets] = useState<Record<string, MemberTicketState>>({});
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
      setSessions((prev) => updateSession(prev, sessionId, (s) => ({ ...s, workspace: path ?? undefined })));
    }
  }, [setWorkspaceRoot]);

  // 点目录 chip（已选工作区时）→ 打开右侧预览栏（栏内左上角有文件浏览入口），
  // 不再弹 chooser 列表。
  const openComputer = useCallback(() => {
    openRightPaneRef.current();
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
        // 程序化扩窗尚未生效（setSize 异步）：别拿旧 innerWidth 把刚打开
        // 的右栏掐掉，否则用户看到"点一下闪一下"。
        if (Date.now() < rightOpenGuardUntil.current) return;
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
  // 「加载更早的消息」翻页状态：hasMore 按会话记录（切回已载会话不丢），
  // loading 只作用于当前活跃会话（翻页按钮的防重入）。
  const [historyPaging, setHistoryPaging] = useState<Record<string, { hasMore: boolean }>>({});
  const [historyLoading, setHistoryLoading] = useState(false);

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
    // 等后端 ready 再拉消息：启动竞态里本 effect 早于 Rust init_global，
    // 直接 getSessionMessages 会吃 503 "backend offline" 刷 console 且不重试。
    void (async () => {
      await api.waitForBackendReady();
      if (cancelled) return;
      try {
        const { messages, contextUsage: restoredCtx, hasMore } = await api.getSessionMessages(
          activeSessionId,
          { limit: api.INITIAL_MESSAGE_WINDOW },
        );
        if (cancelled) return;
        clearLoadError(activeSessionId);
        if (restoredCtx) setContextUsage(restoredCtx);
        setHistoryPaging((prev) => ({ ...prev, [activeSessionId]: { hasMore } }));
        setSessions((prev) => updateSession(prev, activeSessionId, (s) => ({ ...s, messages })));
      } catch (e) {
        if (cancelled) return;
        const msg = e instanceof Error ? e.message : String(e);
        console.error(`[chat] getSessionMessages(${activeSessionId}) failed:`, msg);
        setLoadError(activeSessionId, msg);
      }
    })();
    return () => { cancelled = true; };
  }, [activeSessionId, sessions.length, setActiveSessionId, setLoadError, clearLoadError]);

  // 向后端取「已载窗口之前」的一页并前插。tail_skip = 当前在手条数：
  // 后端跳过末尾这些条再向前取，翻页期间流式追加的新消息天然落在
  // 跳过区间里，窗口不会重叠或漏条。后端 id 稳定，按 id 去重兜底
  // 极端情况（翻页途中发生压缩导致组装序列位移）。
  const loadOlderMessages = useCallback(async () => {
    if (!activeSessionId || historyLoading) return;
    const session = sessionsRef.current.find((s) => s.id === activeSessionId);
    if (!session || session.messages.length === 0) return;
    setHistoryLoading(true);
    try {
      const res = await api.getSessionMessages(activeSessionId, {
        limit: api.HISTORY_PAGE_SIZE,
        tailSkip: session.messages.length,
      });
      // 去重在 updater 内做（以目标会话现有 messages 为准，幂等）。
      setSessions((prev) => prependOlderMessages(prev, activeSessionId, res.messages));
      setHistoryPaging((prev) => ({ ...prev, [activeSessionId]: { hasMore: res.hasMore } }));
    } catch (e) {
      console.error(`[chat] loadOlderMessages(${activeSessionId}) failed:`, e);
    } finally {
      setHistoryLoading(false);
    }
  }, [activeSessionId, historyLoading]);

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;

  /**
   * 主对话空间能看见的会话。`sessions` 里还会混着当前工作台的会话
   * （工作台需要它才能渲染对话栏），但它们归工作台所有，出现在主侧栏
   * 就等于把工作台的对话摆到普通对话旁边 —— 点进去得到一段脱离了
   * 工作台上下文的历史，两个空间就此串味。所以列表出口一律过滤，
   * `activeSession` 仍查全量，保证工作台的对话栏渲染得到。
   *
   * 自动化会话同理：调度侧给无锚点的自动化建独立会话
   * （`automation:{id}:{run_id}`，一次 run 一个），它们承载的是定时
   * 任务的执行记录，不是用户和 agent 的对话。混进主列表等于每次触发
   * 都往侧栏塞一条噪声；且 run_id 每次都不同，列表会被这类条目淹没。
   * 运行历史在自动化面板里看，不在这里。
   *
   * 有锚点的自动化跑在来源对话上（`anchor_session_id`），落在正常会话
   * 列表里是应该的——那本来就是用户的对话。
   */
  const chatSessions = useMemo(
    () => sessions.filter((s) => !s.workbench_id && !s.id.startsWith('automation:')),
    [sessions],
  );

  const { handlePathClick } = useArtifacts({
    activeSession,
    workspaceRoot,
    onOpenArtifact: openPreviewArtifact,
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

  // 从「自动化 → 工作流」页「通过对话创建」按钮触发：进入新对话并预填
  // 工作流设计指令。不能只 setPage("chat")——那会复用上一个激活会话，
  // 用户刚从工作台出来就会落进工作台对话空态（isWorkbenchChat），像跳错页。
  const startCreateWorkflowFlow = useCallback(() => {
    setPendingPrefill("帮我设计一个工作流：");
    setActiveSessionId(null);
    setPage("chat");
  }, [setActiveSessionId, setPage]);

  /**
   * 切到某个会话并跳回对话页。侧栏/搜索结果该用这个。
   *
   * 内部分成两步：`selectSessionOnly` 只切 activeSessionId，这里额外补
   * `setPage("chat")`。分成两个是因为工作台页面也要切会话（每个工作台
   * 有自己的侧栏），但它绝不能把宿主从工作台页面踢回对话页 ——
   * 那会让工作台一挂载就自己关掉自己。
   */
  const selectSessionOnly = useCallback((id: string) => {
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
    if (window.innerWidth < 768) setSidebarOpen(false);

    // The activeSessionId effect will fetch messages from the server
    // if the session doesn't have them in memory yet. We no longer
    // duplicate the fetch here — the effect is the single source of
    // truth for loading session messages, which avoids race conditions
    // between two concurrent fetches overwriting each other's results.
    // 若会话已有内存中的消息（如刚结束的流式对话），effect 跳过 fetch
    // 直接复用内存 state。
  }, [clearStreamError, activeSessionId, setActiveSessionId]);

  /**
   * 把会话登记进宿主会话表，已存在的跳过。
   *
   * 工作台的对话被主对话列表按归属过滤掉了，但对话栏复用的仍是宿主
   * 那份 ChatView，它只认宿主会话表里的对象 —— 不登记就渲染不出东西。
   * 只补不覆盖：已登记的会话可能已经加载好消息，覆盖会把它冲成空。
   */
  const upsertSessions = useCallback((incoming: Session[]) => {
    if (incoming.length === 0) return;
    setSessions((prev) => appendNewSessions(prev, incoming));
  }, []);

  const handleSelectSession = useCallback(async (id: string) => {
    selectSessionOnly(id);
    setPage("chat");
  }, [selectSessionOnly, setPage]);

  // ── 托盘联动 ────────────────────────────────────────────────────
  // 流式会话数上报 → 托盘菜单「正在执行的任务」项的文本/可用态。托盘被
  // 设置页关掉时后端 no-op，这里不用关心托盘开关。两个发送器各自独立，
  // 计数要把它们加起来 —— 她那一轮也是"正在执行的任务"。
  useEffect(() => {
    setTrayTaskCount(streamingSessionId ? 1 : 0).catch(() => {});
  }, [streamingSessionId]);

  // 托盘菜单「正在执行的任务」：Rust 已唤起主窗，这里负责跳到流式会话；
  // 没有在跑的就地回对话页。id 走 ref，避免每轮流式起停都重挂监听。
  const streamingIdRef = useRef<string | null>(null);
  useEffect(() => { streamingIdRef.current = streamingSessionId; }, [streamingSessionId]);
  useEffect(() => {
    const un = listen("hermix:tray-open-tasks", () => {
      const id = streamingIdRef.current;
      if (id) void handleSelectSession(id);
      else setPage("chat");
    });
    return () => { void un.then((f) => f()).catch(() => {}); };
  }, [handleSelectSession, setPage]);

  const handleDeleteSession = useCallback(async (id: string) => {
    // 乐观更新：先从 UI 移除再等 IPC 返回，否则点击后侧栏条目"卡住"到
    // 往返结束才消失。失败回滚恢复原状态。
    const snapshot = sessions;
    const deleted = sessions.find((s) => s.id === id) ?? null;
    setSessions((prev) => prev.filter((s) => s.id !== id));
    if (activeSessionId === id) setActiveSessionId(null);
    try {
      await api.deleteSession(id);
    } catch (err) {
      if (deleted) setSessions(snapshot);
      if (activeSessionId === id) setActiveSessionId(id);
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[deleteSession] failed for", id, err);
      window.alert(`删除会话失败：${msg}`);
    }
  }, [activeSessionId, sessions]);

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
      note?: string,
    ): ApprovalDecision | null => {
      // Helper: flip a single assistant message's
      // `kind: "approval_request"` step to the given decision. Used
      // in both the live (resolver present) and stale (resolver gone)
      // branches below.
      const resolver = approvalBridge.resolvers.get(requestId);
      if (resolver) {
        resolver(decision, remember, note);
        approvalBridge.resolvers.delete(requestId);
        // also drop the session-ownership mapping kept for the REST fallback
        approvalBridge.pendingSessions.delete(requestId);
        // Clean up retry timer if approval is resolved
        const retryTimer = approvalBridge.retryTimers.get(requestId);
        if (retryTimer) {
          clearInterval(retryTimer);
          approvalBridge.retryTimers.delete(requestId);
        }
        // Clean up frontend timeout timer
        const timeoutTimer = approvalBridge.timeoutTimers.get(requestId);
        if (timeoutTimer) {
          clearTimeout(timeoutTimer);
          approvalBridge.timeoutTimers.delete(requestId);
        }
        setSessions((prev) => prev.map((s) => stampApprovalDecision(s, requestId, decision, remember)));
        return decision;
      }
      // Stale click — the resolver is already gone. Two very different
      // situations hide here: (a) the backend really did auto-deny
      // (timeout / stream-end mass-deny) — mark `expired` so the user
      // sees their click did not register; (b) the page reloaded while
      // the approval is STILL live in the server registry (persisted
      // row renders again, resolvers died with the old page) — the
      // click is perfectly actionable, and the old behavior (stamp
      // `expired`, skip REST because pendingApprovalSessionsRef died
      // with the page) made the user watch a live approval "expire"
      // while the agent sat blocked on it. So: locate the card's meta
      // in local state; if its server deadline hasn't passed, resolve
      // via REST against the OWNING session (not activeSessionId —
      // the user may have switched tabs) and stamp the real decision.
      type CardMeta = { sessionId: string; expiresAt: number | null; decided: boolean };
      let card: CardMeta | null = null;
      for (const sess of sessionsRef.current) {
        for (const m of sess.messages) {
          if (m.role === "approval" && m.approval?.requestId === requestId) {
            card = { sessionId: sess.id, expiresAt: m.approval.expiresAt ?? null, decided: !!m.approval.decision };
            break;
          }
          for (const st of m.unified_steps ?? []) {
            if ((st.kind === "approval_request" && st.meta.requestId === requestId)
              || (st.kind === "approval_request_batch" && (st.meta as unknown as { batchId?: string }).batchId === requestId)) {
              const batch = st.kind === "approval_request_batch";
              card = {
                sessionId: sess.id,
                // batch cards carry no server deadline — treat as live
                expiresAt: batch ? null : st.meta.expiresAt ?? null,
                decided: !!st.meta.decision,
              };
              break;
            }
          }
          if (card) break;
        }
        if (card) break;
      }
      const live = card
        && !card.decided
        && (!card.expiresAt || Date.now() < card.expiresAt);
      if (!live || !card) {
        setSessions((prev) => prev.map((s) => stampApprovalDecision(s, requestId, "expired")));
        return null;
      }
      const owningSid = card.sessionId;
      const restPayload: Record<string, string> = { request_id: requestId, decision };
      if (remember) restPayload.decision = "always";
      if (note?.trim()) restPayload.note = note.trim();
      void api
        .fetchJSON<{ matched: boolean }>(
          `/v1/sessions/${encodeURIComponent(owningSid)}/approval`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(restPayload),
          },
        )
        .then((data) => {
          // matched=true  → this click actually delivered; the waiting
          //                 agent resumes. Stamp the real decision.
          // matched=false → the entry was resolved between render and
          //                 click (timeout lost the race) — expired.
          setSessions((prev) => prev.map((s) => stampApprovalDecision(s, requestId, data.matched ? decision : "expired", remember)));
        })
        .catch(() => {
          setSessions((prev) => prev.map((s) => stampApprovalDecision(s, requestId, "expired")));
        });
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
      // the stream layer re-sends `approval_response` over the live
      // session channel, which fires when the
      // `resolveApproval()` Promise below settles. Sending the same payload from BOTH paths meant server-side
      // `ApprovalRegistry::resolve()` got called twice — first call
      // returned `matched=true` (the real success), second call returned
      // `matched=false` (idempotent miss) and surfaced as a confusing
      // red `[APPROVAL-REST] matched=false` in the console, which users
      // read as "the approval failed".
      // We now keep only:
      // 1. resolveApproval() → triggers the main channel path
      // (approval_response over the live session channel)
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
        approvalBridge.pendingSessions.get(requestId) ?? activeSessionId;
      if (restSid) {
        const t = window.setTimeout(async () => {
          approvalBridge.restTimers.delete(t);
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
        approvalBridge.restTimers.add(t);
      }

      // Resolve the promise immediately so the UI updates
      // AND triggers the main upstream channel path
      // (useStreamChat.ts connected-frame send).
      resolveApproval(requestId, "approved", remember);
    },
    [activeSessionId, resolveApproval],
  );

  const handleDenyApproval = useCallback(
    (requestId: string, note?: string) => {
      console.info(`[APPROVAL-DENY] id=${requestId} t=${Date.now()}`);

      // mirror the approve-path cleanup — drop the redundant
      // wsSendRaw() step here too. The main upstream channel path in
      // useStreamChat.ts (the `.then(resp => ...)` after
      // `resolveApproval()`) handles sending `approval_response` over
      // the live `stream:<sid>` channel. The REST fallback below is
      // what actually catches silent message loss across the bridge.
      const restPayload: Record<string, string> = {
        request_id: requestId,
        decision: "denied",
      };
      // 「告诉模型接下来该怎么做」——拒绝时附带的说明进 REST 兜底载荷；
      // 主链路（WS 帧）在 useStreamParser 里同样带上 note。
      if (note?.trim()) restPayload.note = note.trim();
      const restSid =
        approvalBridge.pendingSessions.get(requestId) ?? activeSessionId;
      if (restSid) {
        const t = window.setTimeout(async () => {
          approvalBridge.restTimers.delete(t);
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
        approvalBridge.restTimers.add(t);
      }

      // Resolve the promise immediately so the UI updates
      // AND triggers the main channel path (the stream layer sends
      // decision="denied" via the `.then(resp => ...)` chain).
      resolveApproval(requestId, "denied", undefined, note?.trim() || undefined);
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
      note?: string,
    ): ApprovalDecision | null => {
      const resolver = approvalBridge.batchResolvers.get(batchId);
      if (resolver) {
        resolver(decision, remember, note);
        approvalBridge.batchResolvers.delete(batchId);
        approvalBridge.pendingSessions.delete(batchId);
        const retryTimer = approvalBridge.retryTimers.get(batchId);
        if (retryTimer) {
          clearInterval(retryTimer);
          approvalBridge.retryTimers.delete(batchId);
        }
        const timeoutTimer = approvalBridge.timeoutTimers.get(batchId);
        if (timeoutTimer) {
          clearTimeout(timeoutTimer);
          approvalBridge.timeoutTimers.delete(batchId);
        }
        setSessions((prev) =>
          prev.map((s) => stampBatchApprovalDecision(s, batchId, decision, remember)),
        );
        return decision;
      }
      // Stale click — batch already settled (timeout / resolve /
      // disconnect). Mark the card `expired` so the user sees the
      // honest "your click did not register" state.
      setSessions((prev) =>
        prev.map((s) => stampBatchApprovalDecision(s, batchId, "expired")),
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
        approvalBridge.pendingSessions.get(batchId) ?? activeSessionId;
      if (restSid) {
        const t = window.setTimeout(async () => {
          approvalBridge.restTimers.delete(t);
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
        approvalBridge.restTimers.add(t);
      }
      resolveApprovalBatch(batchId, "approved", remember);
    },
    [activeSessionId, resolveApprovalBatch],
  );

  const handleDenyBatch = useCallback(
    (batchId: string, note?: string) => {
      console.info(`[APPROVAL-BATCH-DENY] id=${batchId} t=${Date.now()}`);
      // REST 1s fallback — mirrors handleApproveBatch. Without it a
      // dropped WS deny frame leaves the agent blocked until the
      // server-side approval timeout (600s). The backend resolves
      // batch ids via the same `request_id` field.
      const restSid =
        approvalBridge.pendingSessions.get(batchId) ?? activeSessionId;
      if (restSid) {
        const t = window.setTimeout(async () => {
          approvalBridge.restTimers.delete(t);
          try {
            await api.fetchJSON<{ matched?: boolean }>(
              `/v1/sessions/${encodeURIComponent(restSid)}/approval`,
              {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                  request_id: batchId,
                  decision: "denied",
                  ...(note?.trim() ? { note: note.trim() } : {}),
                }),
              },
            );
          } catch (e) {
            console.error(
              `[APPROVAL-BATCH-REST-FAIL] id=${batchId} error=${String(e)}`,
            );
          }
        }, 1000);
        approvalBridge.restTimers.add(t);
      }
      resolveApprovalBatch(batchId, "denied", undefined, note?.trim() || undefined);
    },
    [activeSessionId, resolveApprovalBatch],
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
    setSessions((prev) => resolveClarify(prev, requestId, answer));
  }, []);

  // 计划确认卡「知道了」：本地置 acknowledged，卡片随即消失。
  // 后端没有计划确认回传通道（无 WsClientMessage 变体、无 agent
  // loop 等待方），所以这里不发任何上行帧。
  const handleAcknowledgePlan = useCallback((messageId: string) => {
    setSessions((prev) => acknowledgePlan(prev, messageId));
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
    attachments?: { images?: QueuedImage[]; attachedFilePaths?: string[]; attachedFiles?: QueuedFile[] },
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
      // 建会话时的种子标题同样要洗：这是脏标题入库的第一现场。
      const cleaned = cleanSessionTitle(content) || content.replace(/\s+/g, " ").trim();
      const title = cleaned.slice(0, 40) + (cleaned.length > 40 ? "..." : "");
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
        return false;
      }
      // 新会话沿用用户此刻选中的模型：先写进本地会话对象（否则 ChatView
      // 的 restored 逻辑会按 session.model 回灌成后端默认模型，表现就是
      // "欢迎页选好的模型一进去又变回去"），再 PATCH 到后端（重载后仍生效）。
      if (overrides?.model) {
        session = { ...session, model: overrides.model };
        api
          .updateSessionMetadata(session.id, { model: overrides.model })
          .catch((e) => console.warn("[send] persist session model failed:", e));
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
      } catch (e) { reportError("app", e); }
      try {
        const draftWorkspace = storage.getItem(STORAGE_KEYS.draftWorkspace);
        if (draftWorkspace) {
          storage.setItem(sessionKey(session.id, "workspace"), draftWorkspace);
        }
      } catch (e) { reportError("app", e); }
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
      // Insert user message + empty assistant placeholder（纯函数见
      // sessionUpdaters.beginAssistantTurn；自动标题逻辑同文件）。
      setSessions((prev) =>
        updateSession(prev, sessionId, (s) =>
          beginAssistantTurn(s, userMsg, assistantId, content, new Date().toISOString()),
        ),
      );
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
    if (attachments?.attachedFiles?.length) {
      finalOverrides.attachedFiles = await Promise.all(
        attachments.attachedFiles.map(async (f) => ({
          name: f.name,
          mimeType: f.mimeType,
          bytes: Array.from(new Uint8Array(await f.file.arrayBuffer())),
        })),
      );
    }

    // Real-time stream: append each chunk to the *currently active*
    // assistant message. removed needsNewAssistantRef
    // turn-split logic — all text in an agent loop now goes to ONE
    // assistant message. Tool calls are tracked in the assistant's
    // `execution_timeline` (rendered as a collapsible block) instead
    // of separate `role: "tool"` messages. This ensures streaming
    // and reload produce identical visuals.
    //
    // chunk 攒批（P1-3 第二步，与下方 pendingToolChunks 同一模式）：文本
    // chunk 先落闭包局部 buffer，requestAnimationFrame 合并后一次
    // setSessions 应用（同帧 N chunks → 1 次 App 级重渲）。appendTextStep
    // 是纯文本拼接，攒批与逐 chunk 结果逐字一致。done/error 强制 flush，
    // 尾批 chunk 不滞留；`onApprovalRequest` 不换 assistant id（见其内
    // 注释），flush 目标全程稳定。
    let pendingTextChunks = "";
    let textChunkFlushScheduled = false;
    const flushPendingTextChunks = () => {
      textChunkFlushScheduled = false;
      const batch = pendingTextChunks;
      pendingTextChunks = "";
      if (!batch) return;
      const target = currentAssistantIdRef.current;
      if (!target) return;
      setSessions((prev) =>
        updateSession(prev, sessionId, (s) =>
          appendAssistantChunk(s, target, batch, new Date().toISOString()),
        ),
      );
    };
    const appendChunk = (chunk: string) => {
      streamPhaseRef.current = "text";
      pendingTextChunks += chunk;
      if (!textChunkFlushScheduled) {
        textChunkFlushScheduled = true;
        requestAnimationFrame(flushPendingTextChunks);
      }
    };

    // reasoning chunk 攒批（与上方 pendingTextChunks 同一模式）：Thinking
    // 事件是逐 token 的，此前每个 chunk 都走一次 setSessions（O(N) map +
    // App 级重渲）——模型思考越久越卡，是「一思考就掉帧」的元凶。改为闭包
    // 局部 buffer + rAF 合帧，同帧 N chunks → 1 次重渲。applyReasoningChunk
    // 对合并后的长 piece 语义等价（追加 + 末尾 text step 转换是边沿行为，
    // 与 piece 切分无关）。done/error 处与文本攒批一起强制 flush。
    let pendingReasoningChunks = "";
    let reasoningChunkFlushScheduled = false;
    const flushPendingReasoningChunks = () => {
      reasoningChunkFlushScheduled = false;
      const batch = pendingReasoningChunks;
      pendingReasoningChunks = "";
      if (!batch) return;
      const target = currentAssistantIdRef.current;
      if (!target) return;
      setSessions((prev) => updateSession(prev, sessionId, (s) => {
        return {
          ...s,
          messages: s.messages.map((m) => {
            if (m.id !== target) return m;
            // 逐 chunk 拼批后的单次应用：判定与状态转移都在
            // `streamEvents::applyReasoningChunk`（2026-10-01 前这里是
            // 内联 40 行副本，抽出是为了能测——见其注释）。
            const next = applyReasoningChunk(
              m.unified_steps,
              m.content,
              m.reasoningContent,
              batch,
            );
            return {
              ...m,
              content: next.content,
              reasoningContent: next.reasoningContent,
              unified_steps: next.steps,
            };
          }),
          updated_at: new Date().toISOString(),
        };
      }));
    };
    const appendReasoningChunk = (chunk: string) => {
      streamPhaseRef.current = "reasoning";
      pendingReasoningChunks += chunk;
      if (!reasoningChunkFlushScheduled) {
        reasoningChunkFlushScheduled = true;
        requestAnimationFrame(flushPendingReasoningChunks);
      }
    };

    // 子代理 / 成员事件攒批：后端每步推全量 recentSteps，此前每个事件
    // 都立即整对象替换一次 state（高频时放大为连续重渲）。事件先落
    // pending map，rAF 合帧后一次合并写入——同帧多事件 → 1 次渲染。
    // done/error 的 flush 不需要：最后一次事件最多晚一帧上屏，无一致性
    // 判据读它（与文本/reasoning 的"done 前必须落盘"不同）。
    let pendingSubAgentUpdates: Record<
      string,
      SubAgentEventPayload & { updatedAt: number }
    > = {};
    let pendingMemberUpdates: Record<
      string,
      MemberEventPayload & { updatedAt: number }
    > = {};
    let subAgentFlushScheduled = false;
    const flushPendingAgentEvents = () => {
      subAgentFlushScheduled = false;
      if (Object.keys(pendingSubAgentUpdates).length) {
        const batch = pendingSubAgentUpdates;
        pendingSubAgentUpdates = {};
        setSubAgents((prev) => ({ ...prev, ...batch }));
      }
      if (Object.keys(pendingMemberUpdates).length) {
        const batch = pendingMemberUpdates;
        pendingMemberUpdates = {};
        setMemberTickets((prev) => ({ ...prev, ...batch }));
      }
    };
    const queueAgentEventFlush = () => {
      if (!subAgentFlushScheduled) {
        subAgentFlushScheduled = true;
        requestAnimationFrame(flushPendingAgentEvents);
      }
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
      // 会话级放手模式（2026-10-31）：后端已在 ApprovalRegistry 直接放行
      // （预解析 Approved，工具照常执行），这张卡永远等不到答复 —— 不渲染、
      // 不占 resolver，直接以已批准结算。localStorage 是同步真值
      // （usePermissionLevel 写同一键），切换的瞬间即生效。
      if (storage.getItem(STORAGE_KEYS.permissionLevel) === "full") {
        return Promise.resolve({ decision: "approved" as ApprovalDecision });
      }
      // 审批等待是整条链路里唯一「agent 完全停住、零流量」的静默窗口
      //（默认 600s 后自动拒绝）。窗口失焦时抽屉/内联卡用户全看不到，
      // 感知就是「模型卡死了」——只有系统通知能把这次等待变成一次
      // 可点击的提醒。前台时不打扰（抽屉本来就可视）。
      // 仅首次到达时通知（自救重投影会再次进入本函数，不能反复弹）。
      if (!approvalBridge.resolvers.has(req.requestId) && !document.hasFocus()) {
        sendNotification(
          "Hermix 等待你的审批",
          `${req.tool}${req.reason ? `：${req.reason}` : ""}（未响应将在 10 分钟后自动拒绝）`,
        ).catch(() => {});
      }
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
        // 没有可挂卡的 assistant（会话刚切换 / 历史还没装载 / 重连后状态
        // 已丢）：审批卡**渲染不出来**，用户永远点不到。
        //
        // 旧实现返回一个永不 resolve 的 promise，于是前端不回包、服务端停在
        // 审批等待 —— 父 turn 连同调用它的会话一起永久卡死。子代理内部的
        // terminal 审批正是这样把 delegate_task 拖死的：父等子、子等审批、
        // 审批等一张画不出来的卡。
        //
        // 没有界面可决时，**默认拒绝**比无限挂起安全：拒绝是可见的（工具结果
        // 里带原因，模型据此换方案），挂起是无声的。note 会作为拒绝理由进模型
        // 上下文。
        return Promise.resolve({
          decision: "denied" as const,
          note: "审批卡没有可承载的消息位置（会话状态已变），已按默认拒绝处理；如果仍需要执行，请重新发起。",
        });
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
      setSessions((prev) =>
        updateSession(prev, sessionId, (s) =>
          appendApprovalRequestStep(s, previousAssistantId, step, req.requestId, new Date().toISOString()),
        ),
      );

    // Set a frontend timeout to auto-mark as "expired" when the
    // server-emitted deadline passes. The backend already auto-denies
    // at 30s, but without this timer the card UI stays "等待你的决定"
    // forever — the user sees buttons that no longer do anything.
    if (req.expiresAt) {
      const delay = Math.max(0, req.expiresAt - Date.now());
      const timer = setTimeout(() => {
        approvalBridge.timeoutTimers.delete(req.requestId);
        resolveApproval(req.requestId, "expired");
      }, delay);
      approvalBridge.timeoutTimers.set(req.requestId, timer);
    }

    // 静默丢失自救：15s 后仍未决 → 直查服务端待审批注册表。
    // 还挂着 → 走本函数重新投影（幂等：step 写入按 requestId 去重、
    // resolver 重注册、通知被上面的首次到达守卫跳过），并在重投影中
    // 重新武装下一轮 —— 15s 一次的重试能穿越 UI 冻结期，直到用户点击
    // 或服务端超时自动拒绝为止。不在了 → 本地标记过期，卡片不再悬挂。
    // 桥/服务端整个不可达时 fetch 会 reject —— 尽力而为，静默放弃。
    const rescueTimer = setTimeout(() => {
      approvalBridge.rescueTimers.delete(req.requestId);
      if (!approvalBridge.resolvers.has(req.requestId)) return;
      void api
        .getPendingApprovals(req.sessionId)
        .then((pending) => {
          if (!approvalBridge.resolvers.has(req.requestId)) return;
          if (pending.some((it) => it.request_id === req.requestId)) {
            void onApprovalRequest(req);
          } else {
            resolveApproval(req.requestId, "expired");
          }
        })
        .catch(() => {});
    }, 15_000);
    approvalBridge.rescueTimers.set(req.requestId, rescueTimer);

    return new Promise<{ decision: ApprovalDecision; remember?: boolean; note?: string }>((resolve) => {
      approvalBridge.resolvers.set(
        req.requestId,
        (decision, remember, note) => resolve({ decision, remember, note }),
      );
      // record which session owns this request so the REST fallback hits the
      // right `/v1/sessions/<sid>/approval` endpoint
      approvalBridge.pendingSessions.set(req.requestId, sessionId);
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
      // 会话级放手模式（2026-11-10）：与单卡路径（onApprovalRequest）同款
      // 即结算。批量卡此前没有这层检查——放手模式下批量卡照样渲染并等
      // 点击（11-09 超星验证任务实测）。后端 resolve_batch 对已由放手
      // 预解析（Approved）的请求幂等返回 matched=false，无副作用。
      if (storage.getItem(STORAGE_KEYS.permissionLevel) === "full") {
        return Promise.resolve({ decision: "approved" as ApprovalDecision });
      }
      const previousAssistantId =
        currentAssistantIdRef.current
        ?? [...(sessions.find((s) => s.id === req.sessionId)?.messages ?? [])]
          .reverse()
          .find((m) => m.role === "assistant")?.id
        ?? null;
      if (!previousAssistantId) {
        // 同单条审批：卡片无处渲染 → 默认拒绝，绝不无限挂起。
        return Promise.resolve({
          decision: "denied" as const,
          note: "批量审批没有可承载的消息位置（会话状态已变），已按默认拒绝处理。",
        });
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
      setSessions((prev) =>
        updateSession(prev, req.sessionId, (s) =>
          appendApprovalBatchStep(s, previousAssistantId, step, req.batchId, new Date().toISOString()),
        ),
      );

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
          approvalBridge.timeoutTimers.delete(req.batchId);
          resolveApprovalBatch(req.batchId, "expired");
        }, delay);
        approvalBridge.timeoutTimers.set(req.batchId, timer);
      }

      return new Promise<{ decision: ApprovalDecision; remember?: boolean; note?: string }>((resolve) => {
        approvalBridge.batchResolvers.set(
          req.batchId,
          (decision, remember, note) => resolve({ decision, remember, note }),
        );
        approvalBridge.pendingSessions.set(req.batchId, req.sessionId);
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

        setSessions((prev) =>
          updateSession(prev, sessionId, (s) =>
            appendChatMessage(s, clarifyMsg, new Date().toISOString()),
          ),
        );
      }

      return new Promise<{ answer: string }>((resolve) => {
        const list = clarifyResolversRef.current.get(req.requestId) ?? [];
        list.push((answer) => resolve({ answer }));
        clarifyResolversRef.current.set(req.requestId, list);
      });
    };

    // ── 编排总线事件（ws.rs 的 orchestration 桥）──────────────
    // 桌面端只消费两类：自动化触发弹 toast；计划确认插一张
    // 只读卡片（后端无确认回传通道，点了"知道了"即消失）。
    // Task 生命周期事件留空——桌面暂无任务面板，接了也是噪音。
    const onOrchestrationEvent = (ev: import("./hooks/useStreamChat").OrchestrationEventInfo) => {
      if (ev.type === "automation_triggered") {
        const name = ev.automationName || ev.automationId || "自动化任务";
        toastInfo(`自动化「${name}」已触发`);
        return;
      }
      if (ev.type === "plan_confirmation_required") {
        const planMsg: ChatMessage = {
          id: `plan-${ev.taskId}-${Date.now()}`,
          role: "plan",
          content: ev.planText,
          timestamp: new Date().toISOString(),
          planConfirm: {
            taskId: ev.taskId,
            planText: ev.planText,
            acknowledged: false,
          },
        };
        setSessions((prev) =>
          updateSession(prev, sessionId, (s) =>
            appendChatMessage(s, planMsg, new Date().toISOString()),
          ),
        );
      }
    };

    // ── tool_stdout 批处理 ─────────────────────────────────────
    // 每个 stdout chunk 原本各跑一次全量 setSessions（map 所有
    // session→message→unified_steps）。大输出以帧级速率涌回时，
    // setState 队列把 JS 主线程打满——WebView 内容区失去响应但系统
    // 资源正常（"调用工具就卡死"的前端半边根因）。改为：chunk 先落
    // 进闭包局部 Map，requestAnimationFrame 合并后一次 setSessions
    // 应用全部增量（同帧 N 次 → 1 次）。闭包变量随 send 天然隔离；
    // done/error 强制 flush，尾批 chunk 不滞留。
    const pendingToolChunks = new Map<string, string>();
    let toolChunkFlushScheduled = false;
    // `onlyToolCallId` 只把某一个工具的待合并分片落库（见 onToolComplete：
    // 完成事件必须排在它自己的 stdout 之后，否则分片会落进「已关闭」的
    // 调用里，渲染成一行永远转圈的「调用工具」）。
    const flushPendingToolChunks = (onlyToolCallId?: string) => {
      // 整批 flush（rAF 回调 / done / error）才算把这一帧的调度消费掉；
      // 定向 flush 不动它，否则同帧剩下的分片会失去下一帧的 flush 机会。
      if (onlyToolCallId === undefined) toolChunkFlushScheduled = false;
      if (pendingToolChunks.size === 0) return;
      let pending: Map<string, string>;
      if (onlyToolCallId) {
        const buffered = pendingToolChunks.get(onlyToolCallId);
        if (buffered === undefined) return;
        pendingToolChunks.delete(onlyToolCallId);
        pending = new Map([[onlyToolCallId, buffered]]);
      } else {
        pending = new Map(pendingToolChunks);
        pendingToolChunks.clear();
      }
      const target = currentAssistantIdRef.current;
      if (!target) return;
      setSessions((prev) =>
        updateSession(prev, sessionId, (s) =>
          applyToolStdoutChunks(s, target, pending, new Date().toISOString()),
        ),
      );
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
            // 强制 flush 尾批 tool_stdout —— error 后不再有 rAF 机会，
            // 不 flush 会把最后一批 stdout 滞留在 pending Map 里丢掉。
            flushPendingToolChunks();
            // 文本 chunk 同理：error 文本要写进同一条 assistant，
            // 未 flush 的尾批文本会在 `= null` 之后被丢掉。
            flushPendingTextChunks();
            flushPendingReasoningChunks();
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
            // 免费渠道死模型自愈：上游 404 "does not exist" 意味着该 id 已被
            // 下架（不是限流/抖动）。抽出 id 上报后端，从免费清单里摘除——
            // 免费清单「只增不删」，不摘的话死模型永远留在下拉里，
            // 用户每次选中都 404（实测 minimax-m3.1-flash:free）。
            //
            // 2026-10-30 补 `unknown model 'x'`：不是所有厂商都用 404。
            // MiniMax 报的是 400 + 错误码 2013（`invalid params, unknown
            // model 'minimax:minimax-m3.1-flash'`），语义完全相同——provider
            // 根本不认识这个 id。只认 404 时，小写 slug 会让**每一轮**撞
            // 轮次预算的收尾摘要都失败（`handle_max_iterations`），而那个死
            // 模型永远留在默认位上。
            //
            // fire-and-forget：摘除成功广播模型配置变更，下拉立即刷新。
            const deadModelRaw =
              rawForFriendly.match(/'([^']{2,120})' does not exist/i)?.[1] ??
              rawForFriendly.match(/unknown model '([^']{2,120})'/i)?.[1];
            // 错误串里可能带 `provider:model` 前缀，后端只认裸 model_id。
            const deadModel = deadModelRaw
              ? deadModelRaw.includes(":")
                ? deadModelRaw.split(":").slice(1).join(":")
                : deadModelRaw
              : undefined;
            if (deadModel) {
              void api
                .reportDeadModel(deadModel)
                .then((r) => {
                  if (r.removed) {
                    window.dispatchEvent(new CustomEvent("hermix:model-config-changed"));
                  }
                })
                .catch(() => undefined);
            }
            // 401 同样是死信号——但只对**免费渠道**成立：免费路由匿名可调，
            // 401 只能是路由被上游关掉；付费厂商 401 是用户自己的 key 坏了，
            // 摘模型等于掩盖真实问题。后端 remove_dead_model 本就只动免费
            // 渠道，这里再按 provider 前缀挡一道，语义双保险。
            if (!deadModel && /(^|\D)401(\D|$)|unauthorized/i.test(rawForFriendly)) {
              const m = sessionsRef.current.find((s) => s.id === sessionId)?.model;
              const freeModel =
                m && m.startsWith("custom-free-") ? m.split(":").slice(1).join(":") : undefined;
              if (freeModel) {
                void api
                  .reportDeadModel(freeModel)
                  .then((r) => {
                    if (r.removed) {
                      window.dispatchEvent(new CustomEvent("hermix:model-config-changed"));
                    }
                  })
                  .catch(() => undefined);
              }
            }
            setStreamPhase(null);
            setSessions((prev) =>
              updateSession(prev, sessionId, (s) =>
                markAssistantError(s, assistantIdAtError ?? "", errText, new Date().toISOString()),
              ),
            );
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

            setSessions((prev) =>
              updateSession(prev, sessionId, (s) =>
                appendTimelineEvent(s, target, event, new Date().toISOString()),
              ),
            );
          },
          onToolChunk: ({ toolCallId, content }) => {
            // stdout chunk 不再逐个 setSessions —— 入队等 rAF 合并
            // flush（见 pendingToolChunks 批处理说明）。
            if (!content) return;
            const target = currentAssistantIdRef.current;
            if (!target) return;

            pendingToolChunks.set(
              toolCallId,
              (pendingToolChunks.get(toolCallId) ?? "") + content,
            );
            if (!toolChunkFlushScheduled) {
              toolChunkFlushScheduled = true;
              // 包一层：rAF 回调会拿到时间戳，直接传函数会让它被当成
              // `onlyToolCallId`。
              requestAnimationFrame(() => flushPendingToolChunks());
            }
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

            // 先把这个工具尚未合并的 stdout 分片落进 state，再追加
            // `tool_complete`。分片由 rAF 合并（下一帧才应用），完成事件
            // 却是立刻生效的 —— 不先冲掉，分片就会晚于自己的完成事件到达，
            // 落进一个已经关闭的调用里，渲染出一行永远转圈的「调用工具」。
            // 两次 setSessions 由 React 按调用顺序应用，顺序即保证。
            flushPendingToolChunks(toolCallId);

            const event: import("./types").ExecutionTimelineEvent = {
              type: "tool_complete",
              tool_call_id: toolCallId,
              content: content ?? "",
              created_at: new Date().toISOString(),
              // Server-authoritative failure flag; persisted so a reload
              // marks the card failed without inspecting the payload text.
              is_error: isError,
            };

            setSessions((prev) =>
              updateSession(prev, sessionId, (s) =>
                appendTimelineEvent(s, target, event, new Date().toISOString()),
              ),
            );
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
            setSessions((prev) => updateSession(prev, sessionId, (s) => {
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
            setSessions((prev) => updateSession(prev, sessionId, (s) => {
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
            setSessions((prev) => updateSession(prev, sessionId, (s) => {
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
            setSessions((prev) => updateSession(prev, sessionId, (s) => {
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
            // 攒批入口（见上方 appendReasoningChunk 注释）：逐 token 事件
            // 先落 buffer，rAF 合帧后一次 setSessions——
            // 模型思考越久越卡的元凶（逐 chunk O(N) map + 全树重渲）就此消除。
            if (piece) appendReasoningChunk(piece);
          },
          onSubAgentEvent: (info: SubAgentEventPayload) => {
            // Accumulate live sub-agent state keyed by subAgentId so the
            // "子代理" panel can show each delegated task's phase in real
            // time (spawned → started → done / failed). 攒批入口（见上方
            // pendingSubAgentUpdates 注释）：后端每步推全量 recentSteps，
            // 事件先落 pending map，rAF 合帧后一次合并写入。
            if (!info?.subAgentId) return;
            pendingSubAgentUpdates[info.subAgentId] = {
              subAgentId: info.subAgentId,
              phase: info.phase,
              task: info.task,
              role: info.role,
              turns: info.turns,
              costUsd: info.costUsd,
              error: info.error,
              recentSteps: info.recentSteps,
              updatedAt: Date.now(),
            };
            queueAgentEventFlush();
          },
          onMemberEvent: (info: MemberEventPayload) => {
            if (!info?.memberId) return;
            pendingMemberUpdates[info.memberId] = {
              memberId: info.memberId,
              phase: info.phase,
              task: info.task,
              turns: info.turns,
              error: info.error,
              recentSteps: info.recentSteps,
              updatedAt: Date.now(),
            };
            queueAgentEventFlush();
          },
          onResourceAlert: (kind, message) => {
            // 资源闸门边沿（总管方案 P0.5）：breach 红通知、recovered 常规通知。
            if (kind === "resource_breach") {
              toastError(message || "资源闸门越限，已停派新工单");
            } else if (kind === "resource_recovered") {
              toastInfo(message || "资源闸门恢复，重新放行派单");
            }
          },
          onApprovalRequest,
          onApprovalBatchRequest,
          onClarifyRequest,
          onOrchestrationEvent,
          // 用户点击停止时（cancelStream）在 WS 关闭前触发：直接把仍在途的
          // 审批请求以 denied 否决发往后端，使 agent loop 立即解除阻塞，而非
          // 干等其 300s 审批超时。clarify 同理以空答案解除阻塞。
          onCancel: () => {
            approvalBridge.resolvers.forEach((_resolver, requestId) => {
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
            approvalBridge.resolvers.clear();
            // Batch approvals: same "cancel ⇒ deny all" semantics
            // but wire a single batch frame so the server resolves
            // every member under that batch_id.
            approvalBridge.batchResolvers.forEach((_resolver, batchId) => {
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
            approvalBridge.batchResolvers.clear();
            approvalBridge.retryTimers.forEach((t) => clearInterval(t));
            approvalBridge.retryTimers.clear();
            approvalBridge.timeoutTimers.forEach((t) => clearTimeout(t));
            approvalBridge.timeoutTimers.clear();
            approvalBridge.pendingSessions.clear();
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
            // 强制 flush 尾批 tool_stdout —— 之后 rAF 可能不再跑
            // （后台/最小化），不 flush 会丢最后一批 stdout 分片。
            flushPendingToolChunks();
            // 文本 chunk 同理：done 的 fallback 判据读 target.content，
            // 未 flush 的尾批文本会让判据读到旧值。
            flushPendingTextChunks();
            flushPendingReasoningChunks();
            setPrefetchIntent(null);
            setStreamPhase(null);
            // `task_in_flight` settle: the backend saw this retry land on a
            // session whose agent is still running, so it closed THIS socket
            // (a terminal `Done` with finish_reason=task_in_flight). The real
            // turn keeps running on the in-flight agent task, and — crucially
            // — any approval card that was in flight when the socket closed
            // was NEVER re-delivered on the new connection, so it is now an
            // orphan the user can't see (and the agent is still blocked on
            // its `AwaitApproval`). Hydrate: pull the session's still-pending
            // approvals from the backend and re-project each one through the
            // normal `onApprovalRequest` path so the card renders again and a
            // click unblocks the loop. Empty result is a normal state (nothing
            // pending) — never settle the UI on it.
            if (stopReason === "task_in_flight") {
              void api
                .getPendingApprovals(sessionId)
                .then((pending) => {
                  for (const item of pending) {
                    void onApprovalRequest({
                      requestId: item.request_id,
                      tool: item.tool,
                      args: item.arguments,
                      expiresAt: item.expires_at,
                      sessionId,
                    });
                  }
                })
                .catch((e) =>
                  console.error("[APPROVAL-HYDRATE] task_in_flight reproject failed", e),
                );
            }
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
              setSessions((prev) =>
                updateSession(prev, sessionId, (s) =>
                  finalizeAssistantStopReason(s, assistantIdAtDone, stopReason, text, durationMs, new Date().toISOString()),
                ),
              );
              return true;
            })();
            if (!replaced) {
              // Fallback: stamp the trailing assistant row with
              // `stopReason` (preserves the old behavior when we
              // somehow don't have a target id).
              setSessions((prev) =>
                updateSession(prev, sessionId, (s) =>
                  stampTrailingAssistantStopReason(s, stopReason),
                ),
              );
            }
            // 持久化最终 assistant 消息的 unified_steps（前端构建的内联时间线）。
            // reload 时 resolveSteps 会优先用它在真实顺序渲染，工具不再被归到末尾。
            //
            // 2026-10-02：落库前先 `closeOpenReasoning`。一段思考的「结束」是
            // 下一段非 reasoning 的 step 追加的那一刻（见 `closeOpenReasoning`
            // 注释），但 Done 这条路上不经过 `appendTextStep` / `appendToolStep`，
            // 于是**轮次最后一段思考的 `endedAt` 一直是 undefined**，并被这样
            // 写进 `unified_steps`。reload 后 `ReasoningBlock.spanMs` 拿不到
            // endedAt → 「思考」行右边不显示耗时（用户实机反馈）。
            //
            // 实测（sessions.db 抽样）：近 40 条带 unified_steps 的消息里，
            // 含 reasoning 的 30 条中只有 2 条的段是「起止俱全」的。
            // Done 是这一轮唯一的收尾时刻，在这里补 endedAt 是准确的。
            if (assistantIdAtDone) {
              const finalMsg = sessionsRef.current
                .find((s) => s.id === sessionId)
                ?.messages.find((m) => m.id === assistantIdAtDone);
              if (finalMsg?.unified_steps && finalMsg.unified_steps.length > 0) {
                const closed = closeOpenReasoning(finalMsg.unified_steps);
                void api.saveUnifiedSteps(sessionId, closed);
                // 内存里也同步，否则本轮折叠头展开时「思考」行仍按「进行中」
                // 渲染（active 靠 endedAt 判定），和落库后的表现不一致。
                if (closed !== finalMsg.unified_steps) {
                  setSessions((prev) => updateSession(prev, sessionId, (s) => ({
                    ...s,
                    messages: s.messages.map((m) =>
                      m.id === assistantIdAtDone
                        ? { ...m, unified_steps: closed }
                        : m,
                    ),
                  })));
                }
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
              setSessions((prev) => updateSession(prev, sessionId, (s) => {
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
                    // 内存态的 `unified_steps` 也要补 —— 只补 `execution_timeline`
                    // 是「刷新后正确、当场永远转圈」：在线渲染读的是 steps。
                    let stepsPatched = m.unified_steps;
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
                        // 文案只陈述事实：**这一轮结束了，而这个工具没有回报结果**。
                        // 旧文案「执行已中断 — 工具未汇报完成事件」把「回合结束」
                        // 读成了「工具失败」，而真实情况常常相反（长命令仍在后端
                        // 跑，只是前端先结算了）。根因看后端日志的 finalize 出口，
                        // 不在卡片上，所以这里不暗示失败、也不加 [ERROR] 前缀。
                        const statusContent = hasStdout
                          ? ""
                          : "本轮已结束，该工具未回报结果";
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
                        stepsPatched = appendToolStep(stepsPatched, {
                          type: "tool_complete",
                          tool: startEv?.tool,
                          tool_call_id: id,
                          created_at: now,
                          ...extraFields,
                        });
                      }
                    }
                    if (patched.length === timeline.length) return m;
                    return { ...m, execution_timeline: patched, unified_steps: stepsPatched };
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
            approvalBridge.resolvers.forEach((resolver) => resolver("denied"));
            approvalBridge.resolvers.clear();
            // Batch resolvers share the same lifecycle — the WS
            // already closed, so the in-flight responses are moot;
            // mark them all `denied` (stale card → `expired` via the
            // missing-resolver branch in `resolveApprovalBatch`).
            approvalBridge.batchResolvers.forEach((resolver) => resolver("denied"));
            approvalBridge.batchResolvers.clear();
            // Clean up retry timers
            approvalBridge.retryTimers.forEach((timer) => clearInterval(timer));
            approvalBridge.retryTimers.clear();
            // Clean up frontend timeout timers
            approvalBridge.timeoutTimers.forEach((timer) => clearTimeout(timer));
            approvalBridge.timeoutTimers.clear();
            // clear the approval session-ownership map too — every pending
            // approval just got force-denied, so the REST fallback has nothing
            // to look up.
            approvalBridge.pendingSessions.clear();
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
            //
            // Tracked + ownership-checked (see clearSubAgentsAfter): a
            // bare setTimeout here would wipe the NEXT turn's live
            // sub-agent panel whenever the user replies within 3s.
            clearSubAgentsAfter(sessionId);
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
          // P7 前端 seq 对账（设计 §3.5/§6.3）：WS 连接丢失后（bridge
          // 死 / listener 注册失败），useWsConnection 的 catch 路径已拉
          // 一次订阅快照对账，把结果交到这里消费。`fullSnapshot` 双分支：
          // - true（落后）→ 用快照 messages 整体替换该 session 的本地
          //   会话状态（丢弃本地、以后端权威源为准）
          // - false（已追上空增量）→ 仅刷新本地 seq 锚，不替换消息
          //   （本地状态 == 服务端，无需动数据）
          // 消息类型：订阅端点回的服务端 `SessionMessagesResponse.messages`
          // （ProtocolMessage 形态）。本地回填必须过一次
          // `wireArrayToChatMessages`——与 `GET /v1/sessions/{id}/messages`
          // 的加载路径同一个适配器（protocolAdapter：tool 行并入
          // assistant、approval 行补 meta、snake→camel、坏行跳过）。
          // 曾经直接 `as ChatMessage[]` 断言，重连后 tool/approval 行渲染
          // 异常。best-effort：对账失败（bridge 死时快照调用 reject）由
          // useWsConnection 吞掉，这里收不到回调。
          onReconciled: (snapshot) => {
            if (snapshot.fullSnapshot) {
              const reloaded = wireArrayToChatMessages(
                snapshot.messages as WireMessage[],
                snapshot.sessionId,
                detectToolError,
              );
              setSessions((prev) => updateSession(prev, snapshot.sessionId, (s) => {
                return {
                  ...s,
                  messages: reloaded,
                  updated_at: new Date().toISOString(),
                };
              }));
            }
            // fullSnapshot=false（已追上）：仅刷新本地 seq 水位（由
            // useStreamParser 的 ctx.lastLocalSeq 持有，已在本批次
            // hooks 层完成），UI 不替换消息——空增量意味着本地状态
            // 已是最新。此处无需额外 UI 动作。
          },
        },
        finalOverrides,
      );
      return true;
    } catch (e) {
      // (React 18 batching bug fix): same capture pattern
      // as onError above. By the time catch runs, onError may have
      // already set currentAssistantIdRef.current = null. Capture
      // first, use inside setSessions updater.
      const assistantIdAtCatch = currentAssistantIdRef.current;
      const errText = localizeStreamError(String(e));
      setSessions((prev) => updateSession(prev, sessionId, (s) => {
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
      return false;
    }
  }, [activeSessionId, sessions, sendStream]);

  // pop a queued item out of turn and dispatch it
  // immediately. Defined here (after `handleSendMessage`) so the
  // useCallback deps array can reference it without tripping TSC's
  // "used before declaration" check.
  // !streaming → call handleSendMessage directly, then drop the
  // row from the queue. If the send fails, re-prepend
  // so the user can retry.
  // streaming  → move the row to the head AND stop the running turn.
  // Once the stop settles (`streaming` → false) the drain effect below
  // (keyed on `[streaming, messageQueue, …]`) sends that head row —
  // 「立即」是真的立即，不再是「等当前回复跑完」。
  const sendFromQueue = useCallback(
    async (id: string) => {
      const item = messageQueue.find((m) => m.id === id);
      if (!item) return;
      // 队列现在是会话级：别的会话的条目不能在这里发出去。
      if ((item.sessionId ?? null) !== (activeSessionId ?? null)) {
        toastError("这条消息属于另一个会话，请切回该会话后再发送");
        return;
      }
      if (streaming) {
        // 「立即发送」= 停掉当前回复，等它收尾后马上发这条。
        // 旧实现只把条目挪到队首就返回（还得等当前回复自己跑完）——
        // 按钮写着"立即"却毫无立即之处，用户读作「这按钮是坏的」。
        setMessageQueue((prev) => {
          const found = prev.find((m) => m.id === id);
          if (!found) return prev;
          // 手动点发送即视为"重新排队"：清掉 failed / 重试计数，
          // 否则排空会因为 status==="failed" 或 attempts>=3 跳过它。
          return [
            { ...found, status: "queued" as const, error: undefined, attempts: 0 },
            ...prev.filter((m) => m.id !== id),
          ];
        });
        // 只有「正在跑的就是这条消息所属的会话」才停：别的会话在跑时
        // 停它会误杀别人的轮次（cancel 本身也会因 session 不匹配拒绝）。
        if (streamingSessionId === (activeSessionId ?? null)) {
          const asked = cancelStream(activeSessionId ?? undefined);
          toastInfo(
            asked
              ? "已停止当前回复，这条消息马上发送"
              : "停止指令未能送达，这条消息已排到队首",
          );
        } else {
          toastInfo("另一个会话正在生成，这条消息已排到队首");
        }
        return;
      }
      setMessageQueue((prev) => prev.filter((m) => m.id !== id));
      const ok = await handleSendMessage(
        item.text,
        undefined,
        item.overrides,
        item.images || item.attachedFilePaths || item.attachedFiles
          ? { images: item.images, attachedFilePaths: item.attachedFilePaths, attachedFiles: item.attachedFiles }
          : undefined,
      );
      if (!ok) {
        setMessageQueue((prev) => [
          { ...item, status: "failed", error: "发送失败", attempts: (item.attempts ?? 0) + 1 },
          ...prev,
        ]);
        toastError("发送失败，消息已留在队列中");
      }
    },
    [messageQueue, activeSessionId, streaming, streamingSessionId, cancelStream, handleSendMessage],
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
      // selectedModel 时不得用旧值(默认模型)覆盖已选择的模型。
      if (activeSessionId) {
        setSessions((prev) =>
          updateSession(prev, activeSessionId, (s) => ({
            ...s,
            model: newId,
            updated_at: new Date().toISOString(),
          })),
        );
      }
      resetChatStateForModelSwitch({
        cancelStream,
        // Anchor the cancel to the session the user is actually in. Without
        // it, switching models while viewing session B killed session A's
        // live stream and denied A's pending approvals.
        activeSessionId,
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
        approval: approvalBridge,
        clarifyResolversRef,
        runningToolRef,
        currentAssistantIdRef,
        acuiReset: acui.reset,
      });

      // 选中即校准：拉这个模型的 provider 级真实窗口（endpoints）。
      //
      // 模型列表里的上下文默认取 OpenRouter 目录的聚合值，而 `top_provider`
      // 只是某一家 provider 的窗口，可能与我们实际连的那家不同
      // （实测 nemotron 模型级 1M / 某家 256K）。不校准会让压缩阈值估高，
      // 请求拖到超出上游窗口才触发压缩（400）。
      //
      // fire-and-forget：校准失败不影响切换；成功后后端已写缓存，下一次
      // 解析（含压缩阈值）就是校准后的值。
      void api.refreshModelContext(newId).catch(() => undefined);
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
  // 排空守卫：一次只发一条，避免 effect 在 streaming 状态落定前重入。
  const drainingRef = useRef(false);
  useEffect(() => {
    if (streaming || drainingRef.current) return;
    // 只排当前会话的条目：队列是会话级的，A 的待发消息不能进 B。
    const sessionKey = activeSessionId ?? null;
    const head = messageQueue.find((m) => (m.sessionId ?? null) === sessionKey);
    if (!head) return;
    // 失败条目停在队首等用户处理（重试 / 编辑 / 删除），不自动重放。
    if (head.status === "failed") return;
    if ((head.attempts ?? 0) >= 3) return;

    drainingRef.current = true;
    setMessageQueue((prev) => prev.filter((m) => m.id !== head.id));
    void handleSendMessage(
      head.text,
      undefined,
      // 用入队时的快照，而不是排空时 activeSession 的模型/权限/专家。
      head.overrides,
      head.images || head.attachedFilePaths || head.attachedFiles
        ? { images: head.images, attachedFilePaths: head.attachedFilePaths, attachedFiles: head.attachedFiles }
        : undefined,
    )
      .then((ok) => {
        if (!ok) {
          setMessageQueue((prev) => [
            {
              ...head,
              status: "failed" as const,
              attempts: (head.attempts ?? 0) + 1,
              error: "发送失败",
            },
            ...prev,
          ]);
        }
      })
      .finally(() => {
        drainingRef.current = false;
      });
  }, [streaming, messageQueue, activeSessionId, handleSendMessage]);

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
      // 全量重拉后本地就是完整列表，翻页入口随之关闭。
      setHistoryPaging((prev) => ({ ...prev, [activeSessionId]: { hasMore: false } }));
      setSessions((prev) => updateSession(prev, activeSessionId, (s) => ({ ...s, messages })));
    } catch (e) { reportError("app", e); }
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
      // 全量重拉后本地就是完整列表，翻页入口随之关闭。
      setHistoryPaging((prev) => ({ ...prev, [activeSessionId]: { hasMore: false } }));
      setSessions((prev) => updateSession(prev, activeSessionId, (s) => ({ ...s, messages })));
    } catch (e) { reportError("app", e); }
  }, [activeSessionId]);

  // ── ChatView 的稳定回调 ──────────────────────────────────────────
  // ChatView 挂了 React.memo：这里任何内联箭头函数/对象字面量都会在
  // App 每次渲染时产生新引用，击穿 memo 让整棵聊天树陪着重渲染。
  const consumePrefill = useCallback(() => setPendingPrefill(null), []);
  const consumeFileAttachment = useCallback(() => setPendingFileAttachment(null), []);
  const openExperts = useCallback(() => setPage("experts"), [setPage]);
  const expandSidebar = useCallback(() => setSidebarOpen(true), []);
  const openSearch = useCallback(() => setSearchOpen(true), []);
  // 清空只清当前会话：别的会话的待发消息不受影响。
  const clearQueue = useCallback(() => {
    const key = activeSessionId ?? null;
    setMessageQueue((prev) => prev.filter((m) => (m.sessionId ?? null) !== key));
  }, [activeSessionId]);
  const addToQueue = useCallback((msg: QueuedMessage) => {
    // 不再按文本去重：连发两次「继续」是合法指令，之前第二次会被静默吞掉。
    // 防连按 Enter 由 ChatView 的 500ms 同文本窗口负责。
    setMessageQueue((prev) => [
      ...prev,
      {
        ...msg,
        sessionId: msg.sessionId ?? activeSessionId ?? null,
        status: msg.status ?? "queued",
        attempts: msg.attempts ?? 0,
        createdAt: msg.createdAt ?? new Date().toISOString(),
      },
    ]);
  }, [activeSessionId]);
  const handleTodoPatch = useCallback(async (id: string, patch: { status?: string; title?: string; priority?: string }) => {
    try {
      // sessionId 必传：后端从 query 读它，缺省会产生 session_id=None 的
      // 孤儿行，重连快照不再包含该 todo（routes.rs 注释明示）。
      await api.patchTodo(id, patch, activeSessionId ?? undefined);
    } catch (e) {
      console.error('[App] patchTodo failed', e);
      throw e;
    }
  }, []);
  const retryLoadMessages = useCallback(() => {
    if (activeSessionId) {
      void handleSelectSession(activeSessionId);
    }
  }, [activeSessionId, handleSelectSession]);
  const activeChatHistoryPaging = useMemo(
    () => activeSession ? {
      hasMore: historyPaging[activeSession.id]?.hasMore ?? false,
      loading: historyLoading,
      onLoadMore: loadOlderMessages,
    } : null,
    [activeSession, historyPaging, historyLoading, loadOlderMessages],
  );

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

  // 输入框只显示当前会话的待发队列（会话级 outbox）。
  const queueForActiveSession = useMemo(
    () => messageQueue.filter((m) => (m.sessionId ?? null) === (activeSessionId ?? null)),
    [messageQueue, activeSessionId],
  );

  const chatViewNode = (
    <ChatView
      session={activeSession}
      historyPaging={activeChatHistoryPaging}
      onSendMessage={handleSendMessage}
      onNewChat={handleNewChat}
      pendingPrefill={pendingPrefill}
      onConsumePrefill={consumePrefill}
      // inline todo cards in the message stream.
      // App owns the groups store; ChatView renders one card per
      // group right after the originating assistant message.
      todoGroups={todoGroups}
      subAgents={subAgents}
        memberTickets={memberTickets}
      onTodoPatch={handleTodoPatch}
      justPatchedTodoRef={justPatchedTodoRef}
      onOpenExperts={openExperts}
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
      onAcknowledgePlan={handleAcknowledgePlan}
      acuiState={acui.state}
      acuiBus={acuiBus}
      acuiStats={acui.stats}
      messageQueue={queueForActiveSession}
      onAddToQueue={addToQueue}
      onClearQueue={clearQueue}
      onRemoveFromQueue={removeFromQueue}
      onEditQueueItem={editQueueItem}
      onSendFromQueue={sendFromQueue}
      messageLoadError={
        activeSessionId ? messageLoadErrors.get(activeSessionId) ?? null : null
      }
      onRetryLoad={retryLoadMessages}
      streamError={streamErrorForView}
      onClearStreamError={clearStreamError}
      onRetryLastMessage={retryLastMessage}
      prefetchIntent={prefetchIntent}
      rightOpen={rightOpen}
      onToggleRight={handleToggleRight}
      sidebarCollapsed={!sidebarOpen}
      onExpandSidebar={expandSidebar}
      onOpenSearch={openSearch}
      onEnterWelcome={handleEnterWelcome}
      onPathClick={handlePathClick}
      pendingFileAttachment={pendingFileAttachment}
      onConsumeFileAttachment={consumeFileAttachment}
    />
  );
  const routesCtx: RouteContext = {
    sessions,
    handleSelectSession,
    handleDeleteSession,
    selectSessionOnly,
    upsertSessions,
    startCreateAppFlow,
    startCreateWorkflowFlow,
    chatNode: chatViewNode,
    activeSessionId,
    // 运行工作流后的跳转：落到新对话空屏，不落在可能早已过期的激活会话
    // （如工作台会话）。跑起来的会话出现在侧栏，用户自行点入。
    openChat: () => {
      setActiveSessionId(null);
      setPage("chat");
    },
  };

  return (
    <AppProviders value={streamChatApi}>
    <div
      className="app-shell"
      /* 三栏布局的列宽变量与开合状态挂在**壳**上而不是 .app-body：标题栏要
         用同一组值做三栏对齐（见 components-layout.css 的 .titlebar 段），
         而 CSS 变量不往上继承，只能让两者从共同的父级各自取一份。 */
      data-left-open={sidebarVisible ? "true" : "false"}
      data-right-open={rightOpen ? "true" : "false"}
      style={{
        "--ab-left-w": `${left.width}px`,
        "--ab-right-w": rightOpen ? `${right.width}px` : "0px",
        "--pane-main-min": `${mainMinWidth}px`,
        "--left-min": `${leftMin}px`,
      } as React.CSSProperties}
    >
      <TitleBar title={page === "chat" ? activeSession?.title : undefined} />
      <SessionPickerDialog
        open={searchOpen}
        sessions={chatSessions}
        onSelect={handleSelectSession}
        onClose={() => setSearchOpen(false)}
      />
      <SettingsDialog
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        initialCategory={configCategory}
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
              aria-label={CLOSE}
              onClick={(e) => { e.stopPropagation(); dismissDenialBanner(); }}
            >×</button>
          </div>
        );
      })()}
      <div
        className="app-body"
      >
        {sidebarVisible && <div className="pane-overlay md:hidden" onClick={() => setSidebarOpen(false)} />}

        {/* Left sidebar — grid column 1 controls width; wrapper handles mobile slide */}
        <div
          className={`${sidebarVisible ? "translate-x-0" : "-translate-x-full"} transition-transform duration-slow absolute md:relative z-20 h-full min-h-0 overflow-hidden`}
        >
          <ErrorBoundary name="侧边栏">
          <Sidebar
            page={page}
            onPageChange={setPage}
            sessions={chatSessions}
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
          {!sidebarVisible && page !== "chat" && (
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
        {/* Right sidebar — grid column 3. 常挂载 + display:none 隐藏：关闭只是
            收起（列宽 0），不卸载组件树 —— 否则每次重开都全量重挂、ComputerTab
            重新拉目录，表现为「切换/关闭卡」。
            左侧 8px 内缩是「案」面露出来的缝：主列与本列因此读作两张并排卡片，
            而非一条 1px 线切开的两半（见 components-layout.css 的 .pane-main
            注释）。顶部不内缩 —— 标签条要紧贴标题栏下沿，连成一条水平带。 */}
        <div
          className="h-full flex overflow-hidden min-w-0 min-h-0 pl-2 pb-2"
          style={rightOpen ? undefined : { display: "none" }}
        >
          <div
            data-tauri-no-drag
            onMouseDown={right.onMouseDown}
            className="pane-resizer pane-resizer-right hidden md:block"
          />
          <ErrorBoundary name="预览栏">
          <PreviewBar
            tabs={previewTabs}
            activeId={activePreviewId}
            onActivate={setActivePreviewId}
            onClose={closePreviewTab}
            onOpenBrowser={openBrowserTab}
            onOpenReview={openReviewTab}
            onOpenSubagent={openSubagentTab}
            messages={activeSession?.messages}
            subAgents={subAgents}
        memberTickets={memberTickets}
            workspaceRoot={workspaceRoot}
            onAddFileToChat={handleAddFileToChat}
            onPreviewFile={handlePreviewFileFromExplorer}
            onCollapse={() => setRightOpen(false)}
          />
          </ErrorBoundary>
        </div>
        {/* 右栏收起/展开入口的归属（2026-10-04 与左栏对称）：
            · 面板开着 → 收起按钮在右栏自己的标签条右端（PreviewBar onCollapse）；
            · 面板收起 → ChatView 标题行的 PanelRight 按钮（showRightToggle
              仅在收起态渲染，不依赖 displayTitle，新建会话没标题时也在）；
            · 点对话正文里的路径 / 产物 → openRightPane（视口不足先向右扩窗）；
            · 目录 chip → openComputer（同样走 openRightPane）。
            2026-10-03 曾把收起职责整个挪进标题行，两处并存没有归属，已回退。 */}
      </div>

      {/* 预览统一走右侧预览栏（含 PreviewPanel 自带的全屏），不再有全屏模态。 */}
      <ToastContainer />
      <ImageLightbox />
      {/* 全局右键菜单：桌面端不给用户浏览器的网页菜单（重新加载/检查/…），
          换成本应用的菜单（剪切/复制/粘贴/复制图片/复制链接）。 */}
      <GlobalContextMenu />
    </div>
    </AppProviders>
  );
}
