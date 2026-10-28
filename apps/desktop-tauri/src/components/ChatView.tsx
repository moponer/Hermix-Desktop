import { useState, useRef, useEffect, useMemo, useCallback, memo } from "react";
import { useStreamChatContext } from "../context/StreamChatContext";
import { IM_SOURCE_LABEL, imSourceOf, type ImSource } from "../utils/time";
import { PanelRight, PanelLeftOpen, Search, ArrowUp, AlertTriangle } from "lucide-react";
import { SidebarRevealRow } from "./SidebarRevealRow";
import { resolveSessionSelectedModel } from "../utils/sessionModel";
import type {
  ApprovalBatchMessageMeta,
  ApprovalMessageMeta,
  ChatMessage,
  QueuedMessage,
  QueuedImage,
  QueuedFile,
  SubAgentState,
  Session, MemberTicketState } from "../types";
import type { ChatOverrides, StreamPhase, TodoSnapshotItem } from "../hooks/useStreamChat";
import type { ConfigCategory } from "./SettingsDialog";
import { CompactionStatusBanner } from "./CompactionStatusBanner";
import { mergeConsecutiveAssistantMessages, isRowRenderable, shouldShowAssistantHeader, mergeLegacyToolMessages } from "./chatUtils";
import type { PendingImage } from "./chatUtils";
import type { AcuiState } from "../acui/types";
import type { AcuiDecisionBus } from "../acui/AcuiRegistry";
import { AcuiPanel } from "../acui/AcuiPanel";
import { StreamErrorCard, type ErrorKind } from "./StreamErrorBanner";
import { WelcomeNight } from "./WelcomeNight";
import { useModelSelector } from "./ModelSelector";
import { useContextWindows } from "../hooks/useContextWindows";
import { toastError, toastInfo } from "../hooks/useToast";
import { injectIntoComposer, subscribeComposerInjection } from "../lib/composerInjection";
import { useChannelsStatus } from "../hooks/useChannelsStatus";
import { Composer } from "./Composer";
import { TokenUsagePopover } from "./TokenUsagePopover";
import { ApprovalDrawer, type PendingApprovals } from "./ApprovalDrawer";
import { MessageList, type MessageListHandle, type StreamRow } from "./MessageList";
import { useCustomApps } from "../hooks/useCustomApps";
import {
  MENTION_TRIGGER_APP,
  buildMentionPrefix,
} from "../lib/appMention";
import {
  usePermissionLevel,
  useDomainMode,
  useSessionExpert,
  useExpertCatalog,
  useRecentExperts,
  useSessionWorkspace,
  useSessionSkills,
} from "../hooks/useExpert";
import { useExpertCutoff } from "../hooks/useExpertCutoff";
import { useSendKey } from "../hooks/useSendKey";
import { useDesktopNotify } from "../hooks/useDesktopNotify";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { pickDirectory } from "../desktopBridge";
import { getAvailableTools, waitForBackendReady, updateSessionMetadata, warmUpProvider, getModelInfo, type ToolDescriptor } from "../api";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { compressImage, skipCompression, type CompressResult } from "../utils/imageCompressor";
import { NEW_CHAT } from "./strings";

