// src/components/ChatComposerInput.tsx
// 重写 — 参考 ChatView.tsx 的 Composer 同款布局。
// 主诉：之前是单行 <input>，没工具栏、没 textarea 自动高度、没队列指示。
// GroupChatView 输入卡顿也是因为父组件 1500 行每次 setInputText 都重渲染。
// 本版改进：
// · 用 <textarea>（min-h-72 → max-h-200 自动撑开）+ Enter 发送 / Shift+Enter 换行
// · **状态下沉**：input 状态放本组件内部 useState，父组件传 ref 读取
// → 父组件 GroupChatView 每次输入零重渲染（textarea onChange 只更新本组件）
// · 容器样式 100% 复刻 ChatView Composer：rounded-2xl / border / shadow / px-4
// · 工具栏对齐：右边发送按钮（32x32 square）+ 停止按钮（仅 streaming 时）
// · isStreaming 时 placeholder 改为"输入消息加入队列..."
// 群聊不需要 ChatView 的专家/目录/权限/模型/语音/图片/润色等复杂元素，
// 最小化原则 —— 只复刻"用户输入 + 发送"这层。

import { forwardRef, memo, useCallback, useImperativeHandle, useRef, useState } from "react";
import { Send, Square } from "lucide-react";

export interface ChatComposerInputHandle {
  /** 读取当前输入文本（不触发重渲染） */
  getValue: () => string;
  /** 清空输入 */
  clear: () => void;
  /** 聚焦 */
  focus: () => void;
}

export interface ChatComposerInputProps {
  /** 选填：自定义 placeholder */
  placeholder?: string;
  /** 选填：禁用 */
  disabled?: boolean;
  /** 选填：是否正在生成中 → 显示"停止"按钮 */
  isStreaming?: boolean;
  /** 选填：点击发送按钮 / Enter 触发 */
  onSend: (text: string) => void;
  /** 选填：点击停止按钮 */
  onStop?: () => void;
  /** 选填：文件拖放 / 粘贴上传回调 */
  onFilesAttach?: (files: File[]) => void;
}

export const ChatComposerInput = memo(
  forwardRef<ChatComposerInputHandle, ChatComposerInputProps>(function ChatComposerInput(
    { placeholder = "输入消息...", disabled, isStreaming = false, onSend, onStop, onFilesAttach },
    ref,
  ) {
    // ── 状态下沉：父组件零重渲染 ──
    const [value, setValue] = useState("");
    const [focused, setFocused] = useState(false);
    const taRef = useRef<HTMLTextAreaElement>(null);
    const [draggingFiles, setDraggingFiles] = useState(false);
    const dragCounter = useRef(0);

    // 暴露给父组件
    useImperativeHandle(
      ref,
      () => ({
        getValue: () => value,
        clear: () => {
          setValue("");
          // 重置高度
          if (taRef.current) {
            taRef.current.style.height = "auto";
          }
        },
        focus: () => taRef.current?.focus(),
      }),
      [value],
    );

    // 自动撑高 textarea
    const handleChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
      const v = e.target.value;
      setValue(v);
      // auto resize
      const el = e.target;
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 200) + "px";
    }, []);

    // Enter 发送 / Shift+Enter 换行
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
      onStop?.();
    }, [onStop]);

    const canSend = !isStreaming && value.trim().length > 0 && !disabled;

    return (
      <div
        className="rounded-2xl transition-all relative"
        style={{
          background: focused ? "var(--color-bg-primary)" : "var(--color-bg-secondary)",
          border: "1px solid var(--color-border-primary)",
          boxShadow: focused
            ? "0 0 0 3px var(--color-accent-soft), 0 1px 3px rgba(0,0,0,0.04)"
            : "0 1px 3px rgba(0,0,0,0.04), 0 1px 2px rgba(0,0,0,0.03)",
          overflow: "visible",
        }}
        onDragEnter={(e) => {
          if (!e.dataTransfer?.types?.includes("Files")) return;
          e.preventDefault(); // 必须，否则浏览器拒绝 drop
          dragCounter.current += 1;
          setDraggingFiles(true);
        }}
        onDragOver={(e) => {
          if (e.dataTransfer?.types?.includes("Files")) {
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
          }
        }}
        onDragLeave={(e) => {
          // 只有真正离开容器时才减少计数器
          if (e.currentTarget.contains(e.relatedTarget as Node)) return;
          dragCounter.current = Math.max(0, dragCounter.current - 1);
          if (dragCounter.current === 0) setDraggingFiles(false);
        }}
        onDrop={(e) => {
          dragCounter.current = 0;
          setDraggingFiles(false);
          const files = Array.from(e.dataTransfer?.files ?? []);
          if (files.length === 0) return;
          e.preventDefault();
          onFilesAttach?.(files);
        }}
      >
        {/* 拖放附件时的视觉提示遮罩 */}
        {draggingFiles && (
          <div
            className="absolute inset-0 z-10 rounded-2xl flex items-center justify-center pointer-events-none"
            style={{
              background: "rgba(15, 110, 86, 0.08)",
              border: "2px dashed rgba(15, 110, 86, 0.5)",
            }}
          >
            <div className="text-[13px] text-[#0F6E56] font-medium bg-bg-elevated/90 px-3 py-1.5 rounded-lg">
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
            // 粘贴上传：从剪贴板提取文件（图片 + 非图片）
            // clipboardData.files 只包含资源管理器复制的文件；
            // clipboardData.items 包含所有剪贴板项目（包括 Ctrl+C 复制的内容）
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
            // fallback: clipboardData.files
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
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          placeholder={isStreaming ? "输入消息加入队列..." : placeholder}
          disabled={disabled}
          data-no-border
          className="w-full bg-transparent text-text-primary placeholder-text-muted resize-none outline-none px-4 pt-3.5 pb-2 min-h-[72px] disabled:opacity-50"
          style={{
            fontSize: "var(--font-size-body)",
            lineHeight: 1.5,
            border: "none",
            borderRadius: 0,
            boxShadow: "none",
            background: "transparent",
          }}
          rows={2}
        />

        {/* Toolbar — 单右侧发送/停止按钮，参考 ChatView 32x32 样式 */}
        <div className="px-3 pb-3 pt-1 flex items-center justify-end">
          {isStreaming ? (
            <button
              type="button"
              onClick={handleStopClick}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all active:scale-95 hover:opacity-90"
              style={{
                background: "var(--color-error)",
                color: "white",
              }}
              title="停止生成"
             aria-label="停止生成">
              <Square size={12} fill="currentColor" />
            </button>
          ) : (
            <button
              type="button"
              onClick={handleSendClick}
              disabled={!canSend}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all active:scale-95 disabled:opacity-30 disabled:cursor-not-allowed"
              style={{
                background: canSend ? "var(--color-accent)" : "var(--color-bg-tertiary)",
                color: canSend ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
              }}
              title="发送 (Enter)"
             aria-label="发送 (Enter)">
              <Send size={13} />
            </button>
          )}
        </div>
      </div>
    );
  }),
);