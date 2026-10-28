import { useState, useRef, useEffect, useMemo } from "react";
import { Send, Square, FolderOpen, ChevronDown, Sparkles, ShieldCheck, Image, X, Check, AlertTriangle, Lock, Clock, Code2, FileText, Settings } from "lucide-react";
import type { DomainModeChoice } from "../hooks/useExpert";
import { usePopover } from "../hooks/usePopover";
import { PopoverShell, PopoverBody } from "./PopoverShell";
import { InlineExpertPanel } from "./InlineExpertPanel";
import { QueueItemRow } from "./ChatIndicators";
import { VoiceButton } from "./VoiceButton";
import { VisionButton } from "./VisionButton";
import { EnhanceButton } from "./EnhanceButton";
import { ProviderIcon } from "@hermix/preview/ProviderIcon";
import { Badge, Switch } from "./ui";
import {
  formatContextTokens,
  type ModelChoice,
  type ModelGroup,
} from "./ModelSelector";
import type { ConfigCategory } from "./SettingsDialog";
import { injectIntoComposer } from "../lib/composerInjection";
import { AppMentionInput } from "./AppMentionInput";
import { formatBytes } from "../utils/imageCompressor";
import { PERMISSION_LEVELS, type PermissionLevel } from "../hooks/useExpert";
import type { ContextWindowEntry } from "../hooks/useContextWindows";
import type { Workspace } from "../hooks/useWorkspaces";
import type { ExpertDef } from "../data/builtinExperts";
import type { QueuedMessage, QueuedFile, Session } from "../types";
import type { CustomAppSummary } from "../lib/customAppApi";
import type { PendingImage } from "./chatUtils";

export interface ComposerProps {
  input: string;
  setInput: (v: string) => void;
  handleKeyDown: (e: React.KeyboardEvent) => void;
  handleSubmit: () => void;
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  sending: boolean;
  isStreaming: boolean;
  activeWorkspace: Workspace | null;
  workspaceLocked?: boolean;
  workspaces: Workspace[];
  onSelectWorkspace: (path: string | null) => void;
  onAddWorkspace: () => Promise<void>;
  onRemoveWorkspace: (id: string) => void;
  models: ModelChoice[];
  selectedModel: string;
  setSelectedModel: (id: string) => void;
  currentDisplay: string;
  byProvider: ModelGroup[];
  /** 后端权威的默认模型（`provider:model`）。弹层里给这一行挂「默认」标，
   *  否则用户只能从设置页的星标反推「新会话 / 阿黎 / 自动化用的是哪个」。 */
  defaultModel?: string;
  expert: ExpertDef | null;
  setExpert: (id: string | null) => void;
  recentExperts: ExpertDef[];
  experts: ExpertDef[];
  categories: import("../data/builtinExperts").ExpertCategory[];
  byCategory: Record<string, ExpertDef[]>;
  onOpenExperts?: () => void;
  onOpenSettings?: (category?: ConfigCategory) => void;
  onOpenComputer?: () => void;
  onPickRecent?: (id: string) => void;
  onUndo?: () => void;
  onRestore?: () => void;
  onFilesAttach?: (files: File[]) => void;
  pendingImages?: PendingImage[];
  pendingDocs?: QueuedFile[];
  onRemoveDoc?: (index: number) => void;
  isProcessingFiles?: boolean;
  onRemoveImage?: (index: number) => void;
  keepOriginalImage?: boolean;
  setKeepOriginalImage?: (v: boolean) => void;
  /** workbench = 工作台右栏：隐藏专家/目录等主对话专属 chip（工作台有自己的 skill 与代码目录，选这些是噪音）。 */
  variant?: "default" | "workbench";
  onStop?: (sessionId?: string) => boolean;
  /** Identifier of the session this Composer is rendered for. Threaded
   *  into the stop-button click so the stream-chat hook can verify
   *  that "stop" actually targets THIS conversation (defends against
   *  the legacy "clicking stop on chat B kills chat A" bug). */
  activeComposerSessionId?: string;
  onSwitchModel?: (newId: string) => void;
  permissionLevel: PermissionLevel;
  setPermissionLevel: (lvl: PermissionLevel) => void;
  /** 工作模式（auto=自动识别）vs 编码模式（code=pin 代码开发域）。 */
  domainMode: DomainModeChoice;
  setDomainMode: (m: DomainModeChoice) => void;
  placeholder: string;
  messageQueue?: QueuedMessage[];
  onAddToQueue?: (msg: QueuedMessage) => void;
  onClearQueue?: () => void;
  onRemoveFromQueue?: (id: string) => void;
  onEditQueueItem?: (id: string, newText: string) => void;
  onSendFromQueue?: (id: string) => void;
  session?: Session | null;
  customApps?: CustomAppSummary[];
  /** 模型"上下文窗口"栏的数据源（后端 resolve-context-windows 的批解析结果）。 */
  contextWindows?: Record<string, ContextWindowEntry>;
  /** 当前模型的思考/reasoning 能力位（后端 get_model_info 的 supports_reasoning）。
   *  undefined = 未知（旧版后端 / 模型未收录），卡片只读不显开关。 */
  supportsReasoning?: boolean;
  /** 当前会话的"思考开/关"状态。`true` = 本轮显式开启；`false` = 显式关闭；
   *  `undefined` = 跟随 provider 默认。切换后随下一条消息的 overrides 下发。 */
  enableThinking?: boolean;
  onEnableThinkingChange?: (v: boolean) => void;
}

