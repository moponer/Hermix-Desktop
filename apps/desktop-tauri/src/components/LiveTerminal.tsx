// 工具输出面板（原 ToolCallBubble 内部件）。
// 2026-10-01 拆成独立模块：`ProcessLine` 要用它，而 `ToolCallBubble`
// 又要 import `ProcessLine` —— 留在原文件会形成循环依赖。
// 组件本身与拆分前逐字一致，class 名（`tw-msg-output*`）也保持不变，
// 所以 CSS 无需跟着改。
import { useEffect, useMemo, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { unwrapToolOutput } from "./bubbleUtils";
import { COPIED, COPY_OUTPUT, NO_OUTPUT } from "./strings";

export function LiveTerminal({ content, running }: { content: string; running: boolean }) {
  // Unwrap JSON result wrappers so the user sees clean terminal output.
  const displayContent = useMemo(() => unwrapToolOutput(content), [content]);
  // 2026-11-20：运行中跟随输出滚动到底部——「直观看到在哪一步」的一半；
  // 另一半是 ProcessLine 的运行中自动展开。跑完停住，让用户回头翻。
  const scrollRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (running && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [displayContent, running]);
  // Copy-to-clipboard state. ~1.4s "已复制" flash so the affordance
  // gives clear feedback without nagging the user with a toast.
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef<number | null>(null);

  // Cancel any pending "copied" reset on unmount so we don't fire
  // setState on a dead component.
  useEffect(() => () => {
    if (copyTimerRef.current) window.clearTimeout(copyTimerRef.current);
  }, []);

  const handleCopy = async () => {
    const text = displayContent || (running ? "" : NO_OUTPUT);
    if (!text) return;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        // Tauri webview fallback (e.g. http context without the
        // modern clipboard API): hidden textarea + execCommand.
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.left = "-9999px";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      setCopied(true);
      if (copyTimerRef.current) window.clearTimeout(copyTimerRef.current);
      copyTimerRef.current = window.setTimeout(() => setCopied(false), 1400);
    } catch {
      // Silent failure — the user can still select the text manually.
    }
  };

  const hasContent = displayContent.length > 0;

  return (
    <div
      ref={scrollRef}
      className="tw-msg-output"
      data-running={running ? "true" : "false"}
      data-empty={hasContent ? "false" : "true"}
    >
      <button
        type="button"
        className="tw-msg-output-copy"
        onClick={(e) => { e.stopPropagation(); handleCopy(); }}
        data-copied={copied ? "true" : "false"}
        title={copied ? COPIED : COPY_OUTPUT}
        aria-label={copied ? COPIED : COPY_OUTPUT}
      >
        {copied ? <Check className="icon-xs" /> : <Copy className="icon-xs" />}
      </button>
      {hasContent ? (
        <pre className="tw-msg-output-text">
          {displayContent}
          {running && <span className="tw-msg-caret" aria-hidden="true" />}
        </pre>
      ) : (
        <span className="tw-msg-output-empty">
          {running ? "等待输出…" : NO_OUTPUT}
        </span>
      )}
    </div>
  );
}
