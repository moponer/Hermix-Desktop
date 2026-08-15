import { storage, STORAGE_KEYS } from "../../utils/storageCompat";

// ── Provider definitions ───────────────────────────

export interface ProviderDef {
  id: string;
  name: string;
  baseUrl: string;
  keyEnvVar: string;
  description: string;
}

export const PROVIDERS: ProviderDef[] = [
  // ── 国际厂商
  { id: "openai", name: "OpenAI", baseUrl: "https://api.openai.com/v1", keyEnvVar: "OPENAI_API_KEY", description: "GPT-4o、GPT-4o-mini 等" },
  { id: "anthropic", name: "Anthropic", baseUrl: "https://api.anthropic.com/v1", keyEnvVar: "ANTHROPIC_API_KEY", description: "Claude 3.5/3.7 等" },
  { id: "openrouter", name: "OpenRouter", baseUrl: "https://openrouter.ai/api/v1", keyEnvVar: "OPENROUTER_API_KEY", description: "多模型聚合路由" },
  { id: "siliconflow", name: "硅基流动", baseUrl: "https://api.siliconflow.cn/v1", keyEnvVar: "SILICONFLOW_API_KEY", description: "开源模型托管服务" },
  { id: "copilot", name: "GitHub Copilot", baseUrl: "https://api.github.com/copilot", keyEnvVar: "GITHUB_COPILOT_TOKEN", description: "通过 GitHub 账户使用" },
  { id: "nous", name: "Nous Research", baseUrl: "https://inference-nous.nousresearch.com/v1", keyEnvVar: "NOUS_API_KEY", description: "Hermes 系列开源模型" },
  { id: "minimax", name: "MiniMax", baseUrl: "https://api.minimax.chat/v1", keyEnvVar: "MINIMAX_API_KEY", description: "MiniMax-M1/M2 等模型" },
  // ── 国内厂商（按字母序）
  { id: "baichuan", name: "百川", baseUrl: "https://api.baichuan-ai.com/v1", keyEnvVar: "BAICHUAN_API_KEY", description: "百川系列大模型" },
  { id: "dashscope", name: "通义千问", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", keyEnvVar: "DASHSCOPE_API_KEY", description: "Qwen 系列模型" },
  { id: "deepseek", name: "DeepSeek", baseUrl: "https://api.deepseek.com/v1", keyEnvVar: "DEEPSEEK_API_KEY", description: "DeepSeek-V3、DeepSeek-R1 等" },
  { id: "kimi", name: "Kimi", baseUrl: "https://api.moonshot.cn/v1", keyEnvVar: "MOONSHOT_API_KEY", description: "Moonshot 系列模型" },
  { id: "qwen", name: "Qwen (DashScope)", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", keyEnvVar: "DASHSCOPE_API_KEY", description: "Qwen 兼容别名" },
  { id: "spark", name: "讯飞星火", baseUrl: "https://spark-api-open.xf-yun.com/v3.5/chat", keyEnvVar: "SPARK_API_KEY", description: "Spark V3.5 / V4.0" },
  { id: "tencent", name: "腾讯混元", baseUrl: "https://hunyuan.tencent.com/v1", keyEnvVar: "HUNYUAN_API_KEY", description: "hunyuan-pro / standard" },
  { id: "volcengine", name: "火山引擎", baseUrl: "https://ark.cn-beijing.volces.com/api/v3", keyEnvVar: "ARK_API_KEY", description: "豆包等模型" },
  { id: "yi", name: "零一万物", baseUrl: "https://api.lingyiwanwu.com/v1", keyEnvVar: "YI_API_KEY", description: "Yi-Lightning / Yi-Large" },
  { id: "zhipu", name: "智谱", baseUrl: "https://open.bigmodel.cn/api/paas/v4", keyEnvVar: "ZHIPU_API_KEY", description: "GLM-4 系列模型" },
];

// ── Model config persistence ───────────────────────

export const MODEL_CONFIG_KEY = STORAGE_KEYS.modelConfig;
export const MODEL_CACHE_KEY = STORAGE_KEYS.modelCache;

export interface ModelConfigData { enabled: { providerId: string; modelId: string }[]; defaultModel: string; modelContexts: Record<string, number> }

export function loadModelConfig(): ModelConfigData {
  try {
    const r = storage.getItem(MODEL_CONFIG_KEY);
    if (r) return { modelContexts: {}, ...JSON.parse(r) };
  } catch {}
  return { enabled: [], defaultModel: "", modelContexts: {} };
}

export function loadModelCache(): Record<string, string[]> {
  try {
    const r = storage.getItem(MODEL_CACHE_KEY);
    if (r) return JSON.parse(r);
  } catch {}
  return {};
}

export function saveModelCache(cache: Record<string, string[]>) {
  storage.setItem(MODEL_CACHE_KEY, JSON.stringify(cache));
}

export const CONTEXT_WINDOW_OPTIONS = [
  { label: "128K", value: 128_000 },
  { label: "200K", value: 200_000 },
  { label: "1M", value: 1_000_000 },
] as const;

// ── Advanced config ────────────────────────────────

export interface AdvancedConfig {
  // Compression
  min_summary_tokens: number;
  summary_tokens_ceiling: number;
  // Sub-agent
  sub_agent_timeout_secs: number;
  sub_agent_min_turns: number;
  sub_agent_max_turns_cap: number;
  delegation_max_iterations: number;
}

export const DEFAULT_ADVANCED_CONFIG: AdvancedConfig = {
  min_summary_tokens: 2000,
  summary_tokens_ceiling: 12000,
  sub_agent_timeout_secs: 300,
  sub_agent_min_turns: 2,
  sub_agent_max_turns_cap: 12,
  delegation_max_iterations: 50,
};

// ── Python runtime probe ───────────────────────────

export interface PythonProbeEntry {
  source: string;
  path: string;
  exists: boolean;
  version: string | null;
}

export interface PythonProbeResult {
  entries: PythonProbeEntry[];
  selected: string | null;
}
