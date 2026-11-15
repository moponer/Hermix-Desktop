/**
 * useAcuiState — React state hook wrapping `AcuiState` evolution.
 *
 * The caller wires this into a streaming source (e.g. `useStreamChat`'s
 * `onAcuiDirective` callback) and the hook takes care of folding
 * incoming directives into the active card set.
 *
 * The hook also exposes:
 *  - `applyDecision` — record an approval/deny/answer outcome into
 *    the local decision log (for instant "已决定" footer rendering).
 *  - `buildDecisionBus` — produces an `AcuiDecisionBus` closure bound
 *    to this state. Each call to `bus.approve(id)` records a decision
 *    AND calls the caller-provided transport callback (if wired).
 *  - `bind` — convenience: registers three transport callbacks (one
 *    each for approve / deny / answer) and returns a `bus`. The bus
 *    can be passed to `<AcuiPanel state={state} decide={bus} />` so
 *    every card inside can route its decisions back through the
 *    hook's local state AND through to the WS transport.
 *
 * Usage:
 *   const acui = useAcuiState();
 *   const bus = acui.bind({
 *     approve: (id) => wsSend({ type: "approval_response", request_id: id, decision: "approved" }),
 *     deny:    (id) => wsSend({ type: "approval_response", request_id: id, decision: "denied" }),
 *     answer:  (id, a) => wsSend({ type: "clarify_response",  request_id: id, answer: a }),
 *   });
 *   ...
 *   // In streaming source:
 *   send(sessionId, text, {
 *     onAcuiDirective: acui.apply,
 *   });
 *   // In render tree:
 *   <AcuiPanel state={acui.state} decide={bus} />
 */

import { useCallback, useMemo, useRef, useState } from "react";
import {
  applyAcuiDirective,
  applyAcuiDecision,
  emptyAcuiState,
  type AcuiDecision,
  type AcuiDirective,
  type AcuiState,
} from "./types";
import type { AcuiDecisionBus } from "./AcuiRegistry";

export interface UseAcuiState {
  /** Current state. Pass to <AcuiPanel state={state} />. */
  state: AcuiState;
  /** Stable identity across renders (safe to use in dependency arrays). */
  apply: (directive: AcuiDirective) => void;
  /** Applies directives in order. */
  applyBatch: (directives: AcuiDirective[]) => void;
  /** Records a user decision into the local decision log (drives instant footer rendering). */
  applyDecision: (decision: AcuiDecision) => void;
  /** Reset to empty (e.g. when switching sessions). */
  reset: () => void;
  /** Build a decision bus from caller-provided transport callbacks. */
  bind: (transport: {
    approve?: (cardId: string) => void;
    deny?: (cardId: string) => void;
    answer?: (cardId: string, answer: string) => void;
  }) => AcuiDecisionBus;
  /** Live stats for the panel header (counts). */
  stats: AcuiStats;
}

export interface AcuiStats {
  /** Total cards currently in the stack (show not hidden). */
  total: number;
  /** Cards with a recorded user decision (approved/denied/answered). */
  resolved: number;
  /** Cards still waiting on a user decision. */
  pending: number;
  /** Decisions recorded in this session. */
  decisionCount: number;
}

export function useAcuiState(): UseAcuiState {
  const [state, setState] = useState<AcuiState>(emptyAcuiState);

  const apply = useCallback((directive: AcuiDirective) => {
    setState((prev) => applyAcuiDirective(prev, directive));
  }, []);

  const applyBatch = useCallback((directives: AcuiDirective[]) => {
    setState((prev) => {
      let next = prev;
      for (const d of directives) {
        next = applyAcuiDirective(next, d);
      }
      return next;
    });
  }, []);

  const applyDecisionInternal = useCallback((decision: AcuiDecision) => {
    setState((prev) => applyAcuiDecision(prev, decision));
  }, []);

  const reset = useCallback(() => {
    setState(emptyAcuiState);
  }, []);

  // The transport callbacks are typically stable (defined in the
  // renderer's parent component), but to keep `bind` cheap and not
  // re-create a new bus object on every state change, we cache the
  // last-seen transport triple and only rebuild when one of them
  // changes by reference.
  const transportRef = useRef<{
    approve?: (id: string) => void;
    deny?: (id: string) => void;
    answer?: (id: string, a: string) => void;
  }>({});
  const busRef = useRef<AcuiDecisionBus | null>(null);

  const bind = useCallback(
    (transport: {
      approve?: (id: string) => void;
      deny?: (id: string) => void;
      answer?: (id: string, a: string) => void;
    }): AcuiDecisionBus => {
      const t = transportRef.current;
      if (
        busRef.current &&
        t.approve === transport.approve &&
        t.deny === transport.deny &&
        t.answer === transport.answer
      ) {
        return busRef.current;
      }
      transportRef.current = transport;
      const bus: AcuiDecisionBus = {
        approve: transport.approve
          ? (id: string) => {
              applyDecisionInternal({ kind: "approved", cardId: id, at: Date.now() });
              transport.approve?.(id);
            }
          : undefined,
        deny: transport.deny
          ? (id: string) => {
              applyDecisionInternal({ kind: "denied", cardId: id, at: Date.now() });
              transport.deny?.(id);
            }
          : undefined,
        answer: transport.answer
          ? (id: string, answer: string) => {
              applyDecisionInternal({ kind: "answered", cardId: id, at: Date.now(), answer });
              transport.answer?.(id, answer);
            }
          : undefined,
      };
      busRef.current = bus;
      return bus;
    },
    [applyDecisionInternal],
  );

  // O(cards + decisions) — cheap enough to recompute on every state change.
  const stats = useMemo<AcuiStats>(() => {
    const decisionIds = new Set(state.decisions.map((d) => d.cardId));
    return {
      total: state.cards.length,
      resolved: state.cards.filter((c) => decisionIds.has(c.id)).length,
      pending: state.cards.filter((c) => !decisionIds.has(c.id)).length,
      decisionCount: state.decisions.length,
    };
  }, [state]);

  return { state, apply, applyBatch, applyDecision: applyDecisionInternal, reset, bind, stats };
}