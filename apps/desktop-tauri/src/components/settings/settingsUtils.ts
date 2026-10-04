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

export interface ModelConfigData {
  enabled: { providerId: string; modelId: string }[];
  defaultModel: string;
  modelContexts: Record<string, number>;
  /**
   * 用户显式移除过的模型（`provider:model` 全量 ID）。厂商 `/v1/models`
   * 是全量目录，被移除的模型必然仍在目录里——没有墓碑的话，每次进入
   * 设置页的自动合并都会把刚删的模型原样加回（"删除后刷新还在"）。
   * 仅存在于前端 localStorage，不随 `PUT /api/model/list` 上送。
   */
  dismissed?: string[];
}

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

// ── 对话模型判定 / 默认模型挑选 ─────────────────────
//
// 厂商 `/v1/models` 返回的是**全量目录**，含 embedding / rerank / tts /
// 图像模型。此前「获取模型」直接取 `models.sort()[0]` 当默认模型，于是
// OpenAI 会选中 `babbage-002`、硅基流动会选中 `BAAI/bge-*` —— 配完点
// 聊天直接报错，而后端 `auto_correct_model` 救不了（它只判 provider 有
// 没有 key，不判模型能不能对话）。
//
// 判定改为「黑名单排除」，未知模型名一律放行（宁可选错也别漏掉新模型）。

const NON_CHAT_MODEL_PATTERN =
  /(embedding|embed-|-embed|rerank|tts|whisper|audio|speech|voice|dall-e|image|moderation|bge-|gte-|m3e|bce-|clip|sdxl|stable-diffusion|flux|kolors|wanx|cosyvoice|sambert|paraformer|sensevoice|nomic|babbage|davinci|curie)/i;

/** 该模型是否可作为对话模型自动启用 / 自动设为默认。 */
export function isChatModel(modelId: string): boolean {
  return !NON_CHAT_MODEL_PATTERN.test(modelId);
}

/**
 * 挑一个 provider 的默认模型。
 *
 * 优先级：
 *   1. `providerDefault`（后端 `default_model_for_provider` 的人工清单，
 *      如 openai→gpt-4o、siliconflow→Qwen/Qwen2.5-72B-Instruct）—— 只要该模型
 *      确实在可用列表里就采信，不受下面的启发式过滤影响（后端清单是权威）
 *   2. 该 provider 探测到的对话模型中字典序第一个
 *   3. `null` —— 一个对话模型都没有（例如只提供 embedding 的端点）。
 *      **不回退到非对话模型**：那会让默认值指向一个根本不能聊天的模型，
 *      还会与"只自动启用对话模型"的 enabled 列表自相矛盾。调用方应提示手选。
 *
 * @param providerDefault 后端下发的权威默认（`GET /api/providers/meta`）
 */
export function pickDefaultModel(
  models: string[],
  providerDefault?: string,
): string | null {
  if (providerDefault) {
    const exact = models.find((m) => m === providerDefault);
    if (exact) return exact;
    const ci = models.find((m) => m.toLowerCase() === providerDefault.toLowerCase());
    if (ci) return ci;
  }
  const chat = models.filter(isChatModel);
  return chat.length > 0 ? chat[0] : null;
}

// ── Context window resolution (后端权威解析 + 来源透明化) ─────
//
// 后端拥有 catalog，前端不再镜像。解析在
// `crates/hermix-intelligence/src/model_metadata.rs::get_model_context_length_with_source`:
//   L1 providers.model_contexts[id]  → probed
//   L2 providers.context_window      → provider
//   L3 known_models() + DEFAULT_CONTEXT_LENGTHS → catalog
//   L4 DEFAULT_FALLBACK_CONTEXT (128_000)       → fallback
//
// 前端通过 `POST /api/models/context-windows` 批量取回 {value, source}，
// 只负责渲染。此前前端手抄了整张 `DEFAULT_CONTEXT_LENGTHS`，两份必然漂移。
export type ContextWindowSource =
  | "user"      // 用户在 select 里手动选了具体值（CONTEXT_WINDOW_OPTIONS 之一）
  | "probed"    // 后端 fetch_models 从 /v1/models 探测并持久化到 model_contexts
  | "provider"  // provider 级默认（LlmProviderConfig.context_window）
  | "catalog"   // 命中后端静态 catalog
  | "fallback"; // 后端未命中 / 未应答 → 128K 兜底

/** 后端 `POST /api/models/context-windows` 返回的单项。 */
export interface ResolvedContextWindow {
  value: number;
  /** `"probed"` | `"provider"` | `"catalog"` | `"fallback"` */
  source: string;
}

export interface ContextWindowResolution {
  value: number;
  source: ContextWindowSource;
}

export const DEFAULT_FALLBACK_CONTEXT = 128_000;

/**
 * 解析「当前 model 的 context_window 实际值 + 来源」。
 *
 * 后端拥有 catalog（`get_model_context_length_with_source`），前端不再镜像：
 *   1. `userContext` > 0 → user（用户在 select 里选了具体值）
 *   2. 后端返回的 {value, source} → probed / provider / catalog / fallback
 *   3. 后端未应答（旧版本 / 请求未完成）→ fallback (128K)，不再本地编造
 *
 * @param userContext 来自 config.modelContexts[fullId]（用户在 select 选的值）
 * @param backend `POST /api/models/context-windows` 对 fullId 的解析结果
 */
export function resolveContextWindow(
  userContext: number | null | undefined,
  backend: ResolvedContextWindow | null | undefined,
): ContextWindowResolution {
  if (typeof userContext === "number" && userContext > 0) {
    return { value: userContext, source: "user" };
  }
  if (backend && typeof backend.value === "number" && backend.value > 0) {
    return { value: backend.value, source: normalizeSource(backend.source) };
  }
  return { value: DEFAULT_FALLBACK_CONTEXT, source: "fallback" };
}

/** 后端 source 字符串 → 前端联合类型；未知值按 fallback 处理。 */
function normalizeSource(raw: string | undefined): ContextWindowSource {
  switch (raw) {
    case "probed":
    case "provider":
    case "catalog":
      return raw;
    default:
      return "fallback";
  }
}

const SOURCE_LABEL: Record<ContextWindowSource, string> = {
  user: "已配置",
  probed: "探测",
  provider: "厂商默认",
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

/** `model_found === false` 时的说明文案（列表行与添加/编辑弹窗共用）。
 *
 *  放在这里而不是任一组件里：ModelListTab 依赖 ModelAddDialog，反向再从
 *  ModelListTab 取东西会形成循环导入。
 *
 *  只说「不在列表里」用户没法自查 —— 他会怀疑是检测坏了，而真相往往是
 *  「厂商目录里根本没有这个名字」（拿平台侧的模型 id 去问厂商 API 很常见，
 *  比如把 MCode 里的 `minimax/MiniMax-M3.1-Flash-Preview` 填进 hermix 的
 *  MiniMax 供应商）。后端本来就把 /models 返回的原始 id 带回来了，直接摊开
 *  给他看，一眼就能分辨是名字不对还是端点不对。厂商目录通常只有几条到十几条，
 *  摊开不占地方。 */
export function catalogMismatchNote(models: string[] | null | undefined): string {
  const got = models ?? [];
  if (got.length === 0) return " · 厂商 /v1/models 目录未列此模型（不影响使用）";
  const shown = got.slice(0, 6).join("、");
  const more = got.length > 6 ? ` 等 ${got.length} 个` : "";
  return ` · 厂商 /v1/models 目录未列此模型，实际返回：${shown}${more}`;
}
