import { useState } from "react";
import { Check, HelpCircle, Send } from "lucide-react";

export interface ClarifyRequestShape {
  requestId: string;
  question: string;
  choices: string[];
}

export interface InlineClarifyCardProps {
  request: ClarifyRequestShape;
  onAnswer: (requestId: string, answer: string) => void;
  /** Once answered, the server sends `ClarifyResolved` and we render the
   *  resolved footer. Free-form string the caller already has; we just
   *  need to know the question is done. */
  resolved?: { answer: string } | null;
}

/**
 * Inline clarify card — visual sibling of `InlineApprovalCard` so the
 * chat stream keeps a consistent look. Renders a single question and,
 * when the server supplies `choices`, button-style options. When no
 * choices are given (free-form ask) we show a tiny text input. The
 * resolved state dims the card and shows the recorded answer.
 */
export function InlineClarifyCard({
  request,
  onAnswer,
  resolved,
}: InlineClarifyCardProps) {
  const [text, setText] = useState("");
  const isPending = !resolved;
  const hasChoices = request.choices.length > 0;

  const submit = (answer: string) => {
    const trimmed = answer.trim();
    if (!trimmed) return;
    onAnswer(request.requestId, trimmed);
  };

  return (
    <div className="h-enter">
      <div className="flex-1 min-w-0">
        {/* Header — compact: avatar · name · meta */}
        <div className="flex items-center gap-2 mb-1.5">
          <div className="approval-avatar" style={{ background: "var(--color-info-soft)", color: "var(--color-info)" }}>
            <HelpCircle size={12} />
          </div>
          <span className="text-[12.5px] font-semibold text-text-primary">Hermix 在问</span>
          <span className="h-tiny">{"\u00b7"} {isPending ? "等待你的回答" : "已回答"}</span>
        </div>

        {/* Card surface */}
        <div
          className="msg-card approval-card"
          data-resolved={resolved ? "answered" : undefined}
        >
          {/* Line 1: Question text */}
          <div className="approval-pills" style={{ alignItems: "flex-start" }}>
            <span className="approval-tool-pill" style={{ maxWidth: "100%", whiteSpace: "normal" }}>
              <HelpCircle size={11} className="shrink-0 mt-0.5" />
              <span style={{ lineHeight: 1.5 }}>{request.question}</span>
            </span>
            {isPending && (
              <span className="approval-pill approval-pill-pending ml-auto">
                等待回答
              </span>
            )}
            {resolved && (
              <span className="approval-pill approval-pill-approved ml-auto">
                <Check size={9} />
                已回答
              </span>
            )}
          </div>

          {/* Line 2: Choices OR free-form input, with submit */}
          {isPending ? (
            hasChoices ? (
              <div className="flex flex-wrap gap-1.5 mt-2">
                {request.choices.map((choice) => (
                  <button
                    key={choice}
                    onClick={() => submit(choice)}
                    className="approval-btn approval-btn-secondary"
                    style={{ height: 26, padding: "0 10px", fontSize: 11.5 }}
                  >
                    {choice}
                  </button>
                ))}
              </div>
            ) : (
              <div className="flex items-center gap-2 mt-2">
                <input
                  type="text"
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      submit(text);
                    }
                  }}
                  placeholder="输入你的回答…"
                  className="input"
                  style={{ flex: 1, height: 30, fontSize: 12.5, padding: "4px 10px" }}
                  autoFocus
                />
                <button
                  onClick={() => submit(text)}
                  disabled={!text.trim()}
                  className="approval-btn approval-btn-approve"
                  style={{ height: 30, padding: "0 12px" }}
                >
                  <Send size={12} />
                  发送
                </button>
              </div>
            )
          ) : (
            <div className="approval-resolved-footer">
              <span className="h-tiny text-text-faint">你的回答：{resolved?.answer}</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
