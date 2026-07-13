/* ════════════════════════════════════════════════════════════════
   Provider config — single source of truth for LLM provider base URLs
   and their corresponding API key env-var names. Used by both
   AIGenerateWizard (runGeneration) and TestDialog (send) to auto-detect
   the active LLM provider and resolve the matching API key.

   Previously duplicated in AIGenerateWizard and TestDialog (10 entries
   each, ~50 lines of literal copy-paste). Collapsed here.
   ════════════════════════════════════════════════════════════════ */

import { API_BASE } from "../../api";
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
 */
export async function resolveProviderConfig(): Promise<ProviderConfig> {
  let apiUrl = storage.getItem(STORAGE_KEYS.agentsServerUrl) || "";
  let apiKey = storage.getItem(STORAGE_KEYS.agentsApiKey) || "";

  // If no custom server URL is set (or it points back at the local backend),
  // auto-detect from backend model config.
  if (!apiUrl || apiUrl === API_BASE) {
    try {
      const modelResp = await fetch(`${API_BASE}/api/model/info`);
      if (modelResp.ok) {
        const modelInfo = await modelResp.json() as { provider?: string };
        const provider = modelInfo.provider || "openai";
        apiUrl = PROVIDER_URLS[provider] || PROVIDER_URLS.openai;

        const envKey = PROVIDER_ENV_KEYS[provider] || PROVIDER_ENV_KEYS.openai;
        const revealResp = await fetch(`${API_BASE}/api/env/reveal`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ key: envKey }),
        });
        if (revealResp.ok) {
          const revealData = await revealResp.json() as { value?: string };
          if (revealData.value) apiKey = revealData.value;
        }
      }
    } catch {
      // Auto-detect failed — fall back to whatever the user stored.
    }
  }

  return { apiUrl, apiKey };
}