// src/components/ChatComposerInput.tsx
// · No independent border/shadow/rounded — flush with parent container
// · Container provides the visual boundary; this component is transparent
// · Proper disabled state: overall opacity 0.5 when disabled
// · Keep textarea auto-height + Enter send / Shift+Enter newline
// · State stays internal (parent zero re-render on input)

import { forwardRef, memo, useCallback, useImperativeHandle, useRef, useState } from "react";
import { Send, Square } from "lucide-react";

export interface ChatComposerInputHandle {
  getValue: () => string;
  clear: () => void;
  focus: () => void;
}

export interface ChatComposerInputProps {
  placeholder?: string;
  disabled?: boolean;
  isStreaming?: boolean;
  onSend: (text: string) => void;
  onStop?: (sessionId?: string) => boolean;
  onFilesAttach?: (files: File[]) => void;
  /** Identifier of the conversation whose stop button is being rendered.
   *  Threaded into the `onStop` call so the chat hook can scope the
   *  cancellation to THIS session (defends against the legacy shared-state
   *  bug where clicking Stop on conversation B killed conversation A). */
  sessionId?: string;
}

export const ChatComposerInput = memo(
  forwardRef<ChatComposerInputHandle, ChatComposerInputProps>(function ChatComposerInput(
    { placeholder = "输入消息...", disabled, isStreaming = false, onSend, onStop, onFilesAttach, sessionId },
    ref,
  ) {
    const [value, setValue] = useState("");
    const taRef = useRef<HTMLTextAreaElement>(null);
    const [draggingFiles, setDraggingFiles] = useState(false);
    const dragCounter = useRef(0);

    useImperativeHandle(
      ref,
      () => ({
        getValue: () => value,
        clear: () => {
          setValue("");
          if (taRef.current) {
            taRef.current.style.height = "auto";
          }
        },
        focus: () => taRef.current?.focus(),
      }),
      [value],
    );

    const handleChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
      const v = e.target.value;
      setValue(v);
      const el = e.target;
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 200) + "px";
    }, []);

    const handleKeyDown = useCallback(
      (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
        if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          const trimmed = value.trim();
          if (!trimmed || disabled || isStreaming) return;
          onSend(trimmed);
          setValue("");
          if (taRef.current) {
            taRef.current.style.height = "auto";
          }
        }
      },
      [value, disabled, isStreaming, onSend],
    );

    const handleSendClick = useCallback(() => {
      const trimmed = value.trim();
      if (!trimmed || disabled || isStreaming) return;
      onSend(trimmed);
      setValue("");
      if (taRef.current) {
        taRef.current.style.height = "auto";
        taRef.current.focus();
      }
    }, [value, disabled, isStreaming, onSend]);

    const handleStopClick = useCallback(() => {
      // Thread the identifier of THIS conversation into the stop
      // call so the chat hook can verify we're actually the active
      // stream session (defends against the legacy "clicking Stop on
      // chat B also cancels chat A" shared-state bug).
      const stopped = onStop?.(sessionId);
      if (stopped === false) {
        console.log(
          `[chat-composer] stop requested for ${sessionId ?? "(no-sid)"} but no matching stream — likely a parallel-chat race, no-op`,
        );
      }
    }, [onStop, sessionId]);

    const canSend = !isStreaming && value.trim().length > 0 && !disabled;

    return (
      <div
        className="relative"
        style={{ opacity: disabled ? 0.5 : 1 }}
        onDragEnter={(e) => {
          if (disabled || !e.dataTransfer?.types?.includes("Files")) return;
          e.preventDefault();
          dragCounter.current += 1;
          setDraggingFiles(true);
        }}
        onDragOver={(e) => {
          if (!disabled && e.dataTransfer?.types?.includes("Files")) {
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
          }
        }}
        onDragLeave={(e) => {
          if (e.currentTarget.contains(e.relatedTarget as Node)) return;
          dragCounter.current = Math.max(0, dragCounter.current - 1);
          if (dragCounter.current === 0) setDraggingFiles(false);
        }}
        onDrop={(e) => {
          if (disabled) return;
          dragCounter.current = 0;
          setDraggingFiles(false);
          const files = Array.from(e.dataTransfer?.files ?? []);
          if (files.length === 0) return;
          e.preventDefault();
          onFilesAttach?.(files);
        }}
      >
        {draggingFiles && (
          <div
            className="absolute inset-0 z-10 flex items-center justify-center pointer-events-none"
            style={{
              background: "color-mix(in srgb, var(--color-accent) 8%, transparent)",
              border: "2px dashed color-mix(in srgb, var(--color-accent) 40%, transparent)",
              borderRadius: 8,
            }}
          >
            <div className="text-[13px] font-medium px-3 py-1.5 rounded-lg" style={{ color: "var(--color-accent)", background: "var(--color-bg-elevated)" }}>
              松开以添加文件
            </div>
          </div>
        )}
        <textarea
          ref={taRef}
          value={value}
          onChange={handleChange}
          onKeyDown={handleKeyDown}
          onPaste={(e) => {
            if (disabled) return;
            const files: File[] = [];
            const items = e.clipboardData?.items;
            if (items) {
              for (let i = 0; i < items.length; i++) {
                const item = items[i];
                if (item.kind === "file") {
                  const file = item.getAsFile();
                  if (file) files.push(file);
                }
              }
            }
            if (files.length === 0) {
              const cf = e.clipboardData?.files;
              if (cf) {
                for (let i = 0; i < cf.length; i++) files.push(cf[i]);
              }
            }
            if (files.length > 0) {
              e.preventDefault();
              onFilesAttach?.(files);
            }
          }}
          onFocus={() => {}}
          onBlur={() => {}}
          placeholder={isStreaming ? "输入消息加入队列..." : placeholder}
          disabled={disabled}
          data-no-border
          className="w-full bg-transparent text-text-primary placeholder:text-text-muted resize-none outline-none px-1 pt-3 pb-1 min-h-[48px]"
          style={{
            fontSize: "var(--font-size-body)",
            lineHeight: 1.5,
            border: "none",
            borderRadius: 0,
            boxShadow: "none",
            background: "transparent",
          }}
          rows={1}
        />

        {/* Toolbar — right-aligned send/stop button */}
        <div className="flex items-center justify-end pb-2 pt-0.5">
          {isStreaming ? (
            <button
              type="button"
              onClick={handleStopClick}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-[transform,opacity] active:scale-95 hover:opacity-90"
              style={{
                background: "var(--color-error)",
                color: "white",
              }}
              title="停止生成"
              aria-label="停止生成"
            >
              <Square className="icon-sm" fill="currentColor" />
            </button>
          ) : (
            <button
              type="button"
              onClick={handleSendClick}
              disabled={!canSend}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-transform active:scale-95 disabled:cursor-not-allowed"
              style={{
                background: canSend ? "var(--color-accent)" : "var(--color-bg-tertiary)",
                color: canSend ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
                opacity: canSend ? 1 : 0.4,
              }}
              title="发送 (Enter)"
              aria-label="发送 (Enter)"
            >
              <Send className="icon-md" />
            </button>
          )}
        </div>
      </div>
    );
  }),
);
