// A result delivered by a scheduled automation run (`role === "automation"`).
//
// Why this is a card and not a user or assistant bubble: nobody in the
// conversation typed it, and the model never reads it back (the server
// stores it under `MessageRole::Automation` and filters that role out of
// the LLM payload). Rendering it as either role would assert a
// provenance that does not exist.
//
// Two things this card must never do:
//   - hide *why* a result landed here. `fallbackReason` exists because a
//     silent fallback to a standalone `automation:*` session reads to the
//     user as "the result vanished".
//   - pretend to be part of the conversation's turn structure. It sits
//     between turns, visually distinct, and does not align to the
//     user/assistant columns.
import { useState } from "react";
import { ChevronDown, ChevronRight, Clock, AlertTriangle } from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";
import type { ChatMessage } from "../types";

/** Collapsed height in characters before the body is clipped. */
const PREVIEW_CHARS = 300;

export function AutomationBubble({ message }: { message: ChatMessage }) {
  const meta = message.automation;
  const body = message.content ?? "";
  const [open, setOpen] = useState(false);

  // An empty result still needs to render: a scheduled run that produced
  // nothing is itself information ("it ran, there was nothing to say").
  // Collapsing it to a bare header would lose the run happened at all.
  const truncated = body.length > PREVIEW_CHARS;
  const shown = open || !truncated ? body : body.slice(0, PREVIEW_CHARS) + "…";
  const hasBody = body.trim().length > 0;

  return (
    <div className="my-2 h-enter">
      <div className="rounded-lg bg-surface-raised/40">
        <button
          type="button"
          onClick={() => hasBody && setOpen((v) => !v)}
          disabled={!hasBody}
          className="w-full flex items-center gap-2 px-3 py-2 text-left disabled:cursor-default"
        >
          {hasBody ? (
            open ? (
              <ChevronDown className="icon-sm text-text-faint shrink-0" />
            ) : (
              <ChevronRight className="icon-sm text-text-faint shrink-0" />
            )
          ) : (
            <Clock className="icon-sm text-text-faint shrink-0" />
          )}
          <span className="text-small font-medium text-text-secondary">
            定时任务：{meta?.automationName ?? "未命名任务"}
          </span>
          {meta?.runId ? (
            <span className="text-tiny text-text-faint truncate">{meta.runId}</span>
          ) : null}
        </button>

        {meta?.fallbackReason ? (
          <div className="px-3 pb-2 flex items-start gap-2 text-tiny text-text-faint">
            <AlertTriangle className="icon-sm shrink-0 mt-px" />
            <span>{meta.fallbackReason}</span>
          </div>
        ) : null}

        {hasBody ? (
          <div className="px-3 pb-3">
            {meta?.prompt ? (
              <details className="mb-2 group">
                <summary className="text-tiny text-text-faint cursor-pointer select-none hover:text-text-secondary">
                  查看任务指令
                </summary>
                <pre className="mt-1.5 p-2 rounded bg-surface-sunken text-tiny text-text-secondary whitespace-pre-wrap break-words">
                  {meta.prompt}
                </pre>
              </details>
            ) : null}
            <MarkdownBody content={shown} />
          </div>
        ) : (
          <div className="px-3 pb-3 text-tiny text-text-faint">（本次运行没有产生可见输出）</div>
        )}
      </div>
    </div>
  );
}
