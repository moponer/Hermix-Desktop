/**
 * ACUI (Agent-Controlled UI) directive types.
 *
 * 镜像后端 `hermix_core::AcuiDirective`，通过 WS `StreamEvent::Acui`
 * 推送。协议层与渲染层解耦：这里只定义数据，前端的 AcuiRenderer
 * 按 `kind` 查注册表渲染对应组件。
 *
 * 协议格式（与后端 serde 一致）：
 *   { "op": "show",   "id": "...", "type": "...", "position": "after",  "props": {...}, "created_at": "..." }
 *   { "op": "update", "id": "...", "patch": {...} }
 *   { "op": "hide",   "id": "..." }
 */

/** Position of a card relative to the message text body. */
export type AcuiPosition = "before" | "after" | "replace";

/** Show directive — render a new card. */
export interface AcuiShow {
  op: "show";
  /** Unique id; subsequent update/hide target this id. */
  id: string;
  /** Card kind — looks up a React component in AcuiRegistry. */
  type: string;
  /** Position relative to message body. */
  position: AcuiPosition;
  /** Card props (any JSON, interpreted by the registered component). */
  props: unknown;
  /** Server-side creation timestamp (ISO 8601). Optional. */
  created_at?: string;
}

/** Update directive — patch an existing card. */
export interface AcuiUpdate {
  op: "update";
  id: string;
  /** JSON Merge Patch — shallow-merged into existing props. */
  patch: unknown;
}

/** Hide directive — remove a card. */
export interface AcuiHide {
  op: "hide";
  id: string;
}

/** Discriminated union of all three directive variants. */
export type AcuiDirective = AcuiShow | AcuiUpdate | AcuiHide;

/**
 * Decision outcome recorded locally when the user resolves an
 * approval/clarify card via the decision bus. The decision bus is
 * what acui components call to *send* the decision out to the
 * transport layer (WS `approval_response` / `clarify_response`).
 * The `applyAcuiDecision` reducer records the outcome into `state`
 * so the card can render its "已决定" footer even after a re-render.
 */
export type AcuiDecision =
  | { kind: "approved"; cardId: string; at: number }
  | { kind: "denied"; cardId: string; at: number }
  | { kind: "answered"; cardId: string; at: number; answer: string };

/** Outcome lookup — what `lookupAcuiOutcome` returns for a given card id. */
export type AcuiOutcome =
  | { resolved: false }
  | { resolved: true; kind: "approved" | "denied" }
  | { resolved: true; kind: "answered"; answer: string };

/**
 * Client-side state of all currently-displayed ACUI cards.
 *
 * Mirrors `hermix_core::AcuiState` plus a `decisions` log that lives
 * only on the client (no server equivalent — the server gets the
 * outcome via the WS response frames, not via acui).
 *
 * The frontend maintains one per chat session; new directives are
 * applied in order via `applyAcuiDirective`; user decisions are
 * applied via `applyAcuiDecision`.
 */
export interface AcuiState {
  cards: AcuiShow[];
  /** Append-only decision log. Used by `lookupAcuiOutcome` to render
   *  resolved footers without re-querying the transport. */
  decisions: AcuiDecision[];
}

export const emptyAcuiState: AcuiState = { cards: [], decisions: [] };

/** Apply a directive to the state. Returns a new state (immutable). */
export function applyAcuiDirective(
  state: AcuiState,
  directive: AcuiDirective,
): AcuiState {
  switch (directive.op) {
    case "show": {
      // Replace if same id, else append.
      const filtered = state.cards.filter((c) => c.id !== directive.id);
      return { cards: [...filtered, directive], decisions: state.decisions };
    }
    case "update": {
      return {
        cards: state.cards.map((c) => {
          if (c.id !== directive.id) return c;
          // Shallow merge props if both are plain objects.
          if (isPlainObject(c.props) && isPlainObject(directive.patch)) {
            return { ...c, props: { ...c.props, ...directive.patch } };
          }
          // Otherwise, replace.
          return { ...c, props: directive.patch };
        }),
        decisions: state.decisions,
      };
    }
    case "hide": {
      // Note: we deliberately keep the decision log so that, even if the
      // server hides a card after the user has decided, the resolved
      // footer can still be rendered if the same id re-appears.
      return { cards: state.cards.filter((c) => c.id !== directive.id), decisions: state.decisions };
    }
  }
}

/**
 * Apply a batch of directives (in order). Returns the final state.
 * Pure function: no mutation, no side effects.
 */
export function applyAcuiDirectives(
  state: AcuiState,
  directives: AcuiDirective[],
): AcuiState {
  let s = state;
  for (const d of directives) {
    s = applyAcuiDirective(s, d);
  }
  return s;
}

/**
 * Record a user decision into the state log. Pure / additive — does not
 * touch `cards` (the card's `resolved` field is already updated via the
 * `props.decision` / `props.answer` patch the server sends on success,
 * but the decision log is the local source of truth for "the user
 * pressed allow / deny / submit on this id").
 */
export function applyAcuiDecision(
  state: AcuiState,
  decision: AcuiDecision,
): AcuiState {
  // De-dupe: if the same id already has the same kind of decision
  // recorded, don't double-add. (Network retries / double-clicks.)
  const dup = state.decisions.some(
    (d) =>
      d.cardId === decision.cardId &&
      d.kind === decision.kind &&
      // For "answered", also match the exact answer; for approved/denied
      // the kind is sufficient.
      (d.kind !== "answered" ||
        (d as Extract<AcuiDecision, { kind: "answered" }>).answer ===
          (decision as Extract<AcuiDecision, { kind: "answered" }>).answer),
  );
  if (dup) return state;
  return { cards: state.cards, decisions: [...state.decisions, decision] };
}

/**
 * Look up the latest recorded outcome for a card id. Returns
 * `{ resolved: false }` if no decision has been logged yet.
 *
 * Used by acui adapters (approval/clarify) to render the
 * "已允许" / "已拒绝" / "已回答" footer instead of the live buttons.
 */
export function lookupAcuiOutcome(
  state: AcuiState,
  cardId: string,
): AcuiOutcome {
  // Walk from the end so the latest decision wins (in case of
  // approve→deny back-and-forth).
  for (let i = state.decisions.length - 1; i >= 0; i--) {
    const d = state.decisions[i];
    if (d.cardId !== cardId) continue;
    if (d.kind === "approved") return { resolved: true, kind: "approved" };
    if (d.kind === "denied") return { resolved: true, kind: "denied" };
    return { resolved: true, kind: "answered", answer: d.answer };
  }
  return { resolved: false };
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}