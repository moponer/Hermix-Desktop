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

// ── Context window resolution (前端 catalog 镜像 + 来源透明化) ─────
//
// 后端权威链路在 `crates/hermix-intelligence/src/model_metadata.rs::get_model_context_length_with`:
//   L1 providers.model_contexts[id] → L2 providers.context_window →
//   L3 known_models() + DEFAULT_CONTEXT_LENGTHS → L4 DEFAULT_FALLBACK_CONTEXT (128_000)
//
// 前端只做 best-effort 镜像用来在 UI 上展示「默认（N·来源）」，命中规则
// 跟后端 `DEFAULT_CONTEXT_LENGTHS` longest-key-first + contains 一致。
// 前端 catalog miss 时显示 128K·兜底，与后端 fallback 完全对齐。
export type ContextWindowSource =
  | "user"      // 用户在 select 里手动选了具体值（CONTEXT_WINDOW_OPTIONS 之一）
  | "probed"    // 后端 fetch_models 从 /v1/models 探测并持久化到 model_contexts
  | "catalog"   // 命中前端 catalog 镜像（known_models() / DEFAULT_CONTEXT_LENGTHS 镜像）
  | "fallback"; // 都 miss → 128K 兜底（与后端 DEFAULT_FALLBACK_CONTEXT 对齐）

export interface ContextWindowResolution {
  value: number;
  source: ContextWindowSource;
}

// 前端 catalog 镜像（mirror of DEFAULT_CONTEXT_LENGTHS in
// `crates/hermix-intelligence/src/model_metadata.rs:52-...`）。
// 规则：longest-key-first + contains 匹配（lowercase），保持与后端一致。
// 加新条目务必同步后端，反之亦然（避免 UI 显示「200K·catalog」但实际是 128K·兜底）。
const CATALOG_PREFIX_TABLE: ReadonlyArray<readonly [string, number]> = [
  // Claude 4.6 (1M)
  ["claude-opus-4-6", 1_000_000],
  ["claude-sonnet-4-6", 1_000_000],
  ["claude-opus-4.6", 1_000_000],
  ["claude-sonnet-4.6", 1_000_000],
  ["claude", 200_000],
  // OpenAI
  ["gpt-4.1", 1_047_576],
  ["gpt-5", 128_000],
  ["gpt-4", 128_000],
  // Google
  ["gemini", 1_048_576],
  ["gemma-4-31b", 256_000],
  ["gemma-3", 131_072],
  ["gemma", 8_192],
  // DeepSeek
  ["deepseek", 128_000],
  // Meta
  ["llama", 131_072],
  // Qwen
  ["qwen3-coder-plus", 1_000_000],
  ["qwen3-coder", 262_144],
  ["qwen", 131_072],
  // MiniMax
  ["minimax", 204_800],
  // GLM
  ["glm", 202_752],
  // xAI Grok
  ["grok-4-1-fast", 2_000_000],
  ["grok-4-fast", 2_000_000],
  ["grok-4.20", 2_000_000],
  ["grok-code-fast", 256_000],
  ["grok-4", 256_000],
  ["grok-3", 131_072],
  ["grok-2", 131_072],
  ["grok", 131_072],
  // Kimi / Moonshot
  ["kimi", 200_000],
  ["moonshot", 200_000],
  // 主人自营（hermix）专属 provider，命中大模型给个合理兜底
  ["agnes", 128_000],
];

const CATALOG_SORTED: ReadonlyArray<readonly [string, number]> =
  [...CATALOG_PREFIX_TABLE].sort((a, b) => b[0].length - a[0].length);

function lookupCatalog(modelId: string): number | null {
  const m = modelId.toLowerCase();
  for (const [key, value] of CATALOG_SORTED) {
    if (m.includes(key)) return value;
  }
  return null;
}

export const DEFAULT_FALLBACK_CONTEXT = 128_000;

/**
 * 解析「当前 model 的 context_window 实际值 + 来源」。
 *
 * 优先级（与后端 `get_model_context_length_with` 一致）：
 *   1. `userContext` > 0 → user（用户在 select 里选了具体值）
 *   2. `probedContext` > 0 → probed（后端 fetch_models 探测 + 持久化）
 *   3. 命中前端 catalog 镜像 → catalog
 *   4. 都 miss → fallback (128K)
 *
 * @param modelId 例如 "gpt-4o" / "agnes-2.5-flash"
 * @param userContext 来自 config.modelContexts[fullId]（用户在 select 选的值）
 * @param probedContext 来自 GET /api/custom-providers 的 model_contexts（已持久化探测值）
 */
export function resolveContextWindow(
  modelId: string,
  userContext: number | null | undefined,
  probedContext: number | null | undefined,
): ContextWindowResolution {
  if (typeof userContext === "number" && userContext > 0) {
    return { value: userContext, source: "user" };
  }
  if (typeof probedContext === "number" && probedContext > 0) {
    return { value: probedContext, source: "probed" };
  }
  const hit = lookupCatalog(modelId);
  if (hit !== null) {
    return { value: hit, source: "catalog" };
  }
  return { value: DEFAULT_FALLBACK_CONTEXT, source: "fallback" };
}

const SOURCE_LABEL: Record<ContextWindowSource, string> = {
  user: "已配置",
  probed: "探测",
  catalog: "内置",
  fallback: "兜底",
};

/**
 * 把 tokens 数渲染成「128K / 200K / 1M / 2M」短标签（用于 select option 文案）。
 */
export function formatContextWindow(tokens: number): string {
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return Number.isInteger(m) ? `${m}M` : `${m.toFixed(1)}M`;
  }
  if (tokens >= 1_000) {
    const k = Math.round(tokens / 1_000);
    return `${k}K`;
  }
  return `${tokens}`;
}

/**
 * 给 select 的默认 option 生成展示文案：「默认 (200K·catalog)」。
 */
export function defaultOptionLabel(resolution: ContextWindowResolution): string {
  return `默认 (${formatContextWindow(resolution.value)}·${SOURCE_LABEL[resolution.source]})`;
}

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
