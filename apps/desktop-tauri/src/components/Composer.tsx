import { useState, useRef, useEffect, useMemo, useCallback } from "react";
import { Send, Square, FolderOpen, ChevronDown, ChevronRight, Sparkles, ShieldCheck, Image, X, Check, AlertTriangle, Lock, Clock, Code2, FileText, Brain, BrainCircuit } from "lucide-react";
import type { DomainModeChoice } from "../hooks/useExpert";
import { usePopover } from "../hooks/usePopover";
import { PopoverShell, PopoverBody } from "./PopoverShell";
import { InlineExpertPanel } from "./InlineExpertPanel";
import { QueueItemRow } from "./ChatIndicators";
import { VoiceButton } from "./VoiceButton";
import { VisionButton } from "./VisionButton";
import { PolishButton } from "./PolishButton";
import { ProviderIcon } from "./ProviderIcon";
import type { ConfigCategory } from "./SystemConfigPanel";
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
  models: Array<{ id: string; display: string; provider: string }>;
  selectedModel: string;
  setSelectedModel: (id: string) => void;
  currentDisplay: string;
  byProvider: Array<{ provider: string; isCustom: boolean; models: Array<{ id: string; display: string; provider: string }> }>;
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
  onRemoveFromQueue?: (idx: number) => void;
  onEditQueueItem?: (idx: number, newText: string) => void;
  onSendFromQueue?: (idx: number) => void;
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
  const [permOpen, setPermOpen, permContainerRef, permTriggerRef, permToggle] = usePopover();

  const [expandedProviders, setExpandedProviders] = useState<Record<string, boolean>>({});
  const toggleProvider = useCallback((provider: string) => {
    setExpandedProviders((prev) => ({ ...prev, [provider]: !prev[provider] }));
  }, []);

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
          className="absolute inset-0 z-10 rounded-xl flex items-center justify-center pointer-events-none"
          style={{
            background: "color-mix(in srgb, var(--color-chip-workspace-fg) 12%, transparent)",
            border: "2px dashed color-mix(in srgb, var(--color-chip-workspace-fg) 60%, transparent)",
          }}
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
                className="relative w-14 h-14 rounded-lg overflow-hidden flex items-center justify-center shrink-0"
                style={{ background: "var(--color-bg-tertiary)", border: "1px solid var(--color-border-subtle)" }}
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
                  className="absolute top-0.5 right-0.5 p-0.5 rounded-md bg-bg-elevated/90 text-text-muted hover:text-error transition-colors"
                  title="移除附件"
                >
                  <X className="icon-xs" />
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
                className="relative flex items-center gap-2 max-w-[220px] h-14 px-2 rounded-lg overflow-hidden shrink-0"
                style={{ background: "var(--color-bg-tertiary)", border: "1px solid var(--color-border-subtle)" }}
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
                  className="absolute top-0.5 right-0.5 p-0.5 rounded-md bg-bg-elevated/90 text-text-muted hover:text-error transition-colors"
                  title="移除附件"
                >
                  <X className="icon-xs" />
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

      {p.messageQueue && p.messageQueue.length > 0 && (
        <div className="px-4 pt-2 pb-1.5 border-t border-border-subtle">
          <div className="flex items-center justify-between mb-1.5">
            <span className="h-tiny text-text-muted">
              队列中 {p.messageQueue.length} 条消息
            </span>
            <button
              onClick={() => p.onClearQueue?.()}
              className="h-tiny text-text-muted hover:text-text-primary px-1.5 py-0.5 rounded hover:bg-bg-tertiary"
            >
              清空
            </button>
          </div>
          <ul className="space-y-1 max-h-32 overflow-y-auto">
            {p.messageQueue.map((msg, idx) => (
              <QueueItemRow
                key={`${idx}-${msg.text.slice(0, 24)}`}
                index={idx}
                total={p.messageQueue!.length}
                text={msg.text}
                canEdit={!!p.onEditQueueItem}
                canRemove={!!p.onRemoveFromQueue}
                canSend={!!p.onSendFromQueue}
                onEdit={(t) => p.onEditQueueItem?.(idx, t)}
                onRemove={() => p.onRemoveFromQueue?.(idx)}
                onSend={() => p.onSendFromQueue?.(idx)}
              />
            ))}
          </ul>
        </div>
      )}

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

      <div
        ref={toolbarRef}
        className="px-3 pb-3 pt-1 flex flex-wrap items-center gap-x-1 gap-y-1"
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
            title="编码模式：固定代码开发域，工具优先级与领域指令随之切换；关闭则自动识别工作类型"
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
              title={(p.byProvider && p.byProvider.length > 0) ? "切换模型" : "未配置模型，点击直达 设置 → 模型配置"}
            >
              <ProviderIcon providerId={p.selectedModel?.split(":")[0] || "custom"} className="icon-md" title={p.selectedModel} />
              <span className="truncate">{p.currentDisplay}</span>
              <ChevronDown className="icon-sm shrink-0 opacity-60" />
            </button>
            {modelOpen && (
              <PopoverShell containerRef={modelContainerRef} triggerRef={modelTriggerRef} align="right" width="md" side="top">
                <PopoverBody maxHeight="max-h-72">
                  {!p.byProvider || p.byProvider.length === 0 ? (
                    <div className="px-3 py-6 text-center">
                      <p className="text-meta text-text-muted">暂无可用模型</p>
                      <p className="text-tiny text-text-muted mt-1">点下方「管理模型」去添加</p>
                    </div>
                  ) : (
                    p.byProvider.map((group) => {
                      const isSelectedHere = p.selectedModel
                        ? p.selectedModel.startsWith(group.provider + ":")
                        : false;
                      const expanded = expandedProviders[group.provider] ?? isSelectedHere;
                      return (
                        <div key={group.provider}>
                          <button
                            type="button"
                            onClick={() => toggleProvider(group.provider)}
                            className="flex items-center gap-1.5 w-full px-3 py-1.5 text-left rounded-md hover:bg-bg-hover transition-colors"
                            title={group.isCustom ? `自定义：${group.provider}（点击展开/折叠）` : `点击展开/折叠 ${group.provider}`}
                            aria-expanded={expanded}
                          >
                            {expanded
                              ? <ChevronDown className="icon-sm text-text-muted shrink-0" />
                              : <ChevronRight className="icon-sm text-text-muted shrink-0" />}
                            <span className="text-meta text-text-secondary font-medium truncate">{group.provider}</span>
                            {group.isCustom && <span className="text-tiny text-text-faint shrink-0">自定义</span>}
                          </button>
                          {expanded && (
                            <div className="pb-1">
                              {group.models.map((m) => (
                                <button
                                  key={m.id}
                                  onClick={() => {
                                    p.onSwitchModel?.(m.id);
                                    p.setSelectedModel(m.id);
                                    setModelOpen(false);
                                  }}
                                  className="flex items-center gap-2 w-full px-3 py-2 pl-8 text-left rounded-md hover:bg-bg-hover transition-colors"
                                >
                                  <span className="truncate text-ui text-text-primary">{m.display}</span>
                                  {p.selectedModel === m.id && <Check className="icon-md text-accent shrink-0 ml-auto" />}
                                </button>
                              ))}
                            </div>
                          )}
                        </div>
                      );
                    })
                  )}
                </PopoverBody>

                {/* 附加信息：上下文窗口 + 思考开关（仅当模型支持 reasoning 时显开关） */}
                {p.byProvider && p.byProvider.length > 0 && (() => {
                  const currentModelId = p.selectedModel;
                  const ctxEntry = currentModelId ? p.contextWindows?.[currentModelId] : undefined;
                  const ctxTokens = ctxEntry?.value ?? p.contextWindows?.[currentModelId ?? ""]?.value ?? 0;
                  const ctxLabel =
                    ctxTokens >= 1_000_000
                      ? `${(ctxTokens / 1_000_000).toFixed(ctxTokens % 1_000_000 === 0 ? 0 : 1)}M`
                      : ctxTokens >= 1000
                        ? `${Math.round(ctxTokens / 1000)}K`
                        : ctxTokens > 0
                          ? String(ctxTokens)
                          : "未知";
                  const showThinkingToggle = p.supportsReasoning === true;
                  return (
                    <div className="px-3 py-1.5 border-t border-border-primary flex items-center gap-3 text-meta">
                      <span className="text-text-muted">
                        上下文 <span className="font-mono text-text-secondary">{ctxLabel}</span>
                      </span>
                      {showThinkingToggle && (
                        <button
                          type="button"
                          onClick={() => p.onEnableThinkingChange?.(!(p.enableThinking === true))}
                          className={`ml-auto inline-flex items-center gap-1.5 px-2 py-0.5 rounded-md text-tiny transition-colors ${
                            p.enableThinking === true
                              ? "bg-[color:var(--color-chip-model-bg)] text-[color:var(--color-chip-model-fg)]"
                              : "text-text-muted hover:bg-bg-hover"
                          }`}
                          title="开启/关闭深度思考（对支持 reasoning 的模型生效；切换后随下一条消息下发）"
                        >
                          {p.enableThinking === true
                            ? <Brain className="icon-sm shrink-0" aria-hidden="true" />
                            : <BrainCircuit className="icon-sm shrink-0 opacity-50" aria-hidden="true" />}
                          思考 {p.enableThinking === true ? "开" : "关"}
                        </button>
                      )}
                    </div>
                  );
                })()}

                {/* 底部常驻操作：固定在列表下方，不随列表滚动。 */}
                <div className="border-t border-border-primary">
                  <button
                    type="button"
                    className="w-full px-3 py-2.5 text-left text-meta text-text-secondary hover:bg-bg-hover hover:text-text-primary transition-colors"
                    onClick={() => { p.onOpenSettings?.("model"); setModelOpen(false); }}
                  >
                    管理模型
                  </button>
                </div>
              </PopoverShell>
            )}
          </div>
          <VisionButton onFilesAttach={(files) => p.onFilesAttach?.(files)} />
          <VoiceButton onTranscribe={(text) => p.setInput(p.input + text)} />
          <PolishButton
            draft={p.input}
            onResult={(polished) => p.setInput(polished)}
            recentTurns={recentTurns}
            hasAttachments={hasAttachments}
            disabled={p.sending || p.isStreaming}
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
                    className="relative flex items-center justify-center w-8 h-8 rounded-full transition-opacity disabled:opacity-30 disabled:cursor-not-allowed"
                    style={{
                      background: hasContent
                        ? "var(--color-inverse-surface)"
                        : "var(--color-bg-quaternary)",
                      color: hasContent
                        ? "var(--color-inverse-on-surface)"
                        : "var(--color-text-faint)",
                    }}
                    title={canQueue ? "加入队列，等当前回复完成后自动发送 (Enter)" : "发送 (Enter)"}
                  >
                    <Send className="icon-md" />
                    {canQueue && (
                      <Clock
                        className="icon-xs absolute -top-0.5 -right-0.5 p-[1px] rounded-full"
                        style={{
                          background: "var(--color-bg-quaternary)",
                          color: "var(--color-text-secondary)",
                        }}
                      />
                    )}
                  </button>
                )}
                {showStopButton && (
                  <button
                    onClick={() => p.onStop?.(p.activeComposerSessionId)}
                    /* R3-2：与发送键同位同尺寸（32px），消除互斥切换时的跳版 */
                    className="flex items-center justify-center w-8 h-8 rounded-lg transition-opacity hover:opacity-90"
                    style={{
                      background:
                        "color-mix(in srgb, var(--color-error) 14%, transparent)",
                      color: "var(--color-error)",
                    }}
                    aria-label="停止生成"
                    title={
                      p.messageQueue && p.messageQueue.length > 0
                        ? `插话：停止当前回复并发送队列中的 ${p.messageQueue.length} 条消息`
                        : "停止生成"
                    }
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
              className={`flex items-center shrink-0 text-ui transition-colors rounded-md ${
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
                  : "限制当前对话的读写范围（推荐）"
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
                    <div className="my-1 border-t border-border-primary" />
                    <div className="px-2.5 py-1 text-meta text-text-secondary font-medium">工作目录</div>
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
                    <div className="border-t border-border-primary" />
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
              className={`flex items-center shrink-0 text-ui transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[160px]"
              } ${
                p.permissionLevel === "full"
                  ? "text-[color:var(--color-chip-permission-fg)] hover:bg-[color:var(--color-chip-permission-bg)] font-medium"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
              }`}
              title="权限级别"
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
                <div className="px-2.5 py-1 text-meta text-text-secondary font-medium">权限级别</div>
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
  );
}
