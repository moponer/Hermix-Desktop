// src/api/polish.ts
//
// Calls the backend `/api/polish/{chat,automation}` endpoints (implemented at
// `crates/hermix-server/src/dashboard/polish.rs`, wired at `dashboard/mod.rs`).
// If the backend endpoint is unreachable or returns `null`, falls back to a
// no-op that only collapses whitespace, so the UI stays usable when the server
// isn't running.

import { CHAT_POLISH_PROMPT, type ChatPolishInput } from "../prompts/polish.chat";
import { AUTOMATION_POLISH_PROMPT, type AutomationPolishInput } from "../prompts/polish.automation";

interface PolishBackendResponse {
  ok: boolean;
  text: string;
  error?: string;
}

async function callPolishEndpoint(endpoint: string, payload: unknown): Promise<string | null> {
  // Backend routes are at /api/polish/{chat, automation} (dashboard/polish.rs).
  // Falls back to whitespace-only polish when the server is unreachable.
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

  // Try the backend first.
  const fromBackend = await callPolishEndpoint("/api/polish/chat", {
    draft: input.draft,
    ctx: input.ctx,
  });
  if (fromBackend) return fromBackend;

  // Fallback: collapse whitespace only — the prompt template can't run
  // locally, but the caller still expects a non-empty string.
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
