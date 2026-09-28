import { useState, useRef, useEffect, useMemo, useCallback, memo } from "react";
import { useStreamChatContext } from "../context/StreamChatContext";
import { PanelRight, PanelLeftOpen, Search, ArrowUp, AlertTriangle } from "lucide-react";
import { SidebarRevealRow } from "./SidebarRevealRow";
import { resolveSessionSelectedModel } from "../utils/sessionModel";
import type {
  ApprovalBatchMessageMeta,
  ApprovalMessageMeta,
  ChatMessage,
  QueuedMessage,
  QueuedImage,
  SubAgentState,
  Session,
} from "../types";
import type { ChatOverrides, StreamPhase, TodoSnapshotItem } from "../hooks/useStreamChat";
import type { ConfigCategory } from "./SystemConfigPanel";
import { CompactionStatusBanner } from "./CompactionStatusBanner";
import { mergeConsecutiveAssistantMessages, isRowRenderable, shouldShowAssistantHeader, mergeLegacyToolMessages } from "./chatUtils";
import type { PendingImage } from "./chatUtils";
import type { AcuiState } from "../acui/types";
import type { AcuiDecisionBus } from "../acui/AcuiRegistry";
import { AcuiPanel } from "../acui/AcuiPanel";
import { StreamErrorCard, type ErrorKind } from "./StreamErrorBanner";
import { useModelSelector } from "./ModelSelector";
import { Composer } from "./Composer";
import { ApprovalDrawer, type PendingApprovals } from "./ApprovalDrawer";
import { MessageList, type MessageListHandle, type StreamRow } from "./MessageList";
import { useCustomApps } from "../hooks/useCustomApps";
import {
  MENTION_TRIGGER_APP,
  buildMentionPrefix,
} from "../lib/appMention";
import {
  usePermissionLevel,
  useSessionExpert,
  useExpertCatalog,
  useRecentExperts,
  useSessionWorkspace,
  useSessionSkills,
} from "../hooks/useExpert";
import { useExpertCutoff } from "../hooks/useExpertCutoff";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { pickDirectory } from "../desktopBridge";
import { getAvailableTools, updateSessionMetadata, warmUpProvider, type ToolDescriptor } from "../api";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { compressImage, skipCompression, type CompressResult } from "../utils/imageCompressor";

