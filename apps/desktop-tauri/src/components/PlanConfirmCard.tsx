// Plan-confirmation card (role="plan") — rendered from
// `StreamEvent::PlanConfirmationRequired`. Read-only: the backend has
// no confirmation response channel (no `WsClientMessage` variant, no
// agent-loop waiter), so the card is an acknowledgement surface, not a
// gate. Acknowledged cards disappear, matching the clarify card's
// "answered ⇒ gone" behaviour.
import { ListChecks } from "lucide-react";
import { formatRelativeTime } from "../utils/time";
import type { ChatMessage } from "../types";

export function PlanConfirmCard({
  message,
  onAcknowledge,
}: {
  message: ChatMessage;
  onAcknowledge?: (messageId: string) => void;
}) {
  const meta = message.planConfirm;
  if (!meta) {
    return (
      <div className="h-enter flex items-center gap-2 text-meta text-text-faint italic">
        <ListChecks className="icon-sm" />
        <span>计划确认数据缺失</span>
      </div>
    );
  }
  if (meta.acknowledged) return null;

  return (
    <div className="flex gap-2.5 h-enter items-start">
      <div
        className="shrink-0 mt-0.5 w-[22px] h-[22px] rounded-md flex items-center justify-center"
        style={{ background: "color-mix(in srgb, var(--color-info) 12%, transparent)" }}
        title="计划确认"
      >
        <ListChecks className="icon-sm text-accent" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 mb-1 flex-wrap">
          <span className="text-ui font-semibold text-text-primary">执行计划</span>
          <span className="h-tiny">· {formatRelativeTime(message.timestamp)}</span>
        </div>
        <div className="msg-card msg-stack">
          <div className="msg-channel" data-channel="plan-confirm">
            <div
              className="prose max-w-none text-ui text-text-primary whitespace-pre-wrap"
              style={{ maxHeight: "320px", overflow: "auto" }}
            >
              {meta.planText?.trim() || (
                <span className="h-tiny italic text-text-faint">（空计划）</span>
              )}
            </div>
            <div className="flex justify-end mt-2">
              <button
                type="button"
                onClick={() => onAcknowledge?.(message.id)}
                className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-md text-meta font-medium transition-colors"
                style={{
                  background: "var(--color-accent)",
                  color: "var(--color-on-accent)",
                }}
              >
                知道了
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
