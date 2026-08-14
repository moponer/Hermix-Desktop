/**
 * AcuiRenderer — renders a list of currently-active ACUI cards.
 *
 * Inputs:
 *   - `state` — AcuiState (the result of `applyAcuiDirectives` over the
 *     session's WS event history). The renderer is intentionally
 *     stateless w.r.t. event ingestion; ChatView owns the state
 *     evolution and feeds in the final shape.
 *   - `registry` — AcuiRegistry mapping `kind` → React component.
 *   - `decide` — AcuiDecisionBus wired by the caller (useAcuiState.bind).
 *   - `lookupOutcome` — optional (cardId → outcome) provider; defaults
 *     to a lookup against `state.decisions`.
 *
 * Behavior:
 *   - Groups cards by `position` (before / after / replace) and renders
 *     them in show-order within each group.
 *   - Unknown kind → falls back to `AcuiFallback` (raw JSON block).
 *   - Passes `decide` and `outcome` to each component so approval /
 *     clarify cards route their decisions through the bus.
 *
 * The renderer does NOT decide where in the chat stream the cards go —
 * that's the caller's responsibility (typically MessageBubble or ChatView).
 * It just produces the rendered DOM in three buckets that the caller
 * can splice into its own layout.
 */

import * as React from "react";
import type { AcuiState, AcuiOutcome } from "./types";
import { lookupAcuiOutcome } from "./types";
import { AcuiRegistry, AcuiFallback, type AcuiDecisionBus } from "./AcuiRegistry";

export interface AcuiRenderBuckets {
  /** Cards positioned "before" the message body. */
  before: React.ReactNode[];
  /** Cards positioned "after" the message body. */
  after: React.ReactNode[];
  /** Cards that replace the message body. If any, the caller should
   *  suppress the message body's normal rendering. */
  replace: React.ReactNode[];
}

export interface AcuiRendererProps {
  state: AcuiState;
  registry?: AcuiRegistry;
  decide?: AcuiDecisionBus;
  /** Optional custom outcome resolver. Defaults to
   *  `lookupAcuiOutcome(state, id)`. Useful for tests that want to
   *  inject a fixed decision log without wiring useAcuiState. */
  lookupOutcome?: (cardId: string) => AcuiOutcome;
  /**
   * Stream-completion flag — defaults to `true` so callers that
   * don't know about it (tests, settled renders) keep the legacy
   * behaviour of marking stranded groups as failed. Pass `false`
   * from `<AcuiPanel>` while a turn is still streaming so the
   * `tool_event` ACUI doesn't flash a false "失败 / 执行已中断"
   * before the slow tool's real `tool_complete` arrives.
   */
  streamComplete?: boolean;
}

export function renderAcuiCards({
  state,
  registry,
  decide,
  lookupOutcome,
  streamComplete = true,
}: AcuiRendererProps): AcuiRenderBuckets {
  const r = registry ?? AcuiRegistry.defaults();
  const outcomeOf =
    lookupOutcome ?? ((id: string) => lookupAcuiOutcome(state, id));
  const buckets: AcuiRenderBuckets = { before: [], after: [], replace: [] };

  for (const show of state.cards) {
    const Component = r.resolve(show.type) ?? AcuiFallback;
    const outcome = outcomeOf(show.id);
    const node = (
      <Component
        key={show.id}
        show={show}
        decide={decide}
        outcome={outcome}
        streamComplete={streamComplete}
      />
    );
    switch (show.position) {
      case "before":
        buckets.before.push(node);
        break;
      case "after":
        buckets.after.push(node);
        break;
      case "replace":
        buckets.replace.push(node);
        break;
    }
  }

  return buckets;
}

export type { AcuiShow, AcuiPosition } from "./types";