// ── ModelStatusBar — 聊天列头部的动作行 ─────────────────────
//
// 2026-10-04：会话标题**上提到标题栏中栏**（标题栏已改成与三列对齐的网格：
// 左栏品牌 / 中栏标题 / 右栏窗口按钮）。本行此前也渲染标题，等于同一句话说
// 两遍；现在单一出口在标题栏，本行只剩动作按钮与 IM 远程徽标。
//
// 渲染条件随之上调为「有任一动作才渲染」：侧栏展开、右栏也开着、又没有 IM
// 徽标时整行消失，聊天区直接顶到标题栏下沿，把这 40px 让给内容。
function ModelStatusBar({
  imSource,
  rightOpen,
  onToggleRight,
  sidebarCollapsed,
  onExpandSidebar,
  onOpenSearch,
}: {
  /** 远程会话的来源（QQ / 企业微信 / 手机）—— 从那边发起的对话在头部标出来。 */
  imSource?: ImSource | null;
  rightOpen?: boolean;
  onToggleRight?: () => void;
  sidebarCollapsed?: boolean;
  onExpandSidebar?: () => void;
  onOpenSearch?: () => void;
}) {
  // 通道状态（机器人名 + 网关连接）。hook 必须在下方提前 return 之前调用。
  const channels = useChannelsStatus();
  const imChannel = imSource ? channels?.find((c) => c.platform === imSource) : undefined;
  // 侧栏折叠时，展开 + 搜索按钮嵌在本行左侧（用户拍板：与对话标题同一行，
  // 不另起浮层）。折叠但本行无任何可显示内容时也必须渲染 —— 否则展开入口
  // 随侧栏一起消失，用户再也打不开侧栏。
  const showLeftActions = !!sidebarCollapsed && !!onExpandSidebar;
  const imLabel = imSource
    ? `${IM_SOURCE_LABEL[imSource]}${imChannel?.name ? ` · ${imChannel.name}` : ""}`
    : null;
  const imTitle = imChannel
    ? imChannel.connected
      ? "通道已连接，可直接在 IM 里继续对话"
      : `通道未连接${imChannel.detail ? `：${imChannel.detail}` : ""}`
    : imSource === "device"
      ? "这条对话来自手机（经中转），在手机上继续即可"
      : "这条对话是从 IM 发起并远程驱动的";
  const showRightToggle = !!onToggleRight && !rightOpen;
  if (!showLeftActions && !imLabel && !showRightToggle) return null;
  // h-10：与 .sidebar-head、右栏标签条同高（三段头部是一条 40px 水平带，
  // 各列不再各飞各的基线）。
  return (
    <div
      className="h-10 shrink-0 flex items-center gap-2 px-4 sm:px-6 rounded-t-xl text-text-primary"
    >
      {showLeftActions && (
        <>
          <button
            type="button"
            onClick={onExpandSidebar}
            className="icon-btn"
            aria-label="展开侧边栏"
            title="展开侧边栏"
          >
            <PanelLeftOpen className="icon-md" />
          </button>
          {onOpenSearch && (
            <button
              type="button"
              onClick={onOpenSearch}
              className="icon-btn"
              aria-label="搜索对话"
              title="搜索对话"
            >
              <Search className="icon-md" />
            </button>
          )}
        </>
      )}
      {imLabel && (
        <span
          className="shrink-0 inline-flex items-center gap-1.5 rounded-full bg-bg-tertiary px-2 h-[20px] text-[length:var(--text-tiny)] text-text-secondary"
          title={imTitle}
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${
              // 手机没有"通道网关"，缺条目≠未连接，别误报红色
              !imChannel ? "bg-bg-active" : imChannel.connected ? "bg-success" : "bg-error"
            }`}
          />
          {imLabel} 远程
        </span>
      )}
      {showRightToggle && (
        <button
          onClick={onToggleRight}
          className="titlebar-btn ml-auto"
          title={rightOpen ? "关闭产物栏" : "打开产物栏"}
          aria-label="切换产物栏"
          data-right-open={rightOpen ? "true" : "false"}
        >
          <PanelRight className="icon-md" />
        </button>
      )}
    </div>
  );
}

// 单个非图片附件的硬上限。字节以 JSON number[] 走本地 WS（镜像 images
// 通道），30MB 是内存尖峰与本地传输时延的折中。
const MAX_DOC_MB = 30;
const MAX_DOC_BYTES = MAX_DOC_MB * 1024 * 1024;

// ── 欢迎页快捷入口 ──────────────────────────────────────────
/* 2026-10-27：输入框下方的四条「建议行」已移除（用户要求去掉），
   欢迎页只保留字标 + 能力 chip + 输入框。 */

interface ChatViewProps {
  session: Session | null;
  /** 历史翻页：hasMore 时列表顶部显示「加载更早的消息」，onLoadMore 前插一页。 */
  historyPaging?: { hasMore: boolean; loading: boolean; onLoadMore: () => void } | null;
  /** 返回 false = 没发出去（建会话失败 / 流式层错误）。调用方据此恢复草稿。 */
  onSendMessage: (content: string, projectId?: string, overrides?: ChatOverrides, attachments?: { images?: QueuedImage[]; attachedFilePaths?: string[]; attachedFiles?: QueuedFile[] }) => Promise<boolean>;
  onNewChat?: () => void;
  pendingPrefill?: string | null;
  onConsumePrefill?: () => void;
  todoGroups?: Array<{
    groupId: string;
    groupTitle: string | null;
    anchorMessageId: string | null;
    itemsById: Record<string, TodoSnapshotItem>;
  }>;
  subAgents?: Record<string, SubAgentState>;
  /** 成员工单实时状态（总管方案 P4.4），转发给 MessageList。 */
  memberTickets?: Record<string, MemberTicketState>;
  onTodoPatch?: (id: string, patch: { status?: string; title?: string; priority?: string }) => Promise<unknown>;
  justPatchedTodoRef?: React.MutableRefObject<Set<string>>;
  streamingText?: string;
  environmentLabel?: string;
  isSandbox?: boolean;
  branch?: string;
  usagePct?: number;
  quotaResetHint?: string;
  onUpgrade?: () => void;
  onOpenSessionSettings?: () => void;
  onShareSession?: () => void;
  onOpenExperts?: () => void;
  onOpenSettings?: (category?: ConfigCategory) => void;
  onOpenComputer?: () => void;
  onUndo?: () => void;
  onRestore?: () => void;
  onSwitchModel?: (newId: string) => void;
  workspaceRoot?: string | null;
  onWorkspaceChange?: (sessionId: string | null | undefined, path: string | null) => void;
  rightOpen?: boolean;
  onToggleRight?: () => void;
  sidebarCollapsed?: boolean;
  onExpandSidebar?: () => void;
  onOpenSearch?: () => void;
  onPathClick?: (path: string) => void;
  onApproveApproval?: (requestId: string, remember?: boolean) => void;
  onDenyApproval?: (requestId: string, note?: string) => void;
  onApproveBatchApproval?: (batchId: string, remember?: boolean) => void;
  onDenyBatchApproval?: (batchId: string, note?: string) => void;
  onAnswerClarify?: (requestId: string, answer: string) => void;
  onAcknowledgePlan?: (messageId: string) => void;
  messageLoadError?: string | null;
  onRetryLoad?: () => void;
  streamError?: { message: string; detail?: string; kind?: ErrorKind } | null;
  onClearStreamError?: (sid: string | null) => void;
  onRetryLastMessage?: () => void;
  onEnterWelcome?: () => void;
  acuiState?: AcuiState;
  acuiBus?: AcuiDecisionBus;
  acuiStats?: {
    total: number;
    resolved: number;
    pending: number;
    decisionCount: number;
  };
  prefetchIntent?: {
    intent: string;
    confidence: number;
    results: Array<{
      tool: string;
      args: unknown;
      output: string;
      errored: boolean;
      durationMs: number;
    }>;
    } | null;
  streamingPhase?: StreamPhase | null;
  pendingToolsCount?: number;
  messageQueue?: QueuedMessage[];
  onAddToQueue?: (msg: QueuedMessage) => void;
  onClearQueue?: () => void;
  onRemoveFromQueue?: (id: string) => void;
  onEditQueueItem?: (id: string, newText: string) => void;
  onSendFromQueue?: (id: string) => void;
  pendingFileAttachment?: string | null;
  onConsumeFileAttachment?: () => void;
}

export const ChatView = memo(function ChatView({
  session,
  historyPaging,
  onSendMessage,
  onNewChat,
  pendingPrefill,
  onConsumePrefill,
  streamingText: _legacyStreamingText,
  streamingPhase,
  onOpenExperts,
  onOpenSettings,
  onOpenComputer,
  onUndo,
  onRestore,
  onSwitchModel,
  workspaceRoot,
  onWorkspaceChange,
  rightOpen,
  onToggleRight,
  sidebarCollapsed,
  onExpandSidebar,
  onOpenSearch,
  todoGroups, onTodoPatch, justPatchedTodoRef,
  subAgents,
  memberTickets,
  onApproveApproval,
  onDenyApproval,
  onApproveBatchApproval,
  onDenyBatchApproval,
  onPathClick,
  onAnswerClarify,
  onAcknowledgePlan,
  messageLoadError,
  onRetryLoad,
  streamError,
  onClearStreamError,
  onRetryLastMessage,
  prefetchIntent,
  acuiState,
  acuiBus,
  messageQueue,
  onAddToQueue,
  onClearQueue,
  onRemoveFromQueue,
  onEditQueueItem,
  onSendFromQueue,
  pendingFileAttachment,
  onConsumeFileAttachment,
  pendingToolsCount = 0,
  onEnterWelcome,
}: ChatViewProps) {
  const stream = useStreamChatContext();
  const isStreaming = stream.streamingSessionId === session?.id;
  // 工作台会话（workbench_id 已盖戳）→ 欢迎页不渲染通用能力入口。
  const isWorkbenchChat = !!session?.workbench_id;
  // ── 输入草稿：按会话隔离 ──────────────────────────────────────
  // 原先是单个 useState，A 会话写一半切到 B，B 的输入框里是 A 的内容。
  // 现在 key 用 sessionId（欢迎态用 welcome），并防抖落 localStorage。
  const draftKey = session?.id ?? "welcome";
  const [drafts, setDrafts] = useState<Record<string, string>>(() => {
    try {
      const raw = storage.getItem("hermix.composer.drafts.v1");
      return raw ? (JSON.parse(raw) as Record<string, string>) : {};
    } catch {
      return {};
    }
  });
  const input = drafts[draftKey] ?? "";
  const setInput = useCallback((value: string | ((prev: string) => string)) => {
    setDrafts((prev) => {
      const current = prev[draftKey] ?? "";
      const next = typeof value === "function" ? value(current) : value;
      if (next === current) return prev;
      return { ...prev, [draftKey]: next };
    });
  }, [draftKey]);
  useEffect(() => {
    const t = setTimeout(() => {
      try {
        storage.setItem("hermix.composer.drafts.v1", JSON.stringify(drafts));
      } catch {
        /* 配额满 / 隐私模式：草稿只在内存里，不阻塞输入 */
      }
    }, 400);
    return () => clearTimeout(t);
  }, [drafts]);
  const { apps: customApps } = useCustomApps();
  const [sending, setSending] = useState(false);
  const [inlineCompactionDismissed, setInlineCompactionDismissed] = useState(false);
  useEffect(() => {
    const phase = stream.compactionStatus?.phase;
    if (phase === "starting" || phase === "in_progress_fast" || phase === "in_progress_llm") {
      setInlineCompactionDismissed(false);
    }
  }, [stream.compactionStatus?.phase]);
  const [forceScrollSeq, setForceScrollSeq] = useState(0);
  const messageListRef = useRef<MessageListHandle>(null);
  const [isAgentActiveState, setIsAgentActiveState] = useState(false);
  const [postStreamActivity, setPostStreamActivity] = useState(false);
  const prevStreamingRef = useRef(isStreaming);
  const { notify } = useDesktopNotify();
  useEffect(() => {
    const wasStreaming = prevStreamingRef.current;
    prevStreamingRef.current = isStreaming;
    if (wasStreaming && !isStreaming) {
      setPostStreamActivity(true);
      // L4：回复结束通知。notify 内部自己判"开关开 + 窗口无焦点"，
      // 所以这里无条件调——把条件写在钩子里，设置页和 ChatView 不会
      // 各判一套而出现"开关显示开、实际不弹"或反之。
      notify("Hermix Desktop", "回复已完成。");
    } else if (isStreaming) {
      setPostStreamActivity(false);
    }
  }, [isStreaming, notify]);

  useEffect(() => {
    if (!postStreamActivity) return;
    const t = setTimeout(() => setPostStreamActivity(false), 4000);
    return () => clearTimeout(t);
  }, [postStreamActivity, pendingToolsCount]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (!pendingPrefill) return;
    // 统一走注入总线：预填 / 引用 / 编辑重发 / 语音 / 润色 共用一条链路。
    injectIntoComposer({ source: "prefill", text: pendingPrefill, mode: "replace" });
    onConsumePrefill?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingPrefill]);

  // 注入总线订阅：replace 覆盖；append 追加；at-caret 在当前选区插入。
  useEffect(() => {
    return subscribeComposerInjection((req) => {
      const ta = textareaRef.current;
      const focusAt = (pos: number) => {
        if (req.focus === false) return;
        requestAnimationFrame(() => {
          const el = textareaRef.current;
          if (!el) return;
          el.focus();
          el.setSelectionRange(pos, pos);
        });
      };

      if (req.mode === "replace") {
        setInput(req.text);
        focusAt(req.text.length);
        return;
      }
      if (req.mode === "append") {
        const base = ta?.value ?? "";
        const sep = base && !base.endsWith("\n") ? "\n" : "";
        const next = `${base}${sep}${req.text}`;
        setInput(next);
        focusAt(next.length);
        return;
      }
      // at-caret：以 DOM 当前选区为准（React state 可能比用户点击慢一拍）。
      const value = ta?.value ?? "";
      const start = ta?.selectionStart ?? value.length;
      const end = ta?.selectionEnd ?? start;
      const next = value.slice(0, start) + req.text + value.slice(end);
      setInput(next);
      focusAt(start + req.text.length);
    });
  }, [setInput]);

  // 输入框高度自适应由 AppMentionInput 内部负责（textarea 撑满内容高度、
  // 外层容器滚动），这里不要再改 height —— 父组件的 useEffect 在子组件
  // layout effect 之后执行，会把高度覆盖回带上限的值。

  // ── Expert / permission / workspace / skills state ─────────────────
  const sessionId = session?.id ?? null;
  const { expert, setExpert } = useSessionExpert(sessionId);
  const { cutoffAt: expertCutoffAt, recordCutoff } = useExpertCutoff(sessionId);
  const setExpertWithCutoff = (id: string | null) => {
    setExpert(id);
    if (id) markRecentUsed(id);
    recordCutoff(id);
    if (sessionId) {
      updateSessionMetadata(sessionId, { expert_id: id }).catch((e) => { console.warn("[ChatView] sync expert_id failed:", e); });
    }
  };

  const [availableTools, setAvailableTools] = useState<ToolDescriptor[]>([]);
  useEffect(() => {
    let cancelled = false;
    const load = () => {
      getAvailableTools().then((tools) => {
        if (!cancelled) setAvailableTools(tools);
      });
    };
    // 首屏：等后端真正 ready 再取工具。启动竞态里本 effect 早于 Rust
    // init_global 触发，此刻 /v1/tools 直接吃 503 "backend offline" 且不自动
    // 重试，工具列表会一直空着。就绪后补发，一次成功、无 console 噪音。
    void (async () => {
      await waitForBackendReady();
      if (!cancelled) load();
    })();
    // window focus 走快路：用户主动切回窗口时后端通常已在位，直接取。
    window.addEventListener("focus", load);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", load);
    };
  }, []);
  const [permissionLevel, setPermissionLevel] = usePermissionLevel();
  const [domainMode, setDomainMode] = useDomainMode();
  const [sendKey] = useSendKey();
  const { workspace, setWorkspace: setWorkspaceRaw } = useSessionWorkspace(sessionId);
  const workspaceLocked =
    (session?.messages?.length ?? 0) > 0 || sending || isStreaming;

  const setWorkspace = useCallback(
    (path: string | null) => {
      if (workspaceLocked) return;
      setWorkspaceRaw(path);
      if (sessionId) {
        updateSessionMetadata(sessionId, { workspace: path }).catch((e) => { console.warn("[ChatView] sync workspace failed:", e); });
      }
      onWorkspaceChange?.(sessionId, path);
    },
    [setWorkspaceRaw, sessionId, workspaceLocked, onWorkspaceChange],
  );
  const lastIsWelcomeRef = useRef<boolean | null>(null);
  useEffect(() => {
    const isWelcome = !session || ((session.message_count ?? 0) === 0 && session.messages.length === 0);
    if (isWelcome && lastIsWelcomeRef.current !== true) {
      onEnterWelcome?.();
    }
    lastIsWelcomeRef.current = isWelcome;
  }, [session, onEnterWelcome]);
  const { skills: activeSkills } = useSessionSkills(sessionId);
  const { experts, categories, byCategory } = useExpertCatalog();
  const { recent: recentExperts, markUsed: markRecentUsed } = useRecentExperts();
  const { workspaces, add: addWorkspace, remove: removeWorkspace } = useWorkspaces();
  const activeWorkspace = useMemo(
    () => (workspace ? workspaces.find((w) => w.path === workspace) ?? null : null),
    [workspace, workspaces],
  );
  const reconciledRef = useRef<string | null>(null);
  useEffect(() => {
    if (!workspace) { reconciledRef.current = null; return; }
    if (workspaces.find((w) => w.path === workspace)) return;
    if (reconciledRef.current === workspace) return;
    reconciledRef.current = workspace;
    addWorkspace("", workspace);
  }, [workspace, workspaces, addWorkspace]);

  const { models, byProvider, selectedModel, setSelectedModel, currentDisplay, defaultModel } = useModelSelector();

  const setSelectedModelAndPersist = useCallback(
    (id: string) => {
      setSelectedModel(id);
      warmUpProvider(id).catch(() => {});
      if (sessionId) {
        updateSessionMetadata(sessionId, { model: id }).catch((e) => { console.warn("[ChatView] sync model failed:", e); });
      }
    },
    [setSelectedModel, sessionId],
  );

  const restoredRef = useRef<string | null>(null);
  useEffect(() => {
    if (!session?.id) { restoredRef.current = null; return; }
    if (restoredRef.current === session.id) return;
    restoredRef.current = session.id;

    if (session.workspace) {
      setWorkspaceRaw(session.workspace);
    }
    setExpert(session.expert_id ?? null);
    // 只在会话真的记了模型时才覆盖当前选择。新会话 session.model 为空：
    // 欢迎页里用户手动选的模型此刻还活在本组件 state 里，若在这里用
    // defaultModel 重置，发送第一条消息（建会话 → 换 session 对象）的
    // 瞬间选择就被打回后端默认 —— "选了 agens 又变回 minimax" 的根因。
    // 空模型的会话沿用当前选择，由 overrides.model 随首条消息下发给后端。
    if (session.model) {
      setSelectedModel(resolveSessionSelectedModel(session.model, defaultModel));
    }
  }, [session?.id, session?.workspace, session?.expert_id, session?.model, defaultModel, setWorkspaceRaw, setExpert, setSelectedModel]);

  // 新会话落地时把"用户此刻选的模型"写进会话元数据。
  // 不写的话，欢迎页的选择只活在本组件 state 里：一旦 ChatView 重挂载
  // （切换页面/会话对象重建），selectedModel 会按默认模型重新初始化，
  // 表现就是"选好的模型一进去又变回去了"。
  const persistedModelRef = useRef<string | null>(null);
  useEffect(() => {
    if (!session?.id || session.model || !selectedModel) return;
    if (persistedModelRef.current === session.id) return;
    persistedModelRef.current = session.id;
    updateSessionMetadata(session.id, { model: selectedModel }).catch((e) =>
      console.warn("[ChatView] persist model for new session failed:", e),
    );
  }, [session?.id, session?.model, selectedModel]);

  const [pendingImages, setPendingImages] = useState<PendingImage[]>([]);
  // 真实磁盘路径附件（工作台文件树右键"加入对话"）——与 pendingDocs
  // （File 句柄，发送时才读字节）分属两条通道。
  const [filePaths, setFilePaths] = useState<string[]>([]);
  const [pendingDocs, setPendingDocs] = useState<QueuedFile[]>([]);
  const [isProcessingFiles, setIsProcessingFiles] = useState(false);

  useEffect(() => {
    if (pendingFileAttachment) {
      setFilePaths((prev) => prev.includes(pendingFileAttachment) ? prev : [...prev, pendingFileAttachment]);
      onConsumeFileAttachment?.();
    }
  }, [pendingFileAttachment, onConsumeFileAttachment]);

  useEffect(() => {
    return () => {
      setPendingImages((prev) => {
        prev.forEach((img) => URL.revokeObjectURL(img.blobUrl));
        return [];
      });
    };
  }, []);

  const [keepOriginalImage, setKeepOriginalImage] = useState(false);

  // 思考开关（enableThinking）：按会话持久化。`true` = 显式开启；`false` = 显式关闭；
  // `undefined` = 跟随 provider 默认。切了随下一条消息的 overrides 下发（见上方 overrides 构造块）。
  // 键 = session id；切换会话时各自独立，不串。
  const [enableThinkingMap, setEnableThinkingMap] = useState<Record<string, boolean>>(() => {
    try {
      const raw = storage.getItem("hermix.enableThinkingMap");
      return raw ? (JSON.parse(raw) as Record<string, boolean>) : {};
    } catch {
      return {};
    }
  });
  const currentEnableThinking = sessionId ? enableThinkingMap[sessionId] : undefined;
  const handleEnableThinkingChange = useCallback((v: boolean) => {
    if (!sessionId) return;
    setEnableThinkingMap((prev) => {
      const next = { ...prev, [sessionId]: v };
      storage.setItem("hermix.enableThinkingMap", JSON.stringify(next));
      return next;
    });
  }, [sessionId]);

  // 上下文窗口批解析：对当前选中的模型问后端要真值（含来源分层），喂给模型信息卡。
  const contextWindows = useContextWindows(selectedModel ? [selectedModel] : []);

  // 当前模型是否支持 reasoning（思考开关键只在这时显）——从 /api/model/info 的能力位读。
  const [supportsReasoning, setSupportsReasoning] = useState<boolean | undefined>(undefined);
  useEffect(() => {
    if (!selectedModel) { setSupportsReasoning(undefined); return; }
    let cancelled = false;
    getModelInfo().then((info) => {
      if (!cancelled) setSupportsReasoning(info?.capabilities.supports_reasoning);
    }).catch(() => { if (!cancelled) setSupportsReasoning(undefined); });
    return () => { cancelled = true; };
  }, [selectedModel]);

  const handleFilesAttach = useCallback(
    async (files: File[]) => {
      setIsProcessingFiles(true);
      try {
        const images: File[] = [];
        const nonImages: File[] = [];
        for (const f of files) {
          if (f.type.startsWith("image/")) images.push(f);
          else nonImages.push(f);
        }

        if (images.length > 0) {
          const results: CompressResult[] = await Promise.all(
            images.map((f) => (keepOriginalImage ? skipCompression(f) : compressImage(f))),
          );
          const newImages: PendingImage[] = results.map((r) => ({
            file: r.file,
            blobUrl: URL.createObjectURL(r.file),
            sizeBytes: r.compressedSize,
            mimeType: r.file.type,
            isCompressed: r.isCompressed,
            method: r.method,
            name: r.file.name,
          }));
          setPendingImages((prev) => [...prev, ...newImages]);
        }

        if (nonImages.length > 0) {
          // WebView2（以及一切浏览器）的 File 对象没有磁盘路径——文档必须
          // 以字节走 wire（镜像 images 通道），服务端落盘后 agent 才能
          // read_file / read_pdf。File 句柄留到发送时才读，状态里零拷贝。
          const oversize = nonImages.filter((f) => f.size > MAX_DOC_BYTES);
          if (oversize.length > 0) {
            toastError(`文件过大（单个附件上限 ${MAX_DOC_MB}MB）：${oversize.map((f) => f.name).join("、")}`);
          }
          const docs: QueuedFile[] = nonImages
            .filter((f) => f.size <= MAX_DOC_BYTES)
            .map((f) => ({
              file: f,
              name: f.name,
              mimeType: f.type || "application/octet-stream",
              sizeBytes: f.size,
            }));
          if (docs.length > 0) {
            setPendingDocs((prev) => [...prev, ...docs]);
          }
        }
      } finally {
        setIsProcessingFiles(false);
      }
    },
    [keepOriginalImage],
  );

  const removePendingDoc = useCallback((index: number) => {
    setPendingDocs((prev) => prev.filter((_, i) => i !== index));
  }, []);

  const removePendingImage = useCallback((index: number) => {
    setPendingImages((prev) => {
      const target = prev[index];
      if (target) URL.revokeObjectURL(target.blobUrl);
      return prev.filter((_, i) => i !== index);
    });
  }, []);

  const clearPendingImages = useCallback(() => {
    setPendingImages([]);
  }, []);

  // Streaming row detection & message pipeline.
  const streamingEntry =
    isStreaming && session
      ? (() => {
          for (let i = session.messages.length - 1; i >= 0; i--) {
            const m = session.messages[i];
            if (m.role === "assistant") {
              return { id: m.id, msg: m };
            }
          }
          return null;
        })()
      : null;
  const streamingAssistantId = streamingEntry?.id ?? null;
  const streamingMsg = streamingEntry?.msg ?? null;

  // Fold state is still reported up from `<ProcessTimeline>`, but nothing
  // consumes it any more: the bottom-of-chat reasoning ticker was removed
  // (2026-10-01) because it duplicated the visible thinking block and
  // rendered as a clipped half-sentence. The callback stays wired so
  // `<ProcessTimeline>` still reports normally, and so the prop chain
  // through MessageList / MessageBubble / ToolCallBubble / TextBubble
  // (including MessageList's memo comparator) is left intact.
  const handleReasoningFoldChange = useCallback((_folded: boolean) => {
    // no-op sink — see above
  }, []);
  // 待审批清单（供输入框上方的审批抽屉使用）。
  // 两个来源：新流程的 unified_steps（App::onApprovalRequest 推入）与
  // 历史的 role:"approval" 独立消息；按 requestId/batchId 去重后按时间
  // 序排列 —— 抽屉永远先显示最老的一项，agent 正阻塞在那。
  const pendingApprovals = useMemo<PendingApprovals>(() => {
    const singles: ApprovalMessageMeta[] = [];
    const batches: ApprovalBatchMessageMeta[] = [];
    const seen = new Set<string>();
    for (const m of session?.messages ?? []) {
      if (m.role === "approval" && m.approval && !m.approval.decision
          && !seen.has(m.approval.requestId)) {
        seen.add(m.approval.requestId);
        singles.push(m.approval);
      }
      for (const st of m.unified_steps ?? []) {
        if (st.kind === "approval_request" && !st.meta.decision
            && !seen.has(st.meta.requestId)) {
          seen.add(st.meta.requestId);
          singles.push(st.meta);
        } else if (st.kind === "approval_request_batch" && !st.meta.decision
            && !seen.has(st.meta.batchId)) {
          seen.add(st.meta.batchId);
          batches.push(st.meta);
        }
      }
    }
    return { singles, batches };
  }, [session?.messages]);
  // 统一扫描（legacy role:"approval" + unified_steps 两条路径），
  // 供 keepOpen / activeTurnMsgIds / 审批抽屉共用同一份事实。
  const hasPendingApproval = useMemo(
    () => pendingApprovals.singles.length + pendingApprovals.batches.length > 0,
    [pendingApprovals],
  );
  const activeTurnMsgIds = useMemo(() => {
    const msgs = session?.messages ?? [];
    let lastUserIdx = -1;
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === "user") {
        lastUserIdx = i;
        break;
      }
    }
    if (lastUserIdx < 0) return new Set<string>();
    const agentActive =
      pendingToolsCount > 0 || hasPendingApproval || !!streamingAssistantId;
    if (!agentActive) return new Set<string>();
    const s = new Set<string>();
    for (let i = lastUserIdx; i < msgs.length; i++) s.add(msgs[i].id);
    return s;
  }, [session?.messages, pendingToolsCount, hasPendingApproval, streamingAssistantId]);

  useEffect(() => {
    const hasActiveSubAgent = Object.values(subAgents ?? {}).some(
      (sa) => sa.phase === "spawned" || sa.phase === "started",
    );
    setIsAgentActiveState(
      isStreaming ||
        pendingToolsCount > 0 ||
        hasPendingApproval ||
        postStreamActivity ||
        !!streamingAssistantId ||
        hasActiveSubAgent,
    );
  }, [isStreaming, pendingToolsCount, hasPendingApproval, postStreamActivity, streamingAssistantId, subAgents]);

  const messagesWithStream = useMemo<ChatMessage[]>(() => {
    const nonStreamingRaw = streamingAssistantId
      ? session?.messages.filter((m) => m.id !== streamingAssistantId) ?? []
      : session?.messages ?? [];
    // Defensive: `mergeLegacyToolMessages` /
    // `mergeConsecutiveAssistantMessages` walk every row in order and
    // crash on a single corrupt entry (e.g. a timeline item with a
    // `tool_call_id` of the wrong type). Without this try/catch the
    // crash bubbles into React's render phase, kills the whole
    // `MessageList` tree, and the user sees "only the last 2 approval
    // cards survived" because that's what was on-screen before the
    // crash. Wrap in try/catch and fall back to the raw messages so
    // the user always sees SOMETHING.
    let restMsgs: ChatMessage[];
    try {
      // Merge legacy role: "tool" messages into preceding assistant's
      // execution_timeline so they render inline instead of as separate rows.
      const mergedTools = mergeLegacyToolMessages(nonStreamingRaw);
      const visibleNonStreaming = mergedTools.filter(isRowRenderable);
      restMsgs = mergeConsecutiveAssistantMessages(visibleNonStreaming);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error(
        "[ChatView] chat pipeline crashed, falling back to raw messages:",
        err,
      );
      restMsgs = nonStreamingRaw.filter(isRowRenderable);
    }
    if (!streamingAssistantId || !streamingMsg) return restMsgs;
    const rawIdx = session?.messages.findIndex((m) => m.id === streamingAssistantId) ?? -1;
    if (rawIdx < 0) return [...restMsgs, streamingMsg];
    const restIds = new Set(restMsgs.map((m) => m.id));
    let insertIdx = 0;
    for (let i = 0; i < rawIdx; i++) {
      if (restIds.has(session!.messages[i].id)) insertIdx++;
    }
    const result = [...restMsgs];
    result.splice(insertIdx, 0, streamingMsg);
    return result;
  }, [session?.messages, streamingAssistantId, streamingMsg]);

  const streamRow = useMemo<StreamRow[]>(() => {
    const rows: StreamRow[] = messagesWithStream.map((msg) => ({ kind: "message", msg, role: msg.role }));

    const groups = todoGroups ?? [];
    if (groups.length === 0) return rows;

    const assistantIdxById = new Map<string, number>();
    messagesWithStream.forEach((m, i) => {
      if (m.role === "assistant") assistantIdxById.set(m.id, i);
    });

    type Pending = { group: NonNullable<typeof todoGroups>[number]; insertAfter: number };
    const anchored: Pending[] = [];
    const unanchored: NonNullable<typeof todoGroups>[number][] = [];

    for (const g of groups) {
      if (g.anchorMessageId && assistantIdxById.has(g.anchorMessageId)) {
        anchored.push({ group: g, insertAfter: assistantIdxById.get(g.anchorMessageId)! });
        continue;
      }
      const items = Object.values(g.itemsById);
      if (items.length === 0) {
        unanchored.push(g);
        continue;
      }
      let earliest = items[0].created_at;
      for (const it of items) {
        if (it.created_at < earliest) earliest = it.created_at;
      }
      let chosen = -1;
      for (let i = 0; i < messagesWithStream.length; i++) {
        const m = messagesWithStream[i];
        if (m.role !== "assistant") continue;
        if (m.timestamp <= earliest) chosen = i;
        else break;
      }
      if (chosen < 0) unanchored.push(g);
      else anchored.push({ group: g, insertAfter: chosen });
    }

    anchored.sort((a, b) => a.insertAfter - b.insertAfter);

    const out = rows.slice();
    for (let i = anchored.length - 1; i >= 0; i--) {
      const { group, insertAfter } = anchored[i];
      out.splice(insertAfter + 1, 0, { kind: "todo", group, role: undefined as undefined });
    }
    for (const g of unanchored) out.push({ kind: "todo", group: g, role: undefined as undefined });
    return out;
  }, [messagesWithStream, todoGroups]);

  const showHeaderFlags = useMemo(() => {
    return streamRow.map((_, idx) =>
      shouldShowAssistantHeader(idx, streamRow)
    );
  }, [streamRow]);

  // 防连按 Enter：同文本 500ms 内只受理一次（替代旧的"队列按文本去重"，
  // 后者会把合法的重复指令「继续」也吞掉）。
  const lastSubmitRef = useRef<{ text: string; at: number }>({ text: "", at: 0 });

  const handleSubmit = async () => {
    const trimmed = input.trim();
    // @app → 目标自定义应用头。
    let finalText = buildMentionPrefix(trimmed, customApps, MENTION_TRIGGER_APP, "自定义应用", "app_id");
    if (!trimmed && pendingImages.length === 0 && filePaths.length === 0 && pendingDocs.length === 0) return;
    if (isProcessingFiles) return;
    const now = Date.now();
    if (finalText === lastSubmitRef.current.text && now - lastSubmitRef.current.at < 500) return;
    lastSubmitRef.current = { text: finalText, at: now };
    const attachedImages: QueuedImage[] = pendingImages.map((img) => ({
      file: img.file,
      blobUrl: img.blobUrl,
      mimeType: img.mimeType,
      name: img.name,
    }));
    const attachedPaths: string[] = filePaths.length > 0 ? [...filePaths] : [];
    const attachments:
      | { images?: QueuedImage[]; attachedFilePaths?: string[]; attachedFiles?: QueuedFile[] }
      | undefined =
      attachedImages.length > 0 || attachedPaths.length > 0 || pendingDocs.length > 0
        ? {
            images: attachedImages.length > 0 ? attachedImages : undefined,
            attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
            attachedFiles: pendingDocs.length > 0 ? [...pendingDocs] : undefined,
          }
        : undefined;
    // 发送参数先算好：入队与直发必须用同一份（旧实现队列只带 model，
    // 权限/专家/领域/工具/技能静默丢失）。
    const overrides: ChatOverrides = { permissionLevel };
    if (domainMode === "code") overrides.domainOverride = "code";
    if (selectedModel && selectedModel.includes(":") && selectedModel !== "unknown") {
      overrides.model = selectedModel;
    }
    // 思考开关：仅当用户显式设过（map 里有本 session 的值）才下发，
    // 否则 undefined = 后端保持 provider 默认行为。
    if (currentEnableThinking !== undefined) overrides.enableThinking = currentEnableThinking;
    if (expert) {
      overrides.systemPromptOverride = expert.systemPrompt;
      markRecentUsed(expert.id);
    }
    const enabledToolNames = availableTools
      .filter((t) => t.isEnabled)
      .map((t) => t.name);
    if (enabledToolNames.length > 0) {
      overrides.tools = enabledToolNames;
    }
    // 专家 toolset（STABILITY_AND_REMOTE_PLAN §3.2-2）：作为独立字段传
    // 后端，由 registry 展开成组成员并进白名单——不再丢弃。
    if (expert?.toolset) overrides.expertToolset = expert.toolset;
    const fallbackWorkspace =
      (workspace && workspace.trim()) ||
      (session?.workspace && session.workspace.trim()) ||
      (() => {
        try {
          const raw = storage.getItem(
            sessionId
              ? `hermix.session.${sessionId}.workspace`
              : STORAGE_KEYS.draftWorkspace,
          );
          return raw && raw.trim() ? raw : null;
        } catch {
          return null;
        }
      })();
    if (fallbackWorkspace) overrides.workspace = fallbackWorkspace;
    if (activeSkills.length > 0) overrides.skills = activeSkills;

    // 失败恢复用的快照（清空之后还要把这些放回去）。
    // images 用 PendingImage（带压缩元数据），不能拿 QueuedImage 顶回去。
    const draftSnapshot = {
      text: trimmed,
      images: [...pendingImages],
      paths: attachedPaths,
      docs: [...pendingDocs],
    };

    setForceScrollSeq((s) => s + 1);
    messageListRef.current?.scrollToBottom(true);
    onClearStreamError?.(sessionId);
    setInput("");
    clearPendingImages();
    setFilePaths([]);
    setPendingDocs([]);

    if (isStreaming || sending) {
      onAddToQueue?.({
        id: `q-${now}-${Math.random().toString(36).slice(2, 6)}`,
        sessionId,
        text: finalText,
        images: attachedImages.length > 0 ? attachedImages : undefined,
        attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
        attachedFiles: pendingDocs.length > 0 ? [...pendingDocs] : undefined,
        overrides,
        status: "queued",
        attempts: 0,
        createdAt: new Date().toISOString(),
      });
      toastInfo("已加入待发队列");
      return;
    }

    setSending(true);
    try {
      const ok = await onSendMessage(finalText, undefined, overrides, attachments);
      if (!ok) {
        // 没发出去：把原文与附件放回输入区。若用户已经开始输入新内容，
        // 追加而不是覆盖 —— 不能把刚打的新字也一起冲掉。
        setInput((prev) => (prev.trim() ? `${prev}\n${draftSnapshot.text}` : draftSnapshot.text));
        if (draftSnapshot.images.length > 0) {
          setPendingImages((prev) => [...draftSnapshot.images, ...prev]);
        }
        if (draftSnapshot.paths.length > 0) {
          setFilePaths((prev) => Array.from(new Set([...draftSnapshot.paths, ...prev])));
        }
        if (draftSnapshot.docs.length > 0) {
          setPendingDocs((prev) => [...draftSnapshot.docs, ...prev]);
        }
      }
    } finally {
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // L3.8：发送键可配（设置 › 通用 › 发送快捷键）。两种模式都保留
    // Shift+Enter 换行——那是多行输入的通用习惯，不该被模式抢走。
    const ctrlEnter = sendKey === "ctrl_enter";
    const isSend = e.key === "Enter" && !e.shiftKey && (ctrlEnter ? e.ctrlKey || e.metaKey : true);
    if (isSend) {
      e.preventDefault();
      handleSubmit();
    }
  };

  if (messageLoadError) {
    return (
      <div
        className="chat-surface flex-1 flex flex-col px-8 min-h-0"
        style={{ overflow: "visible" }}
      >
        {sidebarCollapsed && onExpandSidebar && (
          <SidebarRevealRow onExpand={onExpandSidebar} onOpenSearch={onOpenSearch} />
        )}
        {/* 工具行必须贴顶 —— 居中只作用于下面的内容区，否则行被 justify-center 拉下去 */}
        <div className="flex-1 min-h-0 flex flex-col items-center justify-center">
        <div className="w-full max-w-md flex flex-col items-center py-12 text-center">
          <div className="tint-warning mb-5 flex h-14 w-14 items-center justify-center rounded-full">
            <AlertTriangle className="icon-xl text-[var(--color-warning)]" />
          </div>
          <h2 className="text-lg font-semibold text-text-primary mb-2">
            加载对话失败
          </h2>
          <p className="text-sm text-text-secondary mb-1">
            后端暂时无法返回这条对话的记录。常见原因：服务还在启动、
            网络瞬断、或数据库暂时不可用。
          </p>
          <p
            className="text-xs text-text-tertiary mb-6 font-mono break-all max-w-sm"
            title={messageLoadError}
          >
            {messageLoadError}
          </p>
          <div className="flex items-center gap-2">
            <button
              onClick={() => onRetryLoad?.()}
              className="btn btn-primary"
              type="button"
            >
              <ArrowUp className="icon-md" />
              <span>重试</span>
            </button>
            <button
              onClick={onNewChat}
              className="btn btn-secondary"
              type="button"
            >
              <span>{NEW_CHAT}</span>
            </button>
          </div>
        </div>
        </div>
      </div>
    );
  }

  const isLoadingExisting = session
    && session.messages.length === 0
    && ((session as Session & { message_count?: number }).message_count ?? 0) > 0;
  if (isLoadingExisting) {
    return (
      <div className="chat-surface flex-1 flex flex-col px-8 min-h-0">
        {sidebarCollapsed && onExpandSidebar && (
          <SidebarRevealRow onExpand={onExpandSidebar} onOpenSearch={onOpenSearch} />
        )}
        <div className="flex-1 min-h-0 flex flex-col items-center justify-center">
        <div className="flex flex-col items-center gap-3 text-center">
          <div
            className="w-9 h-9 rounded-full border-2 border-border-primary border-t-text-muted animate-spin"
            aria-hidden="true"
          />
          <p className="text-[length:var(--text-ui)] text-text-muted">正在恢复对话…</p>
        </div>
        </div>
      </div>
    );
  }

  if (!session || session.messages.length === 0) {
    return (
      <div className="chat-surface flex-1 flex flex-col px-8 min-h-0" style={{ overflow: "visible" }}>
        {sidebarCollapsed && onExpandSidebar && (
          <SidebarRevealRow onExpand={onExpandSidebar} onOpenSearch={onOpenSearch} />
        )}
        {/* 工具行必须贴顶 —— 居中只作用于下面的内容区，否则行被 justify-center 拉下去 */}
        <div className="flex-1 min-h-0 flex flex-col items-center justify-center">
        <div className="w-full max-w-3xl flex flex-col items-center py-12">
          {!isWorkbenchChat && (
            /* 欢迎页 = 夜航面板（2026-10-27 A 方向）。
               锚点是一句承诺 + 吉祥物插画，能力入口是可读的场景句
               （点之前就知道会发生什么），不再是同款灰药丸塞隐藏 prompt。 */
            <WelcomeNight
              onPickScene={(prompt) => {
                setInput(prompt);
                textareaRef.current?.focus();
              }}
            />
          )}
          <div className="w-full relative">
            <Composer
              variant={isWorkbenchChat ? "workbench" : "default"}
              input={input} setInput={setInput} handleKeyDown={handleKeyDown} handleSubmit={handleSubmit} customApps={customApps}
              textareaRef={textareaRef} sending={sending} isStreaming={false}
              activeWorkspace={activeWorkspace} workspaces={workspaces} workspaceLocked={workspaceLocked}
              onSelectWorkspace={(p) => setWorkspace(p)}
              onAddWorkspace={async () => {
                const path = await pickDirectory();
                if (path) {
                  const ws = addWorkspace("", path);
                  setWorkspace(ws.path);
                }
              }}
              onRemoveWorkspace={removeWorkspace}
              models={models} byProvider={byProvider} selectedModel={selectedModel} setSelectedModel={setSelectedModelAndPersist}
              onSwitchModel={onSwitchModel}
              currentDisplay={currentDisplay}
              defaultModel={defaultModel}
              contextWindows={contextWindows}
              supportsReasoning={supportsReasoning}
              enableThinking={currentEnableThinking}
              onEnableThinkingChange={handleEnableThinkingChange}
              expert={expert} setExpert={setExpertWithCutoff}
              recentExperts={recentExperts}
              experts={experts} categories={categories} byCategory={byCategory}
              onPickRecent={(id) => { markRecentUsed(id); setExpertWithCutoff(id); }}
              permissionLevel={permissionLevel} setPermissionLevel={setPermissionLevel}
              domainMode={domainMode} setDomainMode={setDomainMode}
              onOpenExperts={onOpenExperts}
              onOpenSettings={onOpenSettings}
              onOpenComputer={onOpenComputer}
              onUndo={onUndo}
              onRestore={onRestore}
              onStop={stream.cancel}
              activeComposerSessionId={session?.id}
              pendingImages={pendingImages}
              pendingDocs={pendingDocs}
              onRemoveDoc={removePendingDoc}
              isProcessingFiles={isProcessingFiles}
              onRemoveImage={removePendingImage}
              onFilesAttach={handleFilesAttach}
              keepOriginalImage={keepOriginalImage}
              setKeepOriginalImage={setKeepOriginalImage}
              placeholder="输入消息…"
              session={session}
            />
            {streamError && (
              <StreamErrorCard
                message={streamError.message}
                detail={streamError.detail}
                kind={streamError.kind}
                onDismiss={() => onClearStreamError?.(sessionId)}
                onRetry={onRetryLastMessage}
              />
            )}
          </div>
        </div>
        </div>
      </div>
    );
  }

  return (
    <div className="chat-surface flex-1 flex flex-col overflow-hidden min-h-0">
      {/* 工作台对话不渲染状态条：浮窗标题栏已经顶着工作台名，这里再出一行
          产物栏图标就是重复动作，且产物栏在工作台语境下无意义。
          会话标题本身在 2026-10-04 已上提到主窗口标题栏中栏。 */}
      {!isWorkbenchChat && (
        <ModelStatusBar
          imSource={imSourceOf(session?.id)}
          rightOpen={rightOpen}
          onToggleRight={onToggleRight}
          sidebarCollapsed={sidebarCollapsed}
          onExpandSidebar={onExpandSidebar}
          onOpenSearch={onOpenSearch}
        />
      )}

      {/* 外层 padding 抄消息列滚动容器的 `px-3 sm:px-4`，内层再套
          `max-w-reading mx-auto` —— 这样状态行和 `.msg-stream-list`
          落在同一个盒子里，左边缘与正文一致（直接铺在 `.chat-surface`
          上时它是居中窄条，和正文差一截）。 */}

      <MessageList
        ref={messageListRef}
        session={session}
        historyPaging={historyPaging}
        stream={stream}
        isStreaming={isStreaming}
        isAgentActiveState={isAgentActiveState}
        forceScrollSeq={forceScrollSeq}
        setForceScrollSeq={setForceScrollSeq}
        postStreamActivity={postStreamActivity}
        pendingToolsCount={pendingToolsCount}
        streamingAssistantId={streamingAssistantId}
        streamingMsg={streamingMsg}
        streamingPhase={streamingPhase}
        hasPendingApproval={hasPendingApproval}
        activeTurnMsgIds={activeTurnMsgIds}
        streamRow={streamRow}
        showHeaderFlags={showHeaderFlags}
        subAgents={subAgents}
        memberTickets={memberTickets}
        prefetchIntent={prefetchIntent}
        inlineCompactionDismissed={inlineCompactionDismissed}
        setInlineCompactionDismissed={setInlineCompactionDismissed}
        expertCutoffAt={expertCutoffAt}
        onTodoPatch={onTodoPatch}
        justPatchedTodoRef={justPatchedTodoRef}
        onPathClick={onPathClick}
        onApproveApproval={onApproveApproval}
        onDenyApproval={onDenyApproval}
        onApproveBatchApproval={onApproveBatchApproval}
        onDenyBatchApproval={onDenyBatchApproval}
        onAnswerClarify={onAnswerClarify}
        onAcknowledgePlan={onAcknowledgePlan}
        workspaceRoot={workspaceRoot}
        sending={sending}
        onReasoningFoldChange={handleReasoningFoldChange}
      />

      <div className="px-3 sm:px-4 pb-4 shrink-0">
        <div className="max-w-reading mx-auto">
          {acuiState && acuiState.cards.length > 0 && (
            <AcuiPanel
              state={acuiState}
              decide={acuiBus}
              // ACUI 面板（右侧工具时间线）与聊天区共享同一条规则：
              // 只要文本流或任一工具仍在运行，未完成组就保持 "运行中"，
              // 避免长工具在 LLM 输出流结束后被误判为 "执行已中断"。
              streamComplete={!isStreaming && pendingToolsCount === 0}
            />
          )}

          {streamError && (
            <StreamErrorCard
              message={streamError.message}
              detail={streamError.detail}
              kind={streamError.kind}
              onDismiss={() => onClearStreamError?.(sessionId)}
              onRetry={onRetryLastMessage}
            />
          )}

          {/* Outer banner: breaker-state summary (idle/closed/half_open/open/fallback/pressure + complete/failed).
              In-flight phases (starting/in_progress_fast/in_progress_llm) are rendered as an inline card
              inside MessageList so the user sees exactly one "正在准备/正在压缩中" indicator at a time. */}
          {stream.compactionStatus &&
            !["starting", "in_progress_fast", "in_progress_llm"].includes(
              stream.compactionStatus.phase ?? "idle",
            ) && (
              <CompactionStatusBanner
                status={stream.compactionStatus}
                onDismiss={() => stream.setCompactionStatus(null)}
              />
            )}

          {/* Token 消耗明细入口：图标悬停/聚焦弹出输入/输出/缓存/上下文卡片，
              替代原先只塞进 title 的「缓存 N%」。上下文 % 仍在行内保留。 */}
          <TokenUsagePopover
            sessionId={sessionId}
            contextUsage={stream.contextUsage}
            isStreaming={isStreaming}
          />

          <div className="w-full relative">
            <ApprovalDrawer
              pending={pendingApprovals}
              onApprove={(id, remember) => onApproveApproval?.(id, remember)}
              onDeny={(id, note) => onDenyApproval?.(id, note)}
              onApproveBatch={(id, remember) => onApproveBatchApproval?.(id, remember)}
              onDenyBatch={(id, note) => onDenyBatchApproval?.(id, note)}
              onFullAccess={() => setPermissionLevel("full")}
            />
          <Composer
            variant={isWorkbenchChat ? "workbench" : "default"}
            input={input} setInput={setInput} handleKeyDown={handleKeyDown} handleSubmit={handleSubmit}
            textareaRef={textareaRef} sending={sending} isStreaming={isStreaming}
            activeWorkspace={activeWorkspace} workspaces={workspaces} workspaceLocked={workspaceLocked}
            onSelectWorkspace={(p) => setWorkspace(p)}
            onAddWorkspace={async () => {
              const path = await pickDirectory();
              if (path) {
                const ws = addWorkspace("", path);
                setWorkspace(ws.path);
              }
            }}
            onRemoveWorkspace={removeWorkspace}
            models={models} byProvider={byProvider} selectedModel={selectedModel} setSelectedModel={setSelectedModelAndPersist}
            onSwitchModel={onSwitchModel}
            currentDisplay={currentDisplay}
            defaultModel={defaultModel}
            contextWindows={contextWindows}
            supportsReasoning={supportsReasoning}
            enableThinking={currentEnableThinking}
            onEnableThinkingChange={handleEnableThinkingChange}
            expert={expert} setExpert={setExpertWithCutoff}
            recentExperts={recentExperts}
            experts={experts} categories={categories} byCategory={byCategory}
            onPickRecent={(id) => { markRecentUsed(id); setExpertWithCutoff(id); }}
            permissionLevel={permissionLevel} setPermissionLevel={setPermissionLevel}
            domainMode={domainMode} setDomainMode={setDomainMode}
            onOpenExperts={onOpenExperts}
            onOpenSettings={onOpenSettings}
            onOpenComputer={onOpenComputer}
            onUndo={onUndo}
            onRestore={onRestore}
            onStop={stream.cancel}
            activeComposerSessionId={session?.id}
            pendingImages={pendingImages}
            pendingDocs={pendingDocs}
            onRemoveDoc={removePendingDoc}
            isProcessingFiles={isProcessingFiles}
            onRemoveImage={removePendingImage}
            onFilesAttach={handleFilesAttach}
            keepOriginalImage={keepOriginalImage}
            setKeepOriginalImage={setKeepOriginalImage}
            placeholder="输入消息…"
            session={session}
            messageQueue={messageQueue}
            onAddToQueue={onAddToQueue}
            onClearQueue={onClearQueue}
            onRemoveFromQueue={onRemoveFromQueue}
            onEditQueueItem={onEditQueueItem}
            onSendFromQueue={onSendFromQueue}
          />
          </div>
        </div>
      </div>
    </div>
  );
});
