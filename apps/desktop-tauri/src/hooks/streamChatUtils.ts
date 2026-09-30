import type { ChatOverrides } from "./useStreamChat";

/**
 * Pure helpers shared by the stream-chat sub-modules. No React, no
 * WebSocket — just data in, data out (or side-effect-free string
 * transforms). Kept dependency-free so they can be unit-tested and
 * reused from `useStreamParser` / `useWsConnection` without pulling
 * the whole hook closure.
 */

/**
 * Strip leaked `[activity: …]` / `[status: …]` markers from the final
 * assistant text. Defense-in-depth: old backends may have injected
 * these as Text events before the `lib.rs` fix was compiled. Applied
 * once at `settle` time so they never reach the persisted message.
 */
export function stripActivityMarkers(text: string): string {
  return text.replace(/\n?\[(?:activity|status):[^\]]*\]\s*/g, "");
}

/**
 * Client-side dedup key: a same-content re-send is coalesced only
 * while the session is actively streaming (see `send` in
 * `useStreamChat`). Combines session id + content so two different
 * sessions never collide.
 */
export function computeSendDedupHash(sessionId: string, content: string): string {
  return `${sessionId}:${content}`;
}

/**
 * Build the initial `connected` → WS payload from the user message and
 * the per-request `ChatOverrides`. Mirrors the backend's
 * `SendMessageRequest` extension fields. Only non-empty overrides are
 * serialized onto the frame.
 */
export function buildConnectedPayload(
  content: string,
  overrides: ChatOverrides,
): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    text: content,
    request_id: `req-${Date.now()}`,
  };
  if (overrides.workspace) payload.workspace = overrides.workspace;
  if (overrides.systemPromptOverride) payload.system_prompt = overrides.systemPromptOverride;
  if (overrides.tools && overrides.tools.length > 0) payload.tools = overrides.tools;
  // selectedModel (e.g. "minimax:MiniMax-M2.7") must reach the backend so
  // `effective_model` switches to the user's chosen provider; if unset, the
  // backend uses config.yaml's default.
  if (overrides.model) payload.model = overrides.model;
  if (overrides.personality) payload.personality = overrides.personality;
  if (overrides.permissionLevel) payload.permission = overrides.permissionLevel;
  if (overrides.skills && overrides.skills.length > 0) payload.skills = overrides.skills;
  if (overrides.image) payload.image = overrides.image;
  if (overrides.imageMimeType) payload.image_mime_type = overrides.imageMimeType;
  if (overrides.images && overrides.images.length > 0) payload.images = overrides.images;
  if (overrides.domainOverride) payload.domain_override = overrides.domainOverride;
  if (overrides.expertToolset) payload.expert_toolset = overrides.expertToolset;
  return payload;
}

/** Section union that the C3 marker protocol reports on. */
export type GateSection =
  | "identity"
  | "tool_list"
  | "tool_enforce"
  | "working_dir"
  | "timestamp";

export interface SectionGateNarrowed {
  patched: { section: GateSection; applied: boolean; reason: string }[];
  pending: { section: GateSection; applied: boolean; reason: string }[];
}

/**
 * Type-narrow the raw `section_gate` WS report into the shape
 * `onSectionGate` expects. Tolerates missing `/event`-nested envelopes
 * and coerces unknown section strings to `"identity"`. Returns null on
 * a malformed (non-object) report so the caller can skip it.
 */
export function narrowSectionGateReport(reportRaw: unknown): SectionGateNarrowed | null {
  if (!reportRaw || typeof reportRaw !== "object") return null;
  const report = reportRaw as {
    patched?: Array<{ section?: unknown; applied?: unknown; reason?: unknown }>;
    pending?: Array<{ section?: unknown; applied?: unknown; reason?: unknown }>;
  };
  const map = (
    arr?: Array<{ section?: unknown; applied?: unknown; reason?: unknown }>,
  ): { section: GateSection; applied: boolean; reason: string }[] =>
    Array.isArray(arr)
      ? arr.map((g) => ({
          section: (g?.section as GateSection) ?? "identity",
          applied: !!g?.applied,
          reason: typeof g?.reason === "string" ? g.reason : "",
        }))
      : [];
  return { patched: map(report.patched), pending: map(report.pending) };
}