// ── ModelStatusBar — top-of-chat status strip ─────────
function ModelStatusBar({
  title,
  rightOpen,
  onToggleRight,
  sidebarCollapsed,
  onExpandSidebar,
  onOpenSearch,
}: {
  title?: string;
  rightOpen?: boolean;
  onToggleRight?: () => void;
  sidebarCollapsed?: boolean;
  onExpandSidebar?: () => void;
  onOpenSearch?: () => void;
}) {
  // 侧栏折叠时，展开 + 搜索按钮直接嵌在本行标题左侧（用户拍板：与对话标题
  // 同一行，不另起浮层）。折叠但会话尚无标题（title 未生成）时也必须渲染
  // 本行 —— 否则展开入口随侧栏一起消失，用户再也打不开侧栏。
  const showLeftActions = !!sidebarCollapsed && !!onExpandSidebar;
  if (!title && !showLeftActions) return null;
  const displayTitle = title && title.length > 80 ? title.slice(0, 77) + "…" : title;
  const showRightToggle = !!onToggleRight;
  return (
    <div
      className="px-4 sm:px-6 pt-3 pb-2 flex items-center gap-2 rounded-t-xl shrink-0"
      style={{ color: "var(--color-text-primary)" }}
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
      {displayTitle && (
        <span
          className="truncate flex-1 min-w-0"
          style={{
            fontSize: "14px",
            fontWeight: 600,
            letterSpacing: "-0.005em",
          }}
        >
          {displayTitle}
        </span>
      )}
      {showRightToggle && (
        <button
          onClick={onToggleRight}
          className={`titlebar-btn${displayTitle ? "" : " ml-auto"}`}
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

interface ChatViewProps {
  session: Session | null;
  /** 历史翻页：hasMore 时列表顶部显示「加载更早的消息」，onLoadMore 前插一页。 */
  historyPaging?: { hasMore: boolean; loading: boolean; onLoadMore: () => void } | null;
  onSendMessage: (content: string, projectId?: string, overrides?: ChatOverrides, attachments?: { images?: QueuedImage[]; attachedFilePaths?: string[] }) => Promise<void>;
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
  onDenyApproval?: (requestId: string) => void;
  onApproveBatchApproval?: (batchId: string, remember?: boolean) => void;
  onDenyBatchApproval?: (batchId: string) => void;
  onAnswerClarify?: (requestId: string, answer: string) => void;
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
  onRemoveFromQueue?: (idx: number) => void;
  onEditQueueItem?: (idx: number, newText: string) => void;
  onSendFromQueue?: (idx: number) => void;
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
  onApproveApproval,
  onDenyApproval,
  onApproveBatchApproval,
  onDenyBatchApproval,
  onPathClick,
  onAnswerClarify,
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
  // 工作台会话（workbench_id 已盖戳）→ 右栏差异化文案：欢迎页、placeholder
  // 都说"这台工作台"的事，而不是复读主对话的通用问候（方案 A）。
  const isWorkbenchChat = !!session?.workbench_id;
  const [input, setInput] = useState("");
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
  useEffect(() => {
    const wasStreaming = prevStreamingRef.current;
    prevStreamingRef.current = isStreaming;
    if (wasStreaming && !isStreaming) {
      setPostStreamActivity(true);
    } else if (isStreaming) {
      setPostStreamActivity(false);
    }
  }, [isStreaming]);

  useEffect(() => {
    if (!postStreamActivity) return;
    const t = setTimeout(() => setPostStreamActivity(false), 4000);
    return () => clearTimeout(t);
  }, [postStreamActivity, pendingToolsCount]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (!pendingPrefill) return;
    setInput(pendingPrefill);
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) {
        ta.focus();
        const len = ta.value.length;
        ta.setSelectionRange(len, len);
      }
    });
    onConsumePrefill?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingPrefill]);

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
    load();
    window.addEventListener("focus", load);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", load);
    };
  }, []);
  const [permissionLevel, setPermissionLevel] = usePermissionLevel();
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

  const [pendingImages, setPendingImages] = useState<PendingImage[]>([]);
  const [filePaths, setFilePaths] = useState<string[]>([]);
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
          const paths: string[] = [];
          for (const f of nonImages) {
            const filePath = (f as File & { path?: string }).path;
            if (filePath && typeof filePath === "string") {
              paths.push(filePath);
            }
          }
          if (paths.length > 0) {
            setFilePaths((prev) => [...prev, ...paths]);
          }
        }
      } finally {
        setIsProcessingFiles(false);
      }
    },
    [keepOriginalImage],
  );

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

  // Reasoning lines extracted from the active (streaming) assistant's
  // `unified_steps` for the bottom-of-chat `<ReasoningTicker>`. The
  // ticker only shows when the user has folded the reasoning block
  // (`reasoningFolded`); otherwise the same content is visible inline
  // inside the expanded fold. Source = the live `streamingMsg` so
  // the ticker reflects the active turn, not historical folds.
  const reasoningLines = useMemo<string[]>(() => {
    if (!streamingMsg) return [];
    const steps = streamingMsg.unified_steps;
    if (!steps) return [];
    return steps
      .filter((s): s is { kind: "reasoning"; content: string } =>
        s.kind === "reasoning")
      .map((s) => s.content);
  }, [streamingMsg]);
  // Fold state mirrored up from the latest `<ProcessTimeline>`.
  // Starts `false` (2026-08-29 user feedback): during a live stream the
  // thinking block is always expanded inline, so the ticker must stay
  // dormant — reasoning is readable right there, a rolling duplicate is
  // pure noise. Only a REAL `folded=true` report (user manually closed a
  // fold) may mount the ticker. Multi-assistant sessions report from
  // each ProcessTimeline in render order — the streaming assistant's
  // update always wins last.
  const [reasoningFolded, setReasoningFolded] = useState(false);
  const handleReasoningFoldChange = useCallback((folded: boolean) => {
    setReasoningFolded(folded);
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

  const handleSubmit = async () => {
    const trimmed = input.trim();
    // @app → 目标自定义应用头。
    let finalText = buildMentionPrefix(trimmed, customApps, MENTION_TRIGGER_APP, "自定义应用", "app_id");
    if (!trimmed && pendingImages.length === 0 && filePaths.length === 0) return;
    if (isProcessingFiles) return;
    const attachedImages: QueuedImage[] = pendingImages.map((img) => ({
      file: img.file,
      blobUrl: img.blobUrl,
      mimeType: img.mimeType,
      name: img.name,
    }));
    const attachedPaths: string[] = filePaths.length > 0 ? [...filePaths] : [];
    const attachments: { images?: QueuedImage[]; attachedFilePaths?: string[] } | undefined =
      attachedImages.length > 0 || attachedPaths.length > 0
        ? {
            images: attachedImages.length > 0 ? attachedImages : undefined,
            attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
          }
        : undefined;
    setForceScrollSeq((s) => s + 1);
    messageListRef.current?.scrollToBottom(true);
    onClearStreamError?.(sessionId);
    setInput("");
    clearPendingImages();
    setFilePaths([]);
    if (isStreaming || sending) {
      onAddToQueue?.({
        text: finalText,
        images: attachedImages.length > 0 ? attachedImages : undefined,
        attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
      });
      return;
    }
    setSending(true);
    try {
      const overrides: ChatOverrides = { permissionLevel };
      if (selectedModel && selectedModel.includes(":") && selectedModel !== "unknown") {
        overrides.model = selectedModel;
      }
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
      void expert?.toolset;
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
      await onSendMessage(finalText, undefined, overrides, attachments);
    } finally {
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
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
          <div
            className="w-14 h-14 rounded-full flex items-center justify-center mb-5"
            style={{
              background: "var(--color-warning-bg, rgba(245, 158, 11, 0.12))",
              color: "var(--color-warning, #d97706)",
            }}
          >
            <AlertTriangle size={26} />
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
              <span>新建对话</span>
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
          <p className="text-[13px] text-text-muted">正在恢复对话...</p>
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
          {isWorkbenchChat ? (
            <>
              <h1 className="text-xl font-semibold text-text-primary mb-2 tracking-tight">这台工作台由你说了算</h1>
              <p className="text-sm text-text-secondary text-center mb-6 max-w-xs leading-relaxed">
                要加什么功能、改哪里、录什么数据，直接说。改完自动应用到画布。
              </p>
            </>
          ) : (
            <>
              <div className="relative mb-4">
                <img
                  src="/mascot-256.png"
                  alt="Hermix吉祥物"
                  className="w-[140px] h-[140px] object-contain select-none"
                  draggable={false}
                />
              </div>
              <h1 className="text-2xl font-bold text-text-primary mb-2 tracking-tight">你好！我是 Hermix</h1>
              <p className="text-sm text-text-secondary text-center mb-6 max-w-xs leading-relaxed">
                一个智能 AI 助手。告诉我你的目标，我来帮你拆解并完成。
              </p>
            </>
          )}
          <div className="w-full relative">
            <ApprovalDrawer
              pending={pendingApprovals}
              onApprove={(id, remember) => onApproveApproval?.(id, remember)}
              onDeny={(id) => onDenyApproval?.(id)}
              onApproveBatch={(id, remember) => onApproveBatchApproval?.(id, remember)}
              onDenyBatch={(id) => onDenyBatchApproval?.(id)}
            />
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
              expert={expert} setExpert={setExpertWithCutoff}
              recentExperts={recentExperts}
              experts={experts} categories={categories} byCategory={byCategory}
              onPickRecent={(id) => { markRecentUsed(id); setExpertWithCutoff(id); }}
              permissionLevel={permissionLevel} setPermissionLevel={setPermissionLevel}
              onOpenExperts={onOpenExperts}
              onOpenSettings={onOpenSettings}
              onOpenComputer={onOpenComputer}
              onUndo={onUndo}
              onRestore={onRestore}
              onStop={stream.cancel}
              activeComposerSessionId={session?.id}
              pendingImages={pendingImages}
              isProcessingFiles={isProcessingFiles}
              onRemoveImage={removePendingImage}
              onFilesAttach={handleFilesAttach}
              keepOriginalImage={keepOriginalImage}
              setKeepOriginalImage={setKeepOriginalImage}
              placeholder={isWorkbenchChat ? "告诉这台工作台要加什么、改什么、查什么..." : "Hermix帮你做事 — 写作、规划、分析、学习、开发..."}
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
          会话标题 + 产物栏图标就是双层标题，且产物栏在工作台语境下无意义。 */}
      {!isWorkbenchChat && (
        <ModelStatusBar
          title={session?.title}
          rightOpen={rightOpen}
          onToggleRight={onToggleRight}
          sidebarCollapsed={sidebarCollapsed}
          onExpandSidebar={onExpandSidebar}
          onOpenSearch={onOpenSearch}
        />
      )}

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
        workspaceRoot={workspaceRoot}
        sending={sending}
        reasoningLines={reasoningLines}
        reasoningFolded={reasoningFolded}
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

          {stream.contextUsage && stream.contextUsage.maxTokens > 0 ? (
            <div className="mb-1.5 flex justify-end px-1">
              {(() => {
                const fmt = (n: number) => n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1_000 ? `${Math.round(n / 1_000)}k` : String(n);
                const pct = Math.min(100, (stream.contextUsage.usedTokens / stream.contextUsage.maxTokens) * 100);
                const dotColor = pct >= 95 ? "bg-error" : pct >= 85 ? "bg-warning" : "bg-success";
                const textColor = pct >= 95 ? "text-error" : pct >= 85 ? "text-warning" : "text-text-secondary";
                let cacheInfo = "";
                if (stream.usageSnapshot) {
                  const cacheRead = stream.usageSnapshot.cacheReadTokens;
                  const uncached = stream.usageSnapshot.promptTokens;
                  const total = cacheRead + uncached;
                  if (total > 0) {
                    const rate = Math.round((cacheRead / total) * 100);
                    cacheInfo = ` | 缓存 ${rate}%`;
                  }
                }
                return (
                  <span
                    className={`inline-flex items-center gap-1.5 tabular-nums text-[11px] ${textColor} cursor-default`}
                    title={`${fmt(stream.contextUsage.usedTokens)} / ${fmt(stream.contextUsage.maxTokens)} tokens (${Math.round(pct)}%)${cacheInfo}`}
                  >
                    <span className={`w-1.5 h-1.5 rounded-full ${dotColor}`} />
                    {Math.round(pct)}%
                  </span>
                );
              })()}
            </div>
          ) : !isStreaming && (
            <div className="mb-1.5 flex justify-end px-1">
              <span className="text-[11px] text-text-faint">—</span>
            </div>
          )}

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
            expert={expert} setExpert={setExpertWithCutoff}
            recentExperts={recentExperts}
            experts={experts} categories={categories} byCategory={byCategory}
            onPickRecent={(id) => { markRecentUsed(id); setExpertWithCutoff(id); }}
            permissionLevel={permissionLevel} setPermissionLevel={setPermissionLevel}
            onOpenExperts={onOpenExperts}
            onOpenSettings={onOpenSettings}
            onOpenComputer={onOpenComputer}
            onUndo={onUndo}
            onRestore={onRestore}
            onStop={stream.cancel}
            activeComposerSessionId={session?.id}
            pendingImages={pendingImages}
            isProcessingFiles={isProcessingFiles}
            onRemoveImage={removePendingImage}
            onFilesAttach={handleFilesAttach}
            keepOriginalImage={keepOriginalImage}
            setKeepOriginalImage={setKeepOriginalImage}
            placeholder={isWorkbenchChat ? "告诉这台工作台要加什么、改什么..." : "输入消息..."}
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
  );
});
