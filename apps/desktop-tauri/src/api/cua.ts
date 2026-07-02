// src/api/cua.ts
//
// Calls the backend `/api/cua/trace/{session_id}` endpoint to read the
// recorded computer-use (CUA) action timeline. The data is appended
// per-action by hermix-cua's TraceRecorder and is the single source of
// truth for the "what did the agent actually click / type / press"
// view shown to the user in the Stage 4.2 TracePanel.
//
// The endpoint is cheap (tail-read 1 MiB of JSONL, server-side
// session filter), so the panel polls every few seconds while a
// session is active. A 404 / network error returns an empty
// timeline — the panel renders the empty-state copy in that case,
// not a toast, because "no actions yet" is a perfectly normal state
// between actions.

export interface TraceEvent {
  ts: string; // ISO-8601 UTC, e.g. "2026-07-02T12:34:56.789Z"
  kind:
    | "screenshot"
    | "screen_info"
    | "click"
    | "type_text"
    | "press_key"
    | "denied"
    | "timeout"
    | { other: { subkind: string } };
  session_id: string;
  summary: string;
  ok: boolean;
  elapsed_ms: number;
  error?: string;
}

export interface TraceResponse {
  session_id: string;
  events: TraceEvent[];
  total_rows: number;
  missing?: boolean;
  parse_errors?: boolean;
}

const POLL_INTERVAL_MS = 3000;

export async function fetchCuaTrace(sessionId: string): Promise<TraceResponse | null> {
  if (!sessionId.trim()) return null;
  try {
    const resp = await fetch(`/api/cua/trace/${encodeURIComponent(sessionId)}`, {
      method: "GET",
      headers: { Accept: "application/json" },
    });
    if (!resp.ok) return null;
    const data = (await resp.json()) as TraceResponse;
    return data;
  } catch {
    return null;
  }
}

/**
 * Subscribe to trace updates for a session. Returns an unsubscribe
 * function. The callback fires immediately with the first fetch and
 * then every `POLL_INTERVAL_MS` until the caller unsubscribes.
 *
 * Returns `null` if `sessionId` is empty — the consumer should
 * treat that as "render the empty-state copy, don't spin up a
 * poll loop".
 */
export function subscribeCuaTrace(
  sessionId: string,
  onUpdate: (resp: TraceResponse) => void,
): (() => void) | null {
  if (!sessionId.trim()) return null;

  let cancelled = false;
  const tick = async () => {
    if (cancelled) return;
    const resp = await fetchCuaTrace(sessionId);
    if (cancelled) return;
    if (resp) onUpdate(resp);
  };

  void tick();
  const handle = window.setInterval(() => {
    void tick();
  }, POLL_INTERVAL_MS);

  return () => {
    cancelled = true;
    window.clearInterval(handle);
  };
}