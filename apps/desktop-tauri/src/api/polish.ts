// src/api/polish.ts
//
// Calls the backend `/api/polish/{chat,automation}` endpoints to rewrite user
// input. If the backend endpoint is unreachable or returns `null`, falls back
// to a no-op that only collapses whitespace, so the UI stays usable while the
// polish backend is being built out. See `docs/TODO.md` for the rollout plan.

import { CHAT_POLISH_PROMPT, type ChatPolishInput } from "../prompts/polish.chat";
import { AUTOMATION_POLISH_PROMPT, type AutomationPolishInput } from "../prompts/polish.automation";

interface PolishBackendResponse {
  ok: boolean;
  text: string;
  error?: string;
}

async function callPolishEndpoint(endpoint: string, payload: unknown): Promise<string | null> {
  // Polish backend not yet implemented (see `docs/TODO.md`): `fetch` returns
  // `null` whenever the endpoint is absent or errors, which the callers
  // interpret as "use the local whitespace-only fallback" below.
  try {
    const resp = await fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    if (!resp.ok) return null;
    const data = (await resp.json()) as PolishBackendResponse;
    if (data.ok && typeof data.text === "string") return data.text;
    return null;
  } catch {
    return null;
  }
}

export async function polishChat(input: ChatPolishInput): Promise<string> {
  const trimmed = input.draft.trim();
  if (!trimmed) return "";

  // 1) Try the backend first.
  const fromBackend = await callPolishEndpoint("/api/polish/chat", {
    draft: input.draft,
    ctx: input.ctx,
  });
  if (fromBackend) return fromBackend;

  // 2) Fallback: collapse whitespace only (the prompt template cannot be run
  // locally, but the caller still expects a non-empty string).
  return input.draft.replace(/\s+/g, " ").trim();
}

export async function polishAutomation(input: AutomationPolishInput): Promise<string> {
  const trimmed = input.draft.trim();
  if (!trimmed) return "";

  const fromBackend = await callPolishEndpoint("/api/polish/automation", {
    draft: input.draft,
    ctx: input.ctx,
  });
  if (fromBackend) return fromBackend;

  return input.draft.replace(/\s+/g, " ").trim();
}

// 暴露给上层做 telemetry / 调试
export const POLISH_PROMPTS_FOR_DEBUG = {
  chat: CHAT_POLISH_PROMPT,
  automation: AUTOMATION_POLISH_PROMPT,
} as const;