export function Composer(p: ComposerProps) {
  const [projOpen, setProjOpen, projContainerRef, projTriggerRef, projToggle] = usePopover();
  const [draggingFiles, setDraggingFiles] = useState(false);
  const dragCounter = useRef(0);
  const [modelOpen, setModelOpen, modelContainerRef, modelTriggerRef, modelToggle] = usePopover();
  const [expertPanelOpen, setExpertPanelOpen] = useState(false);
  // 待发队列默认折叠；但第一次入队时自动展开一次 —— 否则用户只看到
  // "待发送 1 条"，看不到自己刚排进去的内容。
  const prevQueueLenRef = useRef(0);
  useEffect(() => {
    const len = p.messageQueue?.length ?? 0;
    if (prevQueueLenRef.current === 0 && len > 0) setQueueOpen(true);
    prevQueueLenRef.current = len;
  }, [p.messageQueue]);
  const [queueOpen, setQueueOpen] = useState(false);
  const [permOpen, setPermOpen, permContainerRef, permTriggerRef, permToggle] = usePopover();

  const toolbarRef = useRef<HTMLDivElement>(null);
  const [toolbarCompact, setToolbarCompact] = useState(false);
  useEffect(() => {
    const el = toolbarRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const calc = () => {
      setToolbarCompact(el.clientWidth < 700);
    };
    calc();
    const ro = new ResizeObserver(calc);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const recentTurns = useMemo(() => {
    const msgs = p.session?.messages ?? [];
    const turns = msgs
      .filter((m) => m.role === "user" || m.role === "assistant")
      .slice(-2)
      .map((m) => `${m.role === "user" ? "用户" : "助理"}：${m.content.slice(0, 200)}`)
      .join("\n");
    return turns || undefined;
  }, [p.session?.messages]);

  const hasAttachments =
    (p.pendingImages?.length ?? 0) > 0 || (p.pendingDocs?.length ?? 0) > 0;

  return (
    <div
      className="composer transition-colors relative"
      style={{ overflow: "visible" }}
      onMouseDown={(e) => {
        // 视觉上的"整块面"就是输入区：点空白处也要进输入态。此前只有顶部
        // 那行透明 textarea 能拿焦点，框内大片留白点了没反应。
        const t = e.target as HTMLElement;
        if (t.closest("button, a, input, textarea, select, label, [role='menu'], [role='listbox'], [data-composer-panel]")) return;
        e.preventDefault();
        p.textareaRef.current?.focus();
      }}
      onDragEnter={(e) => {
        if (!e.dataTransfer?.types?.includes("Files")) return;
        dragCounter.current += 1;
        setDraggingFiles(true);
      }}
      onDragOver={(e) => {
        if (e.dataTransfer?.types?.includes("Files")) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
        }
      }}
      onDragLeave={() => {
        dragCounter.current = Math.max(0, dragCounter.current - 1);
        if (dragCounter.current === 0) setDraggingFiles(false);
      }}
      onDrop={(e) => {
        dragCounter.current = 0;
        setDraggingFiles(false);
        const files = Array.from(e.dataTransfer?.files ?? []);
        if (files.length === 0) return;
        e.preventDefault();
        p.onFilesAttach?.(files);
      }}
    >
      {draggingFiles && (
        <div
          className="drop-zone absolute inset-0 z-10 rounded-xl flex items-center justify-center pointer-events-none"
        >
          <div className="text-ui text-[color:var(--color-chip-workspace-fg)] font-medium bg-bg-elevated/90 px-3 py-1.5 rounded-lg">
            松开以添加文件
          </div>
        </div>
      )}

      {p.pendingImages && p.pendingImages.length > 0 && (
        <div className="px-3 pt-3 pb-1">
          <div className="flex items-center gap-2 overflow-x-auto">
            {p.pendingImages.map((img, idx) => (
              <div
                key={`${img.blobUrl}-${idx}`}
                className="relative w-14 h-14 rounded-lg overflow-hidden flex items-center justify-center shrink-0 bg-bg-tertiary"
                title={img.name}
              >
                <img
                  src={img.blobUrl}
                  alt={img.name}
                  className="max-w-full max-h-full object-contain"
                />
                {img.isCompressed && (
                  <span className="absolute bottom-0 left-0 right-0 text-tiny leading-tight bg-black/65 text-white text-center">
                    {formatBytes(img.sizeBytes)}
                  </span>
                )}
                <button
                  type="button"
                  onClick={() => p.onRemoveImage?.(idx)}
                  className="absolute top-0.5 right-0.5 grid h-6 w-6 place-items-center rounded-md text-text-muted transition-colors hover:text-text-error"
                  title="移除附件"
                  aria-label="移除附件"
                >
                  {/* 点击区 24×24（h-6 w-6），视觉仍是 14px 小方块 —— 贴角
                      摆放靠内层绝对定位，不放大视觉体积。 */}
                  <span className="absolute top-0.5 right-0.5 grid h-3.5 w-3.5 place-items-center rounded-md bg-bg-elevated/90">
                    <X className="icon-xs" />
                  </span>
                </button>
              </div>
            ))}
          </div>
          <div className="flex items-center gap-2 mt-1.5">
            <span className="h-tiny text-text-muted flex items-center gap-1">
              <Image className="icon-xs" />
              已附加 {p.pendingImages.length} 个附件
            </span>
            <label className="flex items-center gap-1 h-tiny text-text-faint cursor-pointer">
              <input
                type="checkbox"
                checked={!!p.keepOriginalImage}
                onChange={(e) => p.setKeepOriginalImage?.(e.target.checked)}
                className="w-3 h-3"
              />
              <span>保持原画质</span>
            </label>
          </div>
        </div>
      )}

      {p.pendingDocs && p.pendingDocs.length > 0 && (
        <div className="px-3 pt-3 pb-1">
          <div className="flex items-center gap-2 overflow-x-auto">
            {p.pendingDocs.map((doc, idx) => (
              <div
                key={`${doc.name}-${idx}`}
                className="relative flex items-center gap-2 max-w-[220px] h-14 px-2 rounded-lg overflow-hidden shrink-0 bg-bg-tertiary"
                title={doc.name}
              >
                <FileText className="icon-lg shrink-0 text-text-muted" />
                <div className="flex-1 min-w-0">
                  <div className="text-meta text-text-primary truncate leading-tight">{doc.name}</div>
                  <div className="text-tiny text-text-faint">{formatBytes(doc.sizeBytes)}</div>
                </div>
                <button
                  type="button"
                  onClick={() => p.onRemoveDoc?.(idx)}
                  className="absolute top-0.5 right-0.5 grid h-6 w-6 place-items-center rounded-md text-text-muted transition-colors hover:text-text-error"
                  title="移除附件"
                  aria-label="移除附件"
                >
                  {/* 点击区 24×24（h-6 w-6），视觉仍是 14px 小方块。 */}
                  <span className="absolute top-0.5 right-0.5 grid h-3.5 w-3.5 place-items-center rounded-md bg-bg-elevated/90">
                    <X className="icon-xs" />
                  </span>
                </button>
              </div>
            ))}
          </div>
          <div className="flex items-center gap-2 mt-1.5">
            <span className="h-tiny text-text-muted flex items-center gap-1">
              <FileText className="icon-xs" />
              已附加 {p.pendingDocs.length} 个文档
            </span>
          </div>
        </div>
      )}

      {p.messageQueue && p.messageQueue.length > 0 && (() => {
        const failedCount = p.messageQueue!.filter((m) => m.status === "failed").length;
        return (
          <div className="px-4 pt-2 pb-1.5">
            {/* 默认折叠成一行：队列常驻展开会和输入区争夺垂直空间。 */}
            <div className="flex items-center justify-between">
              <button
                type="button"
                onClick={() => setQueueOpen((v) => !v)}
                aria-expanded={queueOpen}
                className="flex h-tiny items-center gap-1.5 text-text-muted transition-colors hover:text-text-primary"
              >
                <ChevronDown className={`icon-xs transition-transform ${queueOpen ? "" : "-rotate-90"}`} />
                {p.messageQueue!.length} 条排队中
                {failedCount > 0 && (
                  <span className="text-[var(--color-error)]">· {failedCount} 条失败</span>
                )}
              </button>
              <div className="flex items-center gap-0.5">
                {p.onSendFromQueue && (
                  <button
                    type="button"
                    onClick={() => p.onSendFromQueue?.(p.messageQueue![0]!.id)}
                    className="grid size-6 place-items-center rounded text-text-muted transition-colors hover:bg-bg-tertiary hover:text-text-primary"
                    title="立即发送队首消息"
                    aria-label="立即发送队首消息"
                  >
                    <Send className="icon-sm" />
                  </button>
                )}
                <button
                  onClick={() => p.onClearQueue?.()}
                  className="h-tiny rounded px-1.5 py-0.5 text-text-muted transition-colors hover:bg-bg-tertiary hover:text-text-primary"
                >
                  清空
                </button>
              </div>
            </div>
            {queueOpen && (
              <ul className="mt-2 max-h-48 space-y-0.5 overflow-y-auto">
                {p.messageQueue!.map((msg, idx) => (
                  <QueueItemRow
                    key={msg.id}
                    index={idx}
                    total={p.messageQueue!.length}
                    text={msg.text}
                    status={msg.status}
                    error={msg.error}
                    canEdit={!!p.onEditQueueItem}
                    canRemove={!!p.onRemoveFromQueue}
                    canSend={!!p.onSendFromQueue}
                    onEdit={(t) => p.onEditQueueItem?.(msg.id, t)}
                    onRemove={() => p.onRemoveFromQueue?.(msg.id)}
                    onSend={() => p.onSendFromQueue?.(msg.id)}
                  />
                ))}
              </ul>
            )}
          </div>
        );
      })()}

      {expertPanelOpen && (
        <InlineExpertPanel
          currentExpertId={p.expert?.id ?? null}
          onSelect={(id) => {
            p.setExpert(id);
            setExpertPanelOpen(false);
          }}
          onClose={() => setExpertPanelOpen(false)}
          onOpenFullCatalog={p.onOpenExperts}
        />
      )}

      <AppMentionInput
        value={p.input}
        onChange={p.setInput}
        onKeyDown={p.handleKeyDown}
        textareaRef={p.textareaRef}
        apps={p.customApps ?? []}
        isStreaming={p.isStreaming}
        placeholder={p.placeholder}
        onFilesAttach={p.onFilesAttach}
      />
      {/* 工具行与底栏同一行：两块都挂在 .composer-footer 里，靠 CSS
          排成一行（工具行 flex-1、底栏 shrink-0）。此前是两个独立 flex 行，
          在 620px 宽里挤成两排，看上去像"换行"。 */}
      <div className="composer-footer">
      <div
        ref={toolbarRef}
        className="composer-toolrail px-3 pt-2 pb-2 flex flex-wrap items-center gap-x-1 gap-y-1"
        data-compact={toolbarCompact ? "true" : "false"}
      >
        {/* 工作台右栏（variant="workbench"）没有专家/目录概念 —— skill 常驻、
            代码目录固定，这两个 chip 是主对话专属，渲染出来只会误导。 */}
        {p.variant !== "workbench" && (
        <>
        <div className="flex items-center gap-0.5 min-w-0 shrink-0">
          <div className="relative">
            <button
              onClick={() => setExpertPanelOpen((v) => !v)}
              className={`flex items-center shrink-0 text-ui transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[140px]"
              } ${
                p.expert
                  ? "text-[color:var(--color-chip-model-fg)] hover:bg-[color:var(--color-chip-model-bg)] font-medium"
                  : expertPanelOpen
                  ? "bg-bg-hover text-text-primary"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
              }`}
              title={p.expert ? `已雇佣：${p.expert.name}` : "选择专家"}
            >
              <Sparkles className={`icon-sm ${p.expert ? "text-[color:var(--color-chip-model-fg)]" : "text-text-muted"}`} />
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.expert ? p.expert.name : "专家"}
              </span>
              {p.expert && (
                <span
                  role="button"
                  onClick={(e) => { e.stopPropagation(); p.setExpert(null); }}
                  className="ml-0.5 hover:opacity-70 shrink-0 text-text-muted"
                >
                  <X className="icon-sm" />
                </span>
              )}
            </button>
          </div>


          {/* 工作 / 编码模式切换 —— 直接点按切换（两档无需弹层）。
              code = pin 代码开发域（DomainMode::Code）；auto = 交给后端自动识别。 */}
          <button
            onClick={() => p.setDomainMode(p.domainMode === "code" ? "auto" : "code")}
            className={`flex items-center shrink-0 text-ui transition-colors rounded-md ${
              toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1"
            } ${
              p.domainMode === "code"
                ? "text-text-primary font-medium hover:bg-bg-hover"
                : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
            }`}
            title="固定代码开发域"
          >
            <Code2 className={`icon-sm ${p.domainMode === "code" ? "text-text-primary" : "text-text-muted"}`} />
            <span className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}>
              {p.domainMode === "code" ? "编码模式" : "自动"}
            </span>
          </button>
        </div>
        </>
        )}

        <div className="flex items-center gap-0.5 ml-auto shrink-0">
          <div className="relative">
            <button
              ref={modelTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={(p.byProvider && p.byProvider.length > 0) ? modelToggle : () => { p.onOpenSettings?.("model"); setModelOpen(false); }}
              className={`flex items-center gap-1.5 px-2 py-1 text-ui transition-colors rounded-md max-w-narrow ${
                (p.byProvider && p.byProvider.length > 0)
                  ? "text-text-secondary hover:bg-bg-hover hover:text-text-primary cursor-pointer"
                  : "text-[color:var(--color-chip-permission-fg)] hover:bg-error-soft cursor-pointer"
              }`}
              title={
                p.byProvider && p.byProvider.length > 0
                  ? p.defaultModel
                    ? `切换模型 · 默认 ${p.models.find((m) => m.id === p.defaultModel)?.name ?? p.defaultModel}`
                    : "切换模型"
                  : "未配置模型，点击直达 设置 → 模型配置"
              }
            >
              <ProviderIcon providerId={p.selectedModel?.split(":")[0] || "custom"} className="icon-md" title={p.selectedModel} />
              <span className="truncate">{p.currentDisplay}</span>
              <ChevronDown className="icon-sm shrink-0 opacity-60" />
            </button>
            {modelOpen && (
              <PopoverShell containerRef={modelContainerRef} triggerRef={modelTriggerRef} align="right" width="xl" side="top">
                <PopoverBody maxHeight="max-h-72">
                  {!p.byProvider || p.byProvider.length === 0 ? (
                    <div className="px-2.5 py-8 text-center">
                      <p className="text-[length:var(--text-ui)] text-text-secondary">暂无可用模型</p>
                    </div>
                  ) : (
                    p.byProvider.map((group) => (
                      <div key={group.provider} className="pb-0.5 last:pb-0">
                        {/* 分组标题是**静态标签**，不是可点击项：模型卡片要一眼
                            扫完（对齐参考稿的「内置模型」段头），多一层折叠只会
                            把候选藏起来。 */}
                        <div className="flex items-center gap-2 px-2.5 pb-1 pt-2">
                          <span className="truncate text-[length:var(--text-meta)] font-medium tracking-wide text-text-faint">
                            {group.label}
                          </span>
                          {group.isFree && (
                            <Badge tone="success" size="chip" pill={false}>
                              免费源
                            </Badge>
                          )}
                          {!group.isFree && group.isCustom && (
                            <span className="shrink-0 rounded-sm bg-bg-tertiary px-1 py-px text-[length:var(--text-tiny,11px)] text-text-faint">
                              自定义
                            </span>
                          )}
                        </div>
                        {group.models.map((m) => {
                          const selected = p.selectedModel === m.id;
                          const ctx = formatContextTokens(m.info?.context_window);
                          return (
                            <button
                              key={m.id}
                              onClick={() => {
                                p.onSwitchModel?.(m.id);
                                p.setSelectedModel(m.id);
                                setModelOpen(false);
                              }}
                              aria-current={selected ? "true" : undefined}
                              title={m.id}
                              className={`flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left transition-colors ${
                                selected
                                  ? "bg-[var(--color-accent-soft)] text-text-primary"
                                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
                              }`}
                            >
                              {/* 免费模型不用厂商齿轮：`:free` 后缀或所在分组是免费池时
                                  显示 ✨（与设置页「免费源」徽章同色系），一眼区分收费/免费。 */}
                              {m.id.toLowerCase().endsWith(":free") || group.isFree ? (
                                <Sparkles
                                  className="h-3.5 w-3.5 shrink-0 text-[var(--color-success)]"
                                  aria-hidden="true"
                                />
                              ) : (
                                <ProviderIcon providerId={m.provider} size={14} className="shrink-0" title={m.provider} />
                              )}
                              <span className="min-w-0 flex-1 truncate text-[length:var(--text-ui)]">{m.name}</span>
                              {/* 默认模型标在列表里：这是「新会话 / 阿黎 /
                                  自动化」实际会用的那条，跟「当前这轮选中的」
                                  是两件事，必须能分开看。 */}
                              {p.defaultModel === m.id && (
                                <span className="shrink-0 rounded-sm bg-bg-tertiary px-1 py-px text-[length:var(--text-tiny,11px)] text-accent">
                                  默认
                                </span>
                              )}
                              {m.info?.supports_reasoning && (
                                <span className="shrink-0 rounded-sm bg-bg-tertiary px-1 py-px text-[length:var(--text-tiny,11px)] text-text-faint">
                                  思考
                                </span>
                              )}
                              {m.info?.supports_vision && (
                                <span className="shrink-0 rounded-sm bg-bg-tertiary px-1 py-px text-[length:var(--text-tiny,11px)] text-text-faint">
                                  视觉
                                </span>
                              )}
                              {/* 上下文按固定宽度右对齐：一列数字读起来才像表格，
                                  不会因为有的模型没报而为每一行错位。 */}
                              <span className="w-10 shrink-0 text-right font-mono text-[length:var(--text-meta)] text-text-faint">
                                {ctx ?? "—"}
                              </span>
                              <Check
                                className={`icon-sm shrink-0 text-accent ${selected ? "" : "opacity-0"}`}
                                aria-hidden="true"
                              />
                            </button>
                          );
                        })}
                      </div>
                    ))
                  )}
                </PopoverBody>

                {/* 附加信息：上下文 + 思考开关，免责独占一行。 */}
                {p.byProvider && p.byProvider.length > 0 && (() => {
                  const currentModelId = p.selectedModel;
                  const ctxEntry = currentModelId ? p.contextWindows?.[currentModelId] : undefined;
                  const ctxLabel = formatContextTokens(ctxEntry?.value ?? 0) ?? "未知";
                  const showThinkingToggle = p.supportsReasoning === true;
                  const selectedInfo = p.models.find((m) => m.id === currentModelId)?.info;
                  return (
                    <div className="mt-1 space-y-2 rounded-md bg-bg-tertiary px-2.5 py-2">
                      <div className="flex items-center justify-between gap-3">
                        <span className="text-[length:var(--text-meta)] text-text-muted">
                          上下文{" "}
                          <span className="font-mono text-text-secondary">{ctxLabel}</span>
                        </span>
                        {showThinkingToggle && (
                          <Switch
                            label="深度思考"
                            checked={p.enableThinking === true}
                            onChange={(v) => p.onEnableThinkingChange?.(v)}
                            size="sm"
                          />
                        )}
                      </div>
                      {selectedInfo?.is_free && (
                        <p className="truncate text-[length:var(--text-meta)] text-text-faint">
                          {selectedInfo.trains_on_prompts
                            ? "免费源 · 提示词会被上游用于训练，敏感内容不要发送"
                            : "免费源 · 可能随时失效"}
                        </p>
                      )}
                    </div>
                  );
                })()}

                {/* 底部常驻操作：固定在列表下方，不随列表滚动。 */}
                <div className="mt-1.5 pt-0.5">
                  <button
                    type="button"
                    className="flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-left text-[length:var(--text-ui)] text-text-secondary transition-colors hover:bg-bg-hover hover:text-text-primary"
                    onClick={() => { p.onOpenSettings?.("model"); setModelOpen(false); }}
                  >
                    <Settings className="icon-sm shrink-0 opacity-70" />
                    管理模型
                  </button>
                </div>
              </PopoverShell>
            )}
          </div>
          <VisionButton onFilesAttach={(files) => p.onFilesAttach?.(files)} />
          <VoiceButton onTranscribe={(text) => injectIntoComposer({ source: "voice", text, mode: "at-caret" })} />
          <EnhanceButton
            draft={p.input}
            model={p.selectedModel}
            onResult={(enhanced) => injectIntoComposer({ source: "enhance", text: enhanced, mode: "replace" })}
            recentTurns={recentTurns}
            hasAttachments={hasAttachments}
            disabled={p.sending || p.isStreaming}
            compact={toolbarCompact}
          />

          {(() => {
            // Streaming 中输入框有内容时,发送按钮变成"加入队列"(绿色
            // Send + Clock 叠加角标),点击/Enter 都会进队列等当前回复
            // 完成后自动发。Square 停止按钮独立放到输入框右上角。
            const canQueue = !!(p.isStreaming && (p.input.trim() || (p.pendingImages && p.pendingImages.length > 0)));
            const hasContent =
              !!p.input.trim() ||
              !!(p.pendingImages && p.pendingImages.length > 0) ||
              !!(p.messageQueue && p.messageQueue.length > 0);
            // 队列路径永远可点：图片在压缩 / 上一条还没发完都跟"加入队列"
            // 无关，等当前回复完才轮到它。只有"立即发送"路径要避开
            // sending/isProcessingFiles 以免重复触发。
            const sendDisabled =
              !hasContent || (!canQueue && (p.sending || !!p.isProcessingFiles));
            const inProgress = !!(p.isStreaming || p.sending);
            // 空闲 OR (streaming 且有未发内容): 显示发送按钮(后者走
            // canQueue 分支,变成"加入队列"语义)
            const showSendButton = !inProgress || canQueue;
            // 互斥显示:队列按钮亮起时隐藏 Square,空状态才让停。
            const showStopButton = !!p.isStreaming && !canQueue;
            return (
              <>
                {showSendButton && (
                  <button
                    onClick={p.handleSubmit}
                    disabled={sendDisabled}
                    aria-label={canQueue ? "加入队列" : "发送"}
                    /* 墨与纸 v4：发送钮是结构件不是 CTA —— 永不用强调色，
                       主行动靠位置（右下角）表达（WorkBuddy 规律 4）。 */
                    className={`relative flex items-center justify-center w-8 h-8 rounded-full transition-opacity disabled:opacity-30 disabled:cursor-not-allowed ${
                      hasContent ? "send-btn--ready" : "send-btn--idle"
                    }`}
                    title={canQueue ? "加入队列，等当前回复完成后自动发送 (Enter)" : "发送 (Enter)"}
                  >
                    <Send className="icon-md" />
                    {canQueue && (
                      <Clock className="icon-xs absolute -top-0.5 -right-0.5 rounded-full bg-bg-quaternary p-[1px] text-text-secondary" />
                    )}
                  </button>
                )}
                {showStopButton && (
                  <button
                    onClick={() => p.onStop?.(p.activeComposerSessionId)}
                    /* R3-2：与发送键同位同尺寸（32px），消除互斥切换时的跳版 */
                    className="tint-error flex items-center justify-center w-8 h-8 rounded-lg transition-opacity hover:opacity-90 text-text-error"
                    aria-label="停止生成"
                    title="停止生成"
                  >
                    <Square fill="currentColor" className="icon-sm" />
                  </button>
                )}
              </>
            );
          })()}
        </div>
      </div>
        {p.variant !== "workbench" && (
          <div className="composer-subbar">
          <div className="relative">
            <button
              ref={projTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={() => {
                if (p.workspaceLocked) return;
                projToggle();
              }}
              disabled={p.workspaceLocked}
              className={`flex items-center shrink-0 text-meta transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[160px]"
              } ${
                p.workspaceLocked
                  ? "text-text-faint cursor-not-allowed opacity-60"
                  : p.activeWorkspace
                  ? "text-[color:var(--color-chip-workspace-fg)] hover:bg-[color:var(--color-chip-workspace-bg)] font-medium cursor-pointer"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary cursor-pointer"
              }`}
              title={
                p.workspaceLocked
                  ? "对话进行中，工作区已锁定"
                  : p.activeWorkspace
                  ? `已限制到：${p.activeWorkspace.path}（点击可重新选择）`
                  : "限制当前对话的读写范围"
              }
            >
              {p.workspaceLocked ? (
                <Lock className="icon-sm text-text-faint" />
              ) : (
                <FolderOpen className={`icon-sm ${p.activeWorkspace ? "text-[color:var(--color-chip-workspace-fg)]" : "text-text-faint"}`} />
              )}
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.activeWorkspace ? p.activeWorkspace.name : "目录"}
              </span>
            </button>
            {projOpen && (
              <PopoverShell containerRef={projContainerRef} triggerRef={projTriggerRef} width="sm" side="top">
                <button
                  onClick={async () => { await p.onAddWorkspace(); setProjOpen(false); }}
                  className="flex items-center justify-center gap-1.5 w-full px-2.5 py-2 text-meta text-[color:var(--color-chip-workspace-fg)] hover:bg-[color:var(--color-chip-workspace-bg)] rounded-lg transition-colors font-medium"
                  title="选择本地目录添加为工作区"
                >
                  添加新工作区
                </button>
                {p.workspaces.length > 0 && (
                  <>
                    <div className="mt-1.5" />
                    <PopoverBody maxHeight="max-h-48">
                      {p.workspaces.map((ws) => (
                        <button
                          key={ws.id}
                          onClick={() => { p.onSelectWorkspace(ws.path); setProjOpen(false); }}
                          className={`flex items-center gap-2.5 w-full px-2.5 py-2.5 text-left rounded-lg hover:bg-bg-hover transition-colors ${
                            p.activeWorkspace?.id === ws.id ? "bg-[color:var(--color-chip-workspace-bg)]" : ""
                          }`}
                          title={ws.path}
                        >
                          <FolderOpen className="icon-md shrink-0 text-[color:var(--color-chip-workspace-fg)] opacity-70" />
                          <div className="flex-1 min-w-0">
                            <div className="text-ui text-text-primary truncate">{ws.name}</div>
                            <div className="text-tiny text-text-faint truncate">{ws.path}</div>
                          </div>
                          {p.activeWorkspace?.id === ws.id && <Check className="icon-md text-[color:var(--color-chip-workspace-fg)] shrink-0" />}
                        </button>
                      ))}
                    </PopoverBody>
                    <div className="h-1" />
                    <button
                      onClick={() => { p.onSelectWorkspace(null); setProjOpen(false); }}
                      className="flex items-center justify-center w-full px-2.5 py-2 text-tiny text-text-muted hover:bg-bg-hover rounded-lg transition-colors"
                    >
                      不绑定目录
                    </button>
                  </>
                )}
              </PopoverShell>
            )}
          </div>

          <div className="relative">
            <button
              ref={permTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={permToggle}
              className={`flex items-center shrink-0 text-meta transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[160px]"
              } ${
                p.permissionLevel === "full"
                  ? "text-[color:var(--color-chip-permission-fg)] hover:bg-[color:var(--color-chip-permission-bg)] font-medium"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
              }`}
            >
              {p.permissionLevel === "full"
                ? <AlertTriangle className="icon-sm text-[color:var(--color-chip-permission-fg)]" />
                : <ShieldCheck className="icon-sm text-text-muted" />}
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.permissionLevel === "full" ? "完全访问" : "默认权限"}
              </span>
            </button>
            {permOpen && (
              <PopoverShell containerRef={permContainerRef} triggerRef={permTriggerRef} width="md" side="top">
                <PopoverBody maxHeight="max-h-48">
                  {PERMISSION_LEVELS.map((lvl) => (
                    <button
                      key={lvl.id}
                      onClick={() => { p.setPermissionLevel(lvl.id); setPermOpen(false); }}
                      className={`w-full px-2.5 py-3 text-left rounded-lg hover:bg-bg-hover transition-colors ${
                        p.permissionLevel === lvl.id ? "bg-[color:var(--color-chip-permission-bg)]" : ""
                      }`}
                    >
                      <div className="flex items-start gap-2">
                        <span className="shrink-0 mt-0.5">
                          {lvl.id === "full"
                            ? <AlertTriangle className="icon-lg text-[color:var(--color-chip-permission-fg)]" />
                            : <ShieldCheck className="icon-lg text-[color:var(--color-chip-workspace-fg)]" />}
                        </span>
                        <div className="flex-1 min-w-0">
                          <div className="text-ui text-text-primary font-medium">{lvl.label}</div>
                          <div className="text-tiny text-text-faint mt-0.5 leading-snug">{lvl.description}</div>
                        </div>
                        {p.permissionLevel === lvl.id && <Check className="icon-md text-[color:var(--color-chip-model-fg)] shrink-0 mt-0.5" />}
                      </div>
                    </button>
                  ))}
                </PopoverBody>
              </PopoverShell>
            )}
          </div>
          </div>
        )}
      </div>
    </div>
  );
}
