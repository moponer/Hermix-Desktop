import { useState, useRef, useEffect, useMemo, useCallback, memo } from "react";
import { useStreamChatContext } from "../context/StreamChatContext";
import { IM_SOURCE_LABEL, imSourceOf } from "../utils/time";
import { PanelRight, PanelLeftOpen, Search, ArrowUp, AlertTriangle, AppWindow, Workflow, Code2, FileText, PenLine, Image as ImageIcon, Briefcase } from "lucide-react";
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
import { useContextWindows } from "../hooks/useContextWindows";
import { toastError } from "../hooks/useToast";
import { useChannelsStatus } from "../hooks/useChannelsStatus";
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
  /** IM 远程会话的来源平台（QQ / 企业微信）—— 从那边发起的对话在头部标出来。 */
  imSource?: "qq" | "wecom" | null;
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
    : "这条对话是从 IM 发起并远程驱动的";
  const showRightToggle = !!onToggleRight && !rightOpen;
  if (!showLeftActions && !imLabel && !showRightToggle) return null;
  // h-10：与 .sidebar-head、右栏标签条同高（三段头部是一条 40px 水平带，
  // 各列不再各飞各的基线）。
  return (
    <div
      className="h-10 shrink-0 flex items-center gap-2 px-4 sm:px-6 rounded-t-xl"
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
      {imLabel && (
        <span
          className="shrink-0 inline-flex items-center gap-1.5 rounded-full bg-bg-tertiary px-2 h-[20px] text-[length:var(--text-tiny)] text-text-secondary"
          title={imTitle}
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${imChannel?.connected ? "bg-success" : "bg-error"}`}
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

// ── 欢迎页欢迎语 ─────────────────────────────────────────────
// 纯内置哲理短句，按时间段分桶随机展示；无网络请求、无副标题。
// 每次进入欢迎页（sessionId 变化 / 新建对话）重抽一条。
const WELCOME_BY_HOUR: Array<[number, string[]]> = [
  [
    5,
    [
      "夜是白昼的谜底。",
      "安静，是思想的声音。",
      "星星不问路，也各有方向。",
      "黑暗里走的路，天亮后叫经历。",
      "凌晨的灯，照着不肯将就的人。",
      "夜深了，灵感才刚醒。",
      "睡去的人做梦，醒来的人造梦。",
      "夜色收走喧嚣，只留一句实话。",
    ],
  ],
  [
    11,
    [
      "清晨的好处，是一切还没定型。",
      "太阳每天都是新的，你也是。",
      "早起的路最空，先走的人最静。",
      "醒来，是一天里最公平的时刻。",
      "把今天当第一页，不是续集。",
      "露水不等谁，却落进每片叶子。",
      "一天的方向，早晨就知道了。",
      "风从窗外来，答案在心里走。",
    ],
  ],
  [
    14,
    [
      "走得再快的人，也要停下来吃饭。",
      "正午没有影子，正好看清自己。",
      "忙是生命的密度，闲是它的留白。",
      "歇一歇，路又不会跑。",
      "阳光正好，把心摊开晒一晒。",
      "短暂停下，是为了走得更直。",
      "午间的安静，是一天的深呼吸。",
      "一顿好饭，是给下午的敬意。",
    ],
  ],
  [
    18,
    [
      "坚持不是用力，是不停。",
      "慢一点没关系，别停就好。",
      "大事，都是小事做完的样子。",
      "影子长了，说明你还在光里。",
      "路是走出来的，答案是问出来的。",
      "下午的光斜了，心就定了。",
      "时间不催人，只是陪着走。",
      "下午，适合解决上午想不通的事。",
    ],
  ],
  [
    23,
    [
      "夜色温柔，适合原谅与放下。",
      "复盘，是白天的回声。",
      "晚灯下的人，离自己最近。",
      "结束，也是一种完成。",
      "夜里适合读书，也适合读自己。",
      "今天没做完的，明天接着走。",
      "把今天交给回忆，明天交给梦。",
      "一天所获，不在多，在真。",
    ],
  ],
];

function pickWelcomeGreeting(): string {
  const h = new Date().getHours();
  const pool = (WELCOME_BY_HOUR.find(([max]) => h < max) ?? WELCOME_BY_HOUR[0])[1];
  return pool[Math.floor(Math.random() * pool.length)];
}

/** 建议行（墨与纸 v4）：四条快捷入口，一键填入输入框。
 *  内容必须对应 Hermix 真实能力——差异审查 / 文档写作 / 定时自动化 /
 *  文件落盘，不放假能力。 */
const WELCOME_SUGGESTIONS: string[] = [
  "审查当前工作区的未提交改动，按风险从高到低给我一份清单",
  "把这段会议记录整理成一页纸纪要，行动项单独列出",
  "每天早上 9 点汇总昨天未读的消息和待办，生成一份日报",
  "为当前项目写一份新人上手指南，保存到 docs/ 目录",
];

/** 能力 chip 行（墨与纸 v4）：对应真实能力，点击填入能力专属 prompt 模板。
 *  独立应用=对话式创建桌面应用；工作流=自动化工作流；代码开发=差异审查；
 *  PDF转换=文档解析；文档编写=纪要整理；图片处理=图像任务。 */
const WELCOME_CAPABILITIES: Array<{ label: string; icon: typeof AppWindow; prompt: string }> = [
  { label: "独立应用", icon: AppWindow, prompt: "帮我做一个桌面小应用：" },
  { label: "工作流", icon: Workflow, prompt: "帮我搭一个自动化工作流，触发条件和步骤是：" },
  { label: "代码开发", icon: Code2, prompt: "审查当前工作区的未提交改动，按风险从高到低给我一份清单" },
  { label: "PDF 转换", icon: FileText, prompt: "把这份 PDF 里的表格和数据提取出来，整理成结构化表格" },
  { label: "文档编写", icon: PenLine, prompt: "把以下内容整理成一页纸纪要：" },
  { label: "图片处理", icon: ImageIcon, prompt: "帮我处理这张图片，要求是：" },
];

/** R3-8（2026-10-16）：欢迎语按会话 id 稳定取值——同一会话永远同一句，
 *  切走再切回不换句（此前每次进欢迎页都随机重抽，上下文记忆感全无）。 */
function pickStableGreeting(sessionId: string): string {
  let h = 0;
  for (let i = 0; i < sessionId.length; i++) h = (h * 31 + sessionId.charCodeAt(i)) | 0;
  const hour = new Date().getHours();
  const pool = (WELCOME_BY_HOUR.find(([max]) => hour < max) ?? WELCOME_BY_HOUR[0])[1];
  return pool[Math.abs(h) % pool.length];
}

function useWelcomeGreeting(sessionId: string | null): string {
  const [title, setTitle] = useState(() =>
    sessionId ? pickStableGreeting(sessionId) : pickWelcomeGreeting(),
  );
  useEffect(() => {
    setTitle(sessionId ? pickStableGreeting(sessionId) : pickWelcomeGreeting());
  }, [sessionId]);
  return title;
}

interface ChatViewProps {
  session: Session | null;
  /** 历史翻页：hasMore 时列表顶部显示「加载更早的消息」，onLoadMore 前插一页。 */
  historyPaging?: { hasMore: boolean; loading: boolean; onLoadMore: () => void } | null;
  onSendMessage: (content: string, projectId?: string, overrides?: ChatOverrides, attachments?: { images?: QueuedImage[]; attachedFilePaths?: string[]; attachedFiles?: QueuedFile[] }) => Promise<void>;
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
  // 工作台会话（workbench_id 已盖戳）→ 右栏差异化文案：欢迎页、placeholder
  // 都说"这台工作台"的事，而不是复读主对话的通用问候（方案 A）。
  const isWorkbenchChat = !!session?.workbench_id;
  const welcomeGreeting = useWelcomeGreeting(session?.id ?? null);
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

  const handleSubmit = async () => {
    const trimmed = input.trim();
    // @app → 目标自定义应用头。
    let finalText = buildMentionPrefix(trimmed, customApps, MENTION_TRIGGER_APP, "自定义应用", "app_id");
    if (!trimmed && pendingImages.length === 0 && filePaths.length === 0 && pendingDocs.length === 0) return;
    if (isProcessingFiles) return;
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
    setForceScrollSeq((s) => s + 1);
    messageListRef.current?.scrollToBottom(true);
    onClearStreamError?.(sessionId);
    setInput("");
    clearPendingImages();
    setFilePaths([]);
    setPendingDocs([]);
    if (isStreaming || sending) {
      onAddToQueue?.({
        text: finalText,
        images: attachedImages.length > 0 ? attachedImages : undefined,
        attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
        attachedFiles: pendingDocs.length > 0 ? [...pendingDocs] : undefined,
      });
      return;
    }
    setSending(true);
    try {
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
      await onSendMessage(finalText, undefined, overrides, attachments);
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
          <p className="text-[length:var(--text-ui)] text-text-muted">正在恢复对话...</p>
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
              {/* 墨与纸 v4：吉祥物移除、留空 —— hero 标题独当焦点，空白即大气 */}
              <h1 className="text-hero font-semibold text-[var(--color-text-hero)] mb-5">{welcomeGreeting}</h1>
              {/* 场景切换：独立药丸，接真实机制 domainMode（办公=自动识别 / 开发=钉代码域） */}
              <div className="flex items-center gap-2.5 mb-5">
                <button
                  type="button"
                  onClick={() => setDomainMode("auto")}
                  className={`flex items-center gap-1.5 h-7 px-4 rounded-full text-[length:var(--text-ui)] transition-colors ${
                    domainMode !== "code"
                      ? "bg-[var(--color-bg-card)] text-[var(--color-text-primary)] font-medium shadow-[var(--shadow-card)]"
                      : "bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)]"
                  }`}
                >
                  <Briefcase className="icon-sm" /> 办公
                </button>
                <button
                  type="button"
                  onClick={() => setDomainMode("code")}
                  className={`flex items-center gap-1.5 h-7 px-4 rounded-full text-[length:var(--text-ui)] transition-colors ${
                    domainMode === "code"
                      ? "bg-[var(--color-bg-card)] text-[var(--color-text-primary)] font-medium shadow-[var(--shadow-card)]"
                      : "bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)]"
                  }`}
                >
                  <Code2 className="icon-sm" /> 开发
                </button>
              </div>
              {/* 能力 chip 行：点击加载能力专属 prompt 模板 */}
              <div className="flex flex-wrap justify-center gap-2 mb-5">
                {WELCOME_CAPABILITIES.map((c) => (
                  <button
                    key={c.label}
                    type="button"
                    onClick={() => {
                      setInput(c.prompt);
                      textareaRef.current?.focus();
                    }}
                    className="flex items-center gap-1.5 h-7 px-3.5 rounded-full border border-[var(--color-border-secondary)] bg-[var(--color-bg-tertiary)] text-[length:var(--text-ui)] text-[var(--color-text-secondary)] transition-colors hover:text-[var(--color-text-primary)] hover:bg-[var(--color-bg-card-hover)]"
                    title={`填入「${c.label}」能力模板`}
                  >
                    <c.icon className="icon-sm" /> {c.label}
                  </button>
                ))}
              </div>
            </>
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
          {/* 建议行（墨与纸 v4，对齐 WorkBuddy 快捷入口）：左对齐自适应宽度
              圆角矩形（非药丸、不居中），↗ 表示"填入输入框"。内容全部对应
              Hermix 真实能力：差异审查 / 文档写作 / 定时自动化 / 文件落盘。 */}
          {!isWorkbenchChat && (
            <div className="w-full max-w-3xl flex flex-col items-start gap-1.5 mt-3">
              {WELCOME_SUGGESTIONS.map((s) => (
                <button
                  key={s}
                  type="button"
                  onClick={() => {
                    setInput(s);
                    textareaRef.current?.focus();
                  }}
                  className="group flex w-auto max-w-full items-center gap-6 rounded-[var(--radius-lg)] bg-[var(--color-bg-card)] px-4 py-2.5 text-left transition-colors hover:bg-[var(--color-bg-card-hover)]"
                >
                  <span className="text-[length:var(--text-ui)] text-[var(--color-text-secondary)] group-hover:text-[var(--color-text-primary)]">
                    {s}
                  </span>
                  <svg
                    className="icon-sm shrink-0 text-[var(--color-text-faint)] group-hover:text-[var(--color-text-secondary)]"
                    viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"
                  >
                    <path d="M7 17 17 7M8 7h9v9" />
                  </svg>
                </button>
              ))}
            </div>
          )}
          {/* 免责微文案（信任感细节；字号取仓库下限 12px 而非参考的 10-11px） */}
          <p className="text-center text-[length:var(--text-tiny)] text-[var(--color-text-faint)] mt-8">
            内容由 AI 生成，请核实重要信息
          </p>
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
                    className={`inline-flex items-center gap-1.5 tabular-nums text-[length:var(--text-meta)] ${textColor} cursor-default`}
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
              <span className="text-[length:var(--text-meta)] text-text-faint">—</span>
            </div>
          )}

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
    </div>
  );
});
