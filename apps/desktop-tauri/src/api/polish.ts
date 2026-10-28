// src/api/polish.ts
//
// Calls the backend `/api/polish/{automation,enhance}` endpoints
// (implemented at `crates/hermix-server/src/dashboard/polish.rs`, wired at
// `dashboard/mod.rs`).
//
// Transport: the stdio bridge (`fetchJSON`), same as every other REST call.
// A bare `fetch("/api/polish/...")` cannot work in this app — the TCP era's
// host:port no longer exists (see api/rest.ts header).
import { type AutomationPolishInput } from "../prompts/polish.automation";
import { type EnhanceInput } from "../prompts/polish.enhance";
import { fetchJSON } from "./rest";

interface PolishBackendResponse {
  ok: boolean;
  text: string;
  error?: string;
}

/**
 * 增强单次请求的客户端预算。
 *
 * 增强是**非流式**的一整轮生成（150-600 字），默认的 10s 客户端死线比
 * 一次正常生成还短，慢源/冷启动必然先被前端掐断——用户看到的就是
 * 「request timed out after 10000ms」。这里与后端 `POLISH_TIMEOUT`（90s）
 * 对齐；stdio 桥本身给 120s，仍有余量。
 */
const ENHANCE_TIMEOUT_MS = 90_000;

export async function polishAutomation(input: AutomationPolishInput): Promise<string> {
  const trimmed = input.draft.trim();
  if (!trimmed) return "";

  try {
    const data = await fetchJSON<PolishBackendResponse>("/api/polish/automation", {
      method: "POST",
      body: JSON.stringify({
        draft: input.draft,
        ctx: input.ctx,
      }),
    });
    if (data?.ok && typeof data.text === "string") return data.text;
  } catch {
    /* 后端不可达 → 下面的本地兜底 */
  }

  // Fallback: collapse whitespace only — the prompt template can't run
  // locally, but the caller still expects a non-empty string.
  return input.draft.replace(/\s+/g, " ").trim();
}

/**
 * 增强提示词：一句模糊需求 → 结构化提示词。
 *
 * 与 polishAutomation 的降级策略**故意不同**：改写失败可以退回"折叠空白"
 * （顶多多几个空格，用户仍看得懂原话）；增强失败没有可退回的形态——
 * 退回原草稿等于"点了没反应"，退回空白等于"把用户的输入弄没了"。
 * 所以这里把错误抛出去，由调用方 toast 提示。
 */
export async function polishEnhance(input: EnhanceInput): Promise<string> {
  if (!input.draft.trim()) return "";

  const data = await fetchJSON<PolishBackendResponse>(
    "/api/polish/enhance",
    {
      method: "POST",
      body: JSON.stringify({
        draft: input.draft,
        model: input.model,
        recentTurns: input.ctx?.recentTurns,
        hasAttachments: input.ctx?.hasAttachments,
      }),
    },
    { timeoutMs: ENHANCE_TIMEOUT_MS },
  );

  if (!data?.ok || typeof data.text !== "string" || !data.text.trim()) {
    throw new Error(data?.error || "增强失败：后端没有返回结果");
  }
  return data.text;
}
