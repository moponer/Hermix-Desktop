import { useState } from "react";
import { Check, Copy, Pencil } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { COPY_MESSAGE } from "./strings";

// 复制按钮常驻在所属消息上，hover/focus 淡入，不挤占元数据头部
export function MessageCopyAction({
  content,
  className,
  onEdit,
}: {
  content: string;
  className: string;
  /** 用户消息：把原文注入输入框（编辑重发）。 */
  onEdit?: () => void;
}) {
  const safeSetTimeout = useSafeTimeout();
  const [copied, setCopied] = useState(false);

  const copyMessage = async () => {
    if (!content.trim()) return;
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      safeSetTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };

  if (!content.trim()) return null;

  return (
    <div className={className}>
      <button
        type="button"
        onClick={copyMessage}
        className="message-copy-button"
        title={COPY_MESSAGE}
        aria-label={COPY_MESSAGE}
      >
        {copied ? <Check className="icon-sm" /> : <Copy className="icon-sm" />}
        <span>{copied ? "已复制" : "复制"}</span>
      </button>
      {onEdit && (
        <button
          type="button"
          onClick={onEdit}
          className="message-copy-button"
          title="编辑并重发"
          aria-label="编辑并重发"
        >
          <Pencil className="icon-sm" />
          <span>编辑</span>
        </button>
      )}
    </div>
  );
}
