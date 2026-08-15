import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";

// 复制按钮常驻在所属消息上，hover/focus 淡入，不挤占元数据头部
export function MessageCopyAction({
  content,
  className,
}: {
  content: string;
  className: string;
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
        title="复制消息"
        aria-label="复制消息"
      >
        {copied ? <Check size={12} /> : <Copy size={12} />}
        <span>{copied ? "已复制" : "复制"}</span>
      </button>
    </div>
  );
}
