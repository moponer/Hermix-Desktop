import { useState, useRef, useEffect, useMemo, useCallback } from "react";
import { Send, Square, FolderOpen, ChevronDown, ChevronRight, Sparkles, ShieldCheck, Image, X, Check, AlertTriangle, Lock } from "lucide-react";
import { usePopover } from "../hooks/usePopover";
import { PopoverShell, PopoverBody } from "./PopoverShell";
import { InlineExpertPanel } from "./InlineExpertPanel";
import { QueueItemRow } from "./ChatIndicators";
import { VoiceButton } from "./VoiceButton";
import { VisionButton } from "./VisionButton";
import { PolishButton } from "./PolishButton";
import { ProviderIcon } from "./ProviderIcon";
import { AppMentionInput } from "./AppMentionInput";
import { formatBytes } from "../utils/imageCompressor";
import { PERMISSION_LEVELS, type PermissionLevel } from "../hooks/useExpert";
import type { Workspace } from "../hooks/useWorkspaces";
import type { ExpertDef } from "../data/builtinExperts";
import type { QueuedMessage, Session } from "../types";
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
  onOpenSettings?: () => void;
  onOpenComputer?: () => void;
  onPickRecent?: (id: string) => void;
  onUndo?: () => void;
  onRestore?: () => void;
  onFilesAttach?: (files: File[]) => void;
  pendingImages?: PendingImage[];
  isProcessingFiles?: boolean;
  onRemoveImage?: (index: number) => void;
  keepOriginalImage?: boolean;
  setKeepOriginalImage?: (v: boolean) => void;
  onStop?: () => void;
  onSwitchModel?: (newId: string) => void;
  permissionLevel: PermissionLevel;
  setPermissionLevel: (lvl: PermissionLevel) => void;
  placeholder: string;
  messageQueue?: QueuedMessage[];
  onAddToQueue?: (msg: QueuedMessage) => void;
  onClearQueue?: () => void;
  onRemoveFromQueue?: (idx: number) => void;
  onEditQueueItem?: (idx: number, newText: string) => void;
  onSendFromQueue?: (idx: number) => void;
  session?: Session | null;
  customApps?: CustomAppSummary[];
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

  const hasAttachments = (p.pendingImages?.length ?? 0) > 0;

  return (
    <div
      className="rounded-2xl transition-all relative"
      style={{
        background: "var(--color-bg-primary)",
        border: "1px solid var(--color-border-primary)",
        boxShadow: "var(--shadow-pop)",
        overflow: "visible",
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
          className="absolute inset-0 z-10 rounded-2xl flex items-center justify-center pointer-events-none"
          style={{
            background: "color-mix(in srgb, var(--color-chip-workspace-fg) 12%, transparent)",
            border: "2px dashed color-mix(in srgb, var(--color-chip-workspace-fg) 60%, transparent)",
          }}
        >
          <div className="text-[13px] text-[color:var(--color-chip-workspace-fg)] font-medium bg-bg-elevated/90 px-3 py-1.5 rounded-lg">
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
                  <span className="absolute bottom-0 left-0 right-0 text-[8px] leading-tight bg-black/65 text-white text-center">
                    {formatBytes(img.sizeBytes)}
                  </span>
                )}
                <button
                  type="button"
                  onClick={() => p.onRemoveImage?.(idx)}
                  className="absolute top-0.5 right-0.5 p-0.5 rounded-md bg-bg-elevated/90 text-text-muted hover:text-error transition-colors"
                  title="移除附件"
                >
                  <X size={10} />
                </button>
              </div>
            ))}
          </div>
          <div className="flex items-center gap-2 mt-1.5">
            <span className="h-tiny text-text-muted flex items-center gap-1">
              <Image size={10} />
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
        <div className="flex items-center gap-0.5 min-w-0 shrink-0">
          <div className="relative">
            <button
              onClick={() => setExpertPanelOpen((v) => !v)}
              className={`flex items-center shrink-0 text-[12.5px] transition-colors rounded-md ${
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
              <Sparkles size={12} className={p.expert ? "text-[color:var(--color-chip-model-fg)]" : "text-text-muted"} />
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
                  <X size={11} />
                </span>
              )}
            </button>
          </div>

          <div className="relative">
            <button
              ref={projTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={() => {
                if (p.workspaceLocked) return;
                projToggle();
              }}
              disabled={p.workspaceLocked}
              className={`flex items-center shrink-0 text-[12.5px] transition-colors rounded-md ${
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
                <Lock size={12} className="text-text-faint" />
              ) : (
                <FolderOpen size={12} className={p.activeWorkspace ? "text-[color:var(--color-chip-workspace-fg)]" : "text-text-faint"} />
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
                  className="flex items-center justify-center gap-1.5 w-full px-2.5 py-2 text-[12px] text-[color:var(--color-chip-workspace-fg)] hover:bg-[color:var(--color-chip-workspace-bg)] rounded-lg transition-colors font-medium"
                  title="选择本地目录添加为工作区"
                >
                  添加新工作区
                </button>
                {p.workspaces.length > 0 && (
                  <>
                    <div className="my-1 border-t border-border-primary" />
                    <div className="px-2.5 py-1 text-[11.5px] text-text-secondary font-medium">工作目录</div>
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
                          <FolderOpen size={15} className="shrink-0 text-[color:var(--color-chip-workspace-fg)] opacity-70" />
                          <div className="flex-1 min-w-0">
                            <div className="text-[12.5px] text-text-primary truncate">{ws.name}</div>
                            <div className="text-[10.5px] text-text-faint truncate">{ws.path}</div>
                          </div>
                          {p.activeWorkspace?.id === ws.id && <Check size={14} className="text-[color:var(--color-chip-workspace-fg)] shrink-0" />}
                        </button>
                      ))}
                    </PopoverBody>
                    <div className="border-t border-border-primary" />
                    <button
                      onClick={() => { p.onSelectWorkspace(null); setProjOpen(false); }}
                      className="flex items-center justify-center w-full px-2.5 py-2 text-[11px] text-text-muted hover:bg-bg-hover rounded-lg transition-colors"
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
              className={`flex items-center shrink-0 text-[12.5px] transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[160px]"
              } ${
                p.permissionLevel === "full"
                  ? "text-[color:var(--color-chip-permission-fg)] hover:bg-[color:var(--color-chip-permission-bg)] font-medium"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
              }`}
              title="权限级别"
            >
              {p.permissionLevel === "full"
                ? <AlertTriangle size={12} className="text-[color:var(--color-chip-permission-fg)]" />
                : <ShieldCheck size={12} className="text-text-muted" />}
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.permissionLevel === "full" ? "完全访问" : "默认权限"}
              </span>
            </button>
            {permOpen && (
              <PopoverShell containerRef={permContainerRef} triggerRef={permTriggerRef} width="md" side="top">
                <div className="px-2.5 py-1 text-[11.5px] text-text-secondary font-medium">权限级别</div>
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
                            ? <AlertTriangle size={16} className="text-[color:var(--color-chip-permission-fg)]" />
                            : <ShieldCheck size={16} className="text-[color:var(--color-chip-workspace-fg)]" />}
                        </span>
                        <div className="flex-1 min-w-0">
                          <div className="text-[12.5px] text-text-primary font-medium">{lvl.label}</div>
                          <div className="text-[10.5px] text-text-faint mt-0.5 leading-snug">{lvl.description}</div>
                        </div>
                        {p.permissionLevel === lvl.id && <Check size={14} className="text-[color:var(--color-chip-model-fg)] shrink-0 mt-0.5" />}
                      </div>
                    </button>
                  ))}
                </PopoverBody>
              </PopoverShell>
            )}
          </div>
        </div>

        <div className="flex items-center gap-0.5 ml-auto shrink-0">
          <div className="relative">
            <button
              ref={modelTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={(p.byProvider && p.byProvider.length > 0) ? modelToggle : () => { p.onOpenSettings?.(); setModelOpen(false); }}
              className={`flex items-center gap-1.5 px-2 py-1 text-[12.5px] transition-colors rounded-md max-w-[240px] ${
                (p.byProvider && p.byProvider.length > 0)
                  ? "text-text-secondary hover:bg-bg-hover hover:text-text-primary cursor-pointer"
                  : "text-[color:var(--color-chip-permission-fg)] hover:bg-[#FAECE7]/60 cursor-pointer"
              }`}
              title={(p.byProvider && p.byProvider.length > 0) ? "切换模型" : "未配置模型，点击前往 设置 → 模型管理"}
            >
              <ProviderIcon providerId={p.selectedModel?.split(":")[0] || "custom"} size={14} title={p.selectedModel} />
              <span className="truncate">{p.currentDisplay}</span>
              <ChevronDown size={11} className="shrink-0 opacity-60" />
            </button>
            {modelOpen && (
              <PopoverShell containerRef={modelContainerRef} triggerRef={modelTriggerRef} align="right" width="md" side="top">
                <div className="px-3 pt-2.5 pb-2 flex items-center justify-between border-b border-border-primary mb-1">
                  <div className="flex items-center gap-2">
                    <span className="inline-block w-1 h-4 rounded-sm bg-accent" aria-hidden="true" />
                    <span className="text-[13px] text-text-primary font-semibold tracking-wide">切换模型</span>
                  </div>
                  <span className="text-[10.5px] text-text-muted font-mono">
                    {p.byProvider.reduce((acc, g) => acc + g.models.length, 0)} 个 · {p.byProvider.length} 厂商
                  </span>
                </div>
                <PopoverBody maxHeight="max-h-64">
                  {!p.byProvider || p.byProvider.length === 0 ? (
                    <div className="px-2.5 py-3 text-center">
                      <p className="text-[12px] text-text-muted">暂无可用模型</p>
                      <p className="text-[10.5px] text-text-muted mt-1">请到「设置 → 模型管理」添加</p>
                    </div>
                  ) : (
                    <>
                      {p.byProvider.map((group) => {
                        const isSelectedHere = p.selectedModel
                          ? p.selectedModel.startsWith(group.provider + ":")
                          : false;
                        const expanded =
                          expandedProviders[group.provider] ??
                          (!group.isCustom || isSelectedHere);
                        const currentModelDisplay = isSelectedHere
                          ? p.selectedModel!.slice(group.provider.length + 1)
                          : null;
                        return (
                          <div key={group.provider} className="mb-0.5">
                            <button
                              type="button"
                              onClick={() => toggleProvider(group.provider)}
                              className="flex items-center gap-2 w-full px-2 py-1.5 text-left rounded-md hover:bg-bg-hover transition-colors"
                              title={group.isCustom ? `自定义：${group.provider}（点击展开/折叠）` : `点击展开/折叠 ${group.provider}`}
                              aria-expanded={expanded}
                            >
                              {expanded
                                ? <ChevronDown size={11} className="text-text-muted shrink-0" />
                                : <ChevronRight size={11} className="text-text-muted shrink-0" />
                              }
                              <ProviderIcon providerId={group.provider} size={13} title={group.provider} />
                              <span className="text-[11.5px] text-text-secondary font-medium truncate">
                                {group.provider}
                                {group.isCustom && (
                                  <span className="ml-1 text-[10px] text-text-faint">· 自定义</span>
                                )}
                              </span>
                              <span className="text-[10px] text-text-faint font-mono ml-auto shrink-0">
                                {group.models.length}
                                {currentModelDisplay && (
                                  <span className="ml-1.5 text-text-muted truncate">
                                    · {currentModelDisplay}
                                  </span>
                                )}
                              </span>
                            </button>
                            {expanded && (
                              <div className="pl-4 pr-1 pb-1">
                                {group.models.map((m) => (
                                  <button
                                    key={m.id}
                                    onClick={() => {
                                      p.onSwitchModel?.(m.id);
                                      p.setSelectedModel(m.id);
                                      setModelOpen(false);
                                    }}
                                    className={`flex items-center gap-2 w-full px-2 py-2 text-left rounded-md hover:bg-bg-hover transition-colors ${
                                      p.selectedModel === m.id ? "bg-[color:var(--color-chip-permission-bg)]" : ""
                                    }`}
                                  >
                                    <span className="truncate font-mono text-[12px] text-text-primary">{m.display}</span>
                                    {p.selectedModel === m.id && <Check size={13} className="text-[color:var(--color-chip-model-fg)] shrink-0" />}
                                  </button>
                                ))}
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </>
                  )}
                </PopoverBody>
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

          {p.isStreaming && p.messageQueue && p.messageQueue.length > 0 ? (
            <button
              onClick={() => p.onStop?.()}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all hover:opacity-90"
              style={{
                background: "var(--color-error)",
                color: "white",
              }}
              title="插话：停止当前回复并发送队列消息"
            >
              <Square size={12} fill="currentColor" />
            </button>
          ) : (p.isStreaming || p.sending) ? (
            <button
              onClick={() => p.onStop?.()}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all hover:opacity-90"
              style={{
                background: "var(--color-error)",
                color: "white",
              }}
              title="停止生成"
            >
              <Square size={12} fill="currentColor" />
            </button>
          ) : (
            <button
              onClick={p.handleSubmit}
              disabled={(!p.input.trim() && (!p.pendingImages || p.pendingImages.length === 0) && (!p.messageQueue || p.messageQueue.length === 0)) || p.sending || !!p.isProcessingFiles}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all disabled:opacity-30 disabled:cursor-not-allowed"
              style={{
                background: (p.input.trim() || (p.pendingImages && p.pendingImages.length > 0) || (p.messageQueue && p.messageQueue.length > 0)) ? "var(--color-accent)" : "var(--color-bg-tertiary)",
                color: (p.input.trim() || (p.pendingImages && p.pendingImages.length > 0) || (p.messageQueue && p.messageQueue.length > 0)) ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
              }}
              title="发送 (Enter)"
            >
              <Send size={13} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
