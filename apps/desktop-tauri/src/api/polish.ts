// src/api/polish.ts
//
// Calls the backend `/api/polish/{chat,automation}` endpoints (implemented at
// `crates/hermix-server/src/dashboard/polish.rs`, wired at `dashboard/mod.rs`).
// If the backend endpoint is unreachable or returns `null`, falls back to a
// no-op that only collapses whitespace, so the UI stays usable when the server
// isn't running.
//
// Transport: the stdio bridge (`fetchJSON`), same as every other REST call.
// A bare `fetch("/api/polish/chat")` cannot work in this app — the TCP era's
// host:port no longer exists (see api/rest.ts header), so the request always
// failed and the whitespace fallback silently masked a dead feature.
import { type ChatPolishInput } from "../prompts/polish.chat";
import { type AutomationPolishInput } from "../prompts/polish.automation";
import { type EnhanceInput } from "../prompts/polish.enhance";
import { fetchJSON } from "./rest";

interface PolishBackendResponse {
  ok: boolean;
  text: string;
  error?: string;
}

async function callPolishEndpoint(endpoint: string, payload: unknown): Promise<string | null> {
  // Backend routes are at /api/polish/{chat, automation} (dashboard/polish.rs).
  // Falls back to whitespace-only polish when the server is unreachable.
  try {
    const data = await fetchJSON<PolishBackendResponse>(endpoint, {
      method: "POST",
      body: JSON.stringify(payload),
    });
    if (data?.ok && typeof data.text === "string") return data.text;
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

/**
 * 增强提示词：一句模糊需求 → 结构化提示词。
 *
 * 与 polishChat 的降级策略**故意不同**：润色失败可以退回"折叠空白"
 * （顶多多几个空格，用户仍看得懂原话）；增强失败没有可退回的形态——
 * 退回原草稿等于"点了没反应"，退回空白等于"把用户的输入弄没了"。
 * 所以这里把错误抛出去，由调用方 toast 提示。
 */
export async function polishEnhance(input: EnhanceInput): Promise<string> {
  if (!input.draft.trim()) return "";

  const data = await fetchJSON<PolishBackendResponse>("/api/polish/enhance", {
    method: "POST",
    body: JSON.stringify({
      draft: input.draft,
      recentTurns: input.ctx?.recentTurns,
      hasAttachments: input.ctx?.hasAttachments,
    }),
  });

  if (!data?.ok || typeof data.text !== "string" || !data.text.trim()) {
    throw new Error(data?.error || "增强失败：后端没有返回结果");
  }
  return data.text;
}

// POLISH_PROMPTS_FOR_DEBUG 于 2026-10-18 移除（零调用）。提示词模板
// 单一真相源在 src/prompts/，调试直接看文件。
