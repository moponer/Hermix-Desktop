import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { Search, MessageSquare, Clock, X } from "lucide-react";
import { clsx } from "clsx";
import type { Session } from "../types";
import { useSessionSearch } from "../hooks/useSessionSearch";

/**
 * 会话搜索面板 —— 标题栏搜索图标与侧栏"查看全部"共用的浮层。
 *
 * 之所以做成模态而不是独立页面：搜索的本质是"动作"而非"位置"。
 * 独立页面形态下，用户为了搜一次要付两次页面切换（进去、选完出来），
 * 还会丢掉当前所在视图；模态则是搜完即走，当前视图完全不受影响。
 *
 * `sessions` 传全量列表则搜索范围是全部会话；侧栏"查看全部"场景传
 * 该分组的会话（对话分组 / 单个项目），此时搜索自动收窄到该范围，
 * 符合"我只在这个列表里找"的直觉。
 */
export interface SessionPickerDialogProps {
  open: boolean;
  sessions: Session[];
  onSelect: (id: string) => void;
  onClose: () => void;
  /** 面板标题，如"全部对话" / 项目名。 */
  title?: string;
}

export function SessionPickerDialog({
  open,
  sessions,
  onSelect,
  onClose,
  title = "全部对话",
}: SessionPickerDialogProps) {
  // 搜索 hook 内部持有 query；关闭再打开时应重置，否则残留上次关键词
  // 会让人以为列表本身就是空的。
  const { query, setQuery, trimmed, hits } = useSessionSearch(sessions);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    setQuery("");
    inputRef.current?.focus();
  }, [open, setQuery]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  // 无关键词时展示完整列表；有关键词时展示命中结果。
  const rows: { session: Session; snippet: string | null }[] = trimmed
    ? hits.map((h) => ({ session: h.session, snippet: h.snippet }))
    : sessions.map((session) => ({ session, snippet: null }));

  return createPortal(
    <div className="fixed inset-0 z-[200] flex items-start justify-center p-4 pt-[12vh]">
      <button
        type="button"
        aria-label="关闭"
        onClick={onClose}
        className="absolute inset-0 bg-black/40 backdrop-blur-sm"
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className={clsx(
          "relative z-10 w-full max-w-wide flex flex-col",
          "rounded-xl border shadow-2xl",
          "bg-bg-secondary border-border-subtle",
          "max-h-[70vh]",
        )}
      >
        {/* 头部：标题 + 搜索框 */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-border-subtle shrink-0">
          <Search className="icon-md text-text-muted shrink-0" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索对话标题或消息内容…"
            className="flex-1 bg-transparent text-ui text-text-primary placeholder:text-text-faint outline-none"
          />
          <span className="text-tiny text-text-faint shrink-0">
            {trimmed ? `${hits.length} 条命中` : `${sessions.length} 条`}
          </span>
          <button
            type="button"
            onClick={onClose}
            aria-label="关闭"
            className="icon-btn shrink-0"
          >
            <X className="icon-md" />
          </button>
        </div>

        {/* 列表 */}
        <div className="overflow-y-auto p-1.5">
          {rows.length === 0 ? (
            <p className="text-meta text-text-muted text-center py-10">
              {trimmed ? "没有找到匹配的对话" : "暂无对话"}
            </p>
          ) : (
            rows.map(({ session, snippet }) => (
              <button
                key={session.id}
                type="button"
                onClick={() => {
                  onSelect(session.id);
                  onClose();
                }}
                className={clsx(
                  "w-full text-left px-3 py-2 rounded-lg",
                  "hover:bg-bg-tertiary transition-colors",
                )}
              >
                <div className="flex items-center gap-2">
                  <MessageSquare className="icon-sm text-text-muted shrink-0" />
                  <span className="flex-1 truncate-1 text-ui font-medium text-text-primary">
                    {session.title || "新建对话"}
                  </span>
                </div>
                {snippet && (
                  <p className="text-tiny text-text-muted line-clamp-2 mt-0.5 ml-5">
                    {snippet}
                  </p>
                )}
                <div className="flex items-center gap-1 mt-1 ml-5">
                  <Clock size={10} className="text-text-faint" />
                  <span className="text-tiny text-text-faint">
                    {new Date(session.updated_at).toLocaleDateString("zh-CN")}
                  </span>
                  <span className="text-tiny text-text-faint ml-2">
                    {session.messages.length} 条消息
                  </span>
                </div>
              </button>
            ))
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
