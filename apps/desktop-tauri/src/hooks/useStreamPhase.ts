import type { StreamPhase } from "./useStreamChat";

/**
 * StreamPhase state machine. The `useStreamChat` `send` closure used to
 * keep `phase` as a bare `let` and an inline `updatePhase` helper; this
 * factory extracts that into a self-contained controller so the phase
 * transitions can be reasoned about (and tested) independently of the
 * giant WS handler.
 *
 * Transitions:
 *   idle → connecting → streaming → (slow | completed | error | timeout | partial)
 *
 * `updatePhase` is the single write path: it advances the internal
 * phase AND notifies the caller via `onPhaseChange` so the UI can drive
 * the phase-aware status chip. `getPhase` lets the WS handler read the
 * live phase without prop-drilling the closure variable.
 */
export interface PhaseController {
  updatePhase: (next: StreamPhase) => void;
  getPhase: () => StreamPhase;
  reset: () => void;
}

export function createPhaseController(
  onPhaseChange?: (phase: StreamPhase) => void,
): PhaseController {
  let phase: StreamPhase = "idle";
  return {
    updatePhase: (next: StreamPhase) => {
      phase = next;
      onPhaseChange?.(next);
    },
    getPhase: () => phase,
    reset: () => {
      phase = "idle";
    },
  };
}
