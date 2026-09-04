/* ════════════════════════════════════════════════════════════════
   Provider config — single source of truth for LLM provider base URLs
   and their corresponding API key env-var names. Used by both
   AIGenerateWizard (runGeneration) and TestDialog (send) to auto-detect
   the active LLM provider and resolve the matching API key.

   Previously duplicated in AIGenerateWizard and TestDialog (10 entries
   each, ~50 lines of literal copy-paste). Collapsed here.
   ════════════════════════════════════════════════════════════════ */

import { fetchJSON } from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";

export const PROVIDER_URLS: Record<string, string> = {
  openai: "https://api.openai.com",
  anthropic: "https://api.anthropic.com",
  openrouter: "https://openrouter.ai/api",
  deepseek: "https://api.deepseek.com",
  dashscope: "https://dashscope.aliyuncs.com/compatible-mode",
  zhipu: "https://open.bigmodel.cn/api/paas/v4",
  moonshot: "https://api.moonshot.cn",
  minimax: "https://api.minimax.chat",
  siliconflow: "https://api.siliconflow.cn",
  volcengine: "https://ark.cn-beijing.volces.com/api/v3",
};

export const PROVIDER_ENV_KEYS: Record<string, string> = {
  openai: "OPENAI_API_KEY",
  anthropic: "ANTHROPIC_API_KEY",
  openrouter: "OPENROUTER_API_KEY",
  deepseek: "DEEPSEEK_API_KEY",
  dashscope: "DASHSCOPE_API_KEY",
  zhipu: "ZHIPUAI_API_KEY",
  moonshot: "MOONSHOT_API_KEY",
  minimax: "MINIMAX_API_KEY",
  siliconflow: "SILICONFLOW_API_KEY",
  volcengine: "ARK_API_KEY",
};

export interface ProviderConfig {
  apiUrl: string;
  apiKey: string;
}

/**
 * Resolve the LLM provider baseUrl + apiKey the agent should call.
 *
 *   1. If the user has explicitly configured `agentsServerUrl`, use it.
 *   2. Otherwise ask the backend `/api/model/info` for the active provider,
 *      then look up its base URL and ask `/api/env/reveal` for the matching
 *      API key (so user keys stay in backend env, never shipped to the UI).
 *
 * Returns `{ apiUrl, apiKey }` — caller passes both to its chat-completions
 * fetch. On any failure returns the user's stored values (may be empty).
 *
 * Stdio transport: backend calls go through `fetchJSON` (which is the
 * `invoke("backend_http_request")` chokepoint). The local-backend
 * auto-detect path used to compare the user-supplied `apiUrl` against
 * the old `API_BASE` sentinel ("http://127.0.0.1:3000"); that sentinel
 * is gone now, so the comparison drops — anything the user explicitly
 * stored is honoured, otherwise we auto-detect.
 */
export async function resolveProviderConfig(): Promise<ProviderConfig> {
  const userUrl = storage.getItem(STORAGE_KEYS.agentsServerUrl) || "";
  let apiUrl = userUrl;
  let apiKey = storage.getItem(STORAGE_KEYS.agentsApiKey) || "";

  if (!apiUrl) {
    try {
      const modelInfo = await fetchJSON<{ provider?: string }>("/api/model/info");
      const provider = modelInfo.provider || "openai";
      apiUrl = PROVIDER_URLS[provider] || PROVIDER_URLS.openai;

      const envKey = PROVIDER_ENV_KEYS[provider] || PROVIDER_ENV_KEYS.openai;
      const revealData = await fetchJSON<{ value?: string }>("/api/env/reveal", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: envKey }),
      });
      if (revealData.value) apiKey = revealData.value;
    } catch {
      // Auto-detect failed — fall back to whatever the user stored.
    }
  }

  return { apiUrl, apiKey };
}
