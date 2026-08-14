// Thinking/processing indicators previously colocated in MessageBubble.tsx.
import { MarkdownBody } from "./MarkdownBody";
export function ThinkingDots({ label = "正在处理" }: { label?: string }) {
  return (
    <div className="tdots h-enter">
      <span className="tdots-label">{label}</span>
      <span className="tdots-dot" style={{ animationDelay: "0ms" }} />
      <span className="tdots-dot" style={{ animationDelay: "200ms" }} />
      <span className="tdots-dot" style={{ animationDelay: "400ms" }} />
    </div>
  );
}
export function ReasoningBlock({ content }: { content: string }) {
  // Strip both <think> and <reasoning> wrappers — providers use both tags
  // for internal monologue, and the surrounding ProcessTimeline already
  // signals that this is process content.
  const clean = content
    .replace(/<\/?think>/gi, "")
    .replace(/<\/?reasoning>/gi, "")
    .trim();
  if (!clean) return null;

  return (
    <div className="tl-reasoning-row">
      <MarkdownBody content={clean} onPathClick={() => {}} />
    </div>
  );
}
