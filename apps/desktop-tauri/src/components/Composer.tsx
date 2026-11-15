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

    /* 2026-11-07：观测对象 = .composer-footer 本身。它是块级元素，宽度只由
       卡片决定，与 compact 与否无关 —— 观测值稳定，不存在「宽度→compact→
       宽度」的测量回环（更早的版本曾观测工具行自身，宽度在 614/706 之间
       每秒翻转数次，整条底栏跟着抖）。阈值 640：全标签排布约需 660px，
       收成图标后 ~380px 也放得下。 */
    const calc = () => {
      setToolbarCompact(el.clientWidth < 640);
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
    <>
      {/* 待发队列住在**输入卡之外**：它描述的是「还没轮到说的话」，
          不是本次草稿的一部分。放在卡里会和 textarea 抢同一块面，
          也跟着卡片的拖放 / 点击聚焦一起被当成输入区。 */}
      {p.messageQueue && p.messageQueue.length > 0 && (() => {
        const failedCount = p.messageQueue!.filter((m) => m.status === "failed").length;
        return (
          <div className="composer-queue">
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() => setQueueOpen((v) => !v)}
                aria-expanded={queueOpen}
                className="flex h-6 min-w-0 items-center gap-1.5 rounded px-1 text-[length:var(--text-meta)] text-text-muted transition-colors hover:text-text-primary"
              >
                <ChevronDown className={`icon-xs shrink-0 transition-transform ${queueOpen ? "" : "-rotate-90"}`} />
                <span className="truncate">{p.messageQueue!.length} 条排队中</span>
                {failedCount > 0 && (
                  <span className="shrink-0 text-text-error">· {failedCount} 条失败</span>
                )}
              </button>
              <div className="ml-auto flex shrink-0 items-center gap-0.5">
                {p.onSendFromQueue && (
                  <button
                    type="button"
                    onClick={() => p.onSendFromQueue?.(p.messageQueue![0]!.id)}
                    className="flex h-6 items-center gap-1 rounded px-1.5 text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
                    /* 标签直接写明「停止并发送」：这枚按钮在流式里会先停掉
                       当前回复再发这条，不再是"等当前跑完"。 */
                    title={p.isStreaming ? "停止当前回复，立即发送这条" : "立即发送这条"}
                    aria-label={p.isStreaming ? "停止当前回复并发送队首消息" : "立即发送队首消息"}
                  >
                    <Send className="icon-sm shrink-0" />
                    <span>{p.isStreaming ? "停止并发送" : "发送"}</span>
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => p.onClearQueue?.()}
                  className="flex h-6 items-center rounded px-1.5 text-[length:var(--text-meta)] text-text-muted transition-colors hover:bg-bg-quaternary hover:text-text-primary"
                >
                  清空
                </button>
              </div>
            </div>
            {queueOpen && (
              <ul className="mt-1 max-h-48 space-y-0.5 overflow-y-auto">
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

      {/* ① 上下文条已并入底部控制条（2026-11-07）：此前专家/模式单独浮在
          卡片左上角，与正文之间没有任何边界，读起来是两行迷路的小字。
          现在「本轮设置」统一在控制条左侧（目录/权限/专家/模式），
          「模型 + 动作 + 发送」在右侧 —— 设置在左、动作在右，一行扫完。 */}
      {p.pendingImages && p.pendingImages.length > 0 && (
        <div className="px-3 pt-3 pb-1">
          <div className="flex items-center gap-2 overflow-x-auto">
            {p.pendingImages.map((img, idx) => (
              <div
                key={`${img.blobUrl}-${idx}`}
                className="relative w-14 h-14 rounded-lg overflow-hidden flex items-center justify-center shrink-0 bg-bg-quaternary"
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
                className="relative flex items-center gap-2 max-w-[220px] h-14 px-2 rounded-lg overflow-hidden shrink-0 bg-bg-quaternary"
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
      {/* 控制条：与正文同一张面，无独立底色。左 = 本轮设置（目录/权限/
          专家/模式），右 = 模型 + 动作（附件/语音/增强）+ 发送。
          compact（容器 < 640px）时设置与动作收成图标，模型名保留。 */}
      <div className="composer-footer" ref={toolbarRef} data-compact={toolbarCompact ? "true" : "false"}>
        {p.variant !== "workbench" && (
          <div className="composer-scope">
            <div className="relative">
              <button
                ref={projTriggerRef as React.RefObject<HTMLButtonElement>}
                onClick={() => {
                  if (p.workspaceLocked) return;
                  projToggle();
                }}
                disabled={p.workspaceLocked}
                className={`composer-chip ${
                  toolbarCompact ? "composer-chip--compact" : ""
                } ${
                  p.workspaceLocked
                    ? "cursor-not-allowed opacity-60"
                    : p.activeWorkspace
                    ? "composer-chip--on cursor-pointer"
                    : "cursor-pointer"
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
                  <Lock />
                ) : (
                  <FolderOpen />
                )}
                <span className="max-w-[120px]">
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
                className={`composer-chip ${
                  toolbarCompact ? "composer-chip--compact" : ""
                } ${p.permissionLevel === "full" ? "composer-chip--on" : ""}`}
                title="权限档位"
              >
                {p.permissionLevel === "full" ? <AlertTriangle /> : <ShieldCheck />}
                <span className="max-w-[120px]">
                  {p.permissionLevel === "full" ? "放手模式" : "默认模式"}
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

            <div className="relative">
              <button
                onClick={() => setExpertPanelOpen((v) => !v)}
                className={`composer-chip ${
                  toolbarCompact ? "composer-chip--compact" : ""
                } ${p.expert ? "composer-chip--on" : ""}`}
                title={p.expert ? `已雇佣：${p.expert.name}` : "选择专家"}
              >
                <Sparkles />
                <span className="max-w-[120px]">
                  {p.expert ? p.expert.name : "专家"}
                </span>
                {p.expert && (
                  <span
                    role="button"
                    tabIndex={0}
                    aria-label="清除已选专家"
                    onClick={(e) => { e.stopPropagation(); p.setExpert(null); }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        e.stopPropagation();
                        p.setExpert(null);
                      }
                    }}
                    className="hover:opacity-70 shrink-0"
                  >
                    <X />
                  </span>
                )}
              </button>
            </div>

            {/* 工作 / 编码模式切换 —— 直接点按切换（两档无需弹层）。
                code = pin 代码开发域（DomainMode::Code）；auto = 交给后端自动识别。 */}
            <button
              onClick={() => p.setDomainMode(p.domainMode === "code" ? "auto" : "code")}
              className={`composer-chip ${
                toolbarCompact ? "composer-chip--compact" : ""
              } ${p.domainMode === "code" ? "composer-chip--on" : ""}`}
              title="固定代码开发域"
            >
              <Code2 />
              <span>{p.domainMode === "code" ? "编码模式" : "自动"}</span>
            </button>
          </div>
        )}

        <div className="composer-actions">
          <div className="relative">
            <button
              ref={modelTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={(p.byProvider && p.byProvider.length > 0) ? modelToggle : () => { p.onOpenSettings?.("model"); setModelOpen(false); }}
              className={`composer-chip max-w-[200px] ${
                (p.byProvider && p.byProvider.length > 0)
                  ? "cursor-pointer"
                  : "composer-chip--alert cursor-pointer"
              }`}
              title={
                p.byProvider && p.byProvider.length > 0
                  ? p.defaultModel
                    ? `切换模型 · 默认 ${p.models.find((m) => m.id === p.defaultModel)?.name ?? p.defaultModel}`
                    : "切换模型"
                  : "未配置模型，点击直达 设置 → 模型配置"
              }
            >
              {(p.byProvider && p.byProvider.length > 0) ? (
                <ProviderIcon providerId={p.selectedModel?.split(":")[0] || "custom"} title={p.selectedModel} />
              ) : (
                <AlertTriangle />
              )}
              <span className="truncate">
                {(p.byProvider && p.byProvider.length > 0) ? p.currentDisplay : "未配置模型"}
              </span>
              <ChevronDown className="opacity-60" />
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
            // 只要在流式，停止按钮就必须在。旧实现用 `!canQueue` 把它藏起来：
            // 用户为了补充要求刚打了几个字，"停止"就消失换成了"加入队列"，
            // 主观感受是「停止按钮点了没反应」。两者现在并排，互不遮挡。
            const showStopButton = !!p.isStreaming;
            return (
              <>
                {showSendButton && (
                  <button
                    onClick={p.handleSubmit}
                    disabled={sendDisabled}
                    aria-label={canQueue ? "加入队列" : "发送"}
                    /* 墨与纸 v4：发送钮是结构件不是 CTA —— 永不用强调色，
                       主行动靠位置（右下角）表达（WorkBuddy 规律 4）。 */
                    className={`send-btn disabled:opacity-30 disabled:cursor-not-allowed ${
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
                    /* R3-2：与发送键同位同尺寸同形状（32px 圆），
                       消除互斥切换时的跳版 —— 之前是方形，切换时会"换了个按钮"。 */
                    className="send-btn tint-error text-text-error hover:opacity-90"
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
    </div>
    </>
  );
}
