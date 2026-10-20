import { useState, useEffect, useMemo } from "react";
import { useCurrentModel, MODEL_CONFIG_CHANGED_EVENT } from "../hooks/useCurrentModel";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { getModelList, getProviderMeta, type ModelListEntryInfo } from "../api";
import { PROVIDERS } from "./settings/settingsUtils";

/** 分组标题用的供应商名：自定义 provider 用 display_name，内置厂商用品牌名
 *  （`minimax` → `MiniMax`），都没有才退回 id。 */
function providerLabel(pid: string, info?: ModelListEntryInfo): string {
  if (info?.provider_name && info.provider_name !== pid) return info.provider_name;
  return PROVIDERS.find((p) => p.id === pid)?.name ?? pid;
}

/** `192000` → `"192K"`、`1048576` → `"1M"`。未知返回 null（UI 直接不显示）。 */
export function formatContextTokens(tokens?: number): string | null {
  if (!tokens || tokens <= 0) return null;
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return `${Number.isInteger(m) ? m : m.toFixed(1)}M`;
  }
  if (tokens >= 1000) return `${Math.round(tokens / 1000)}K`;
  return String(tokens);
}

/**
 * 模型 id → 展示名。免费池的 id 形如 `minimax/minimax-m3.1-flash:free`，
 * 直接把整串塞进选择卡片会读成一串路径；去掉厂商前缀与 `:free` 后缀即可，
 * 是否免费由「免费」标记表达，不必写在名字里。
 */
export function friendlyModelName(modelId: string): string {
  const bare = modelId.replace(/:free$/i, "");
  const tail = bare.includes("/") ? bare.slice(bare.lastIndexOf("/") + 1) : bare;
  return tail || modelId;
}

/** 静态兜底清单——权威来源是后端 /api/providers/meta（BUILTIN_PROVIDER_ENV_VARS），
 *  拉取失败时才落到这里。禁止在此再增厂商：改 hermix-config。 */
export const BUILTIN_PROVIDER_IDS = new Set([
  "minimax", "openai", "deepseek", "dashscope", "kimi",
  "siliconflow", "openrouter", "zhipu", "volcengine",
  "anthropic", "copilot", "qwen", "moonshot",
  "nous", "baichuan", "tencent", "spark", "yi",
]);

const MODEL_CONFIG_KEY = STORAGE_KEYS.modelConfig;
interface ModelConfigData { enabled: { providerId: string; modelId: string }[]; defaultModel: string }

/** 选择卡片里的一个模型（`info` = 后端下发的上游能力，可能缺席）。 */
export interface ModelChoice {
  id: string;
  display: string;
  /** 人读的短名（去掉厂商前缀 / `:free`）。 */
  name: string;
  provider: string;
  info?: ModelListEntryInfo;
}

/** 按供应商分组后的模型列表。 */
export interface ModelGroup {
  provider: string;
  /** 供应商展示名（自定义 provider 用 display_name）。 */
  label: string;
  isCustom: boolean;
  isFree: boolean;
  models: ModelChoice[];
}

export function useModelSelector() {
  const { defaultModel } = useCurrentModel();
  const [config, setConfig] = useState<ModelConfigData>(() => {
    try {
      const raw = storage.getItem(MODEL_CONFIG_KEY);
      if (raw) {
        const cfg = JSON.parse(raw) as ModelConfigData;
        if (cfg && Array.isArray(cfg.enabled)) {
          if (cfg.enabled.length > 0) return cfg;
          if (cfg.defaultModel) {
            const parts = cfg.defaultModel.split(":", 2);
            if (parts.length === 2 && parts[0] && parts[1]) {
              return { enabled: [{ providerId: parts[0], modelId: parts[1] }], defaultModel: cfg.defaultModel };
            }
          }
        }
      }
    } catch {}
    // 语义（"三刀"）：enabled 为空就是空——不再用 modelCache 全量
    // 目录冒充可选列表。未启用模型不会出现在聊天选择器，空态提示去设置启用。
    return { enabled: [], defaultModel: "" };
  });
  const [selectedModel, setSelectedModel] = useState(() =>
    config.defaultModel || (config.enabled[0] ? `${config.enabled[0].providerId}:${config.enabled[0].modelId}` : "")
  );
  /** 后端下发的逐模型能力（上下文 / 模态 / reasoning / 免费标记）。 */
  const [meta, setMeta] = useState<Record<string, ModelListEntryInfo>>({});

  useEffect(() => {
    let cancelled = false;

    const refresh = () => {
      try {
        const raw = storage.getItem(MODEL_CONFIG_KEY);
        if (!raw) return;
        let cfg: ModelConfigData = JSON.parse(raw);
        if (cfg.enabled.length === 0 && cfg.defaultModel) {
          const parts = cfg.defaultModel.split(":", 2);
          if (parts.length === 2 && parts[0] && parts[1]) {
            cfg = { enabled: [{ providerId: parts[0], modelId: parts[1] }], defaultModel: cfg.defaultModel };
          }
        }
        setConfig(cfg);
        setSelectedModel((prev) => prev || cfg.defaultModel || (cfg.enabled[0] ? `${cfg.enabled[0].providerId}:${cfg.enabled[0].modelId}` : ""));
      } catch {}
    };

    // reconcile: 统一权威规则（与 ModelListTab 的 load 对齐 ——
    // 后端 config.yaml 的 model_selector 是持久权威，localStorage 是跨 tab
    // 瞬态镜像；此前本文件注释自称"localStorage 单一来源"，与 ModelTab
    // "后端权威"注释矛盾，即"双权威漂移"：同一份数据 3 处各认一个权威。
    //
    // 历史教训: 旧实现从 4 个 source（localStorage.enabled / cache /
    // backend.models / customProviders)做 union 再写回 localStorage +
    // saveModelList → 用户在设置面板取消勾选一个 model,
    // 下次 mount 时又从 customProviders 把它 union 回来,等于
    // 取消勾选彻底白做。同时 saveModelList 主动覆盖后端,跟
    // toggleModel 的"用户主动 commit"语义冲突。
    //
    // 统一后规则: 后端可达 → 后端即权威。本地∩后端非空 = 权威 enabled
    // （用户在设置页的增删已同步进镜像，交集保留用户意图）；交集为空但
    // 后端非空 = 本地镜像整体陈旧（新增首个模型 / localStorage 被逐出 /
    // 后端侧改名），采纳后端——不是旧 union 的复活勾选，后端本来就没有
    // 被取消的项。后端空列表也是权威：全 provider 未配钥时本地残留该清。
    // 后端不可达（离线）→ 本地镜像兜底，不吞勾选，也**不把空集写回镜像**
    // （否则一次网络抖动就毒化镜像，聊天页一直显示"未配置"）。
    // 本组件永不主动调 saveModelList —— 写路径统一走
    // writeModelConfigAndNotify（ModelListTab / ModelAddDialog 统一走它），
    // 那里一次写 localStorage + 推后端，事件扇出保持所有消费方一致。
    const reconcile = async () => {
      try {
        const backend = await getModelList().catch(() => null);
        if (cancelled) return;

        let local: ModelConfigData = { enabled: [], defaultModel: "" };
        try {
          const raw = storage.getItem(MODEL_CONFIG_KEY);
          if (raw) {
            const parsed = JSON.parse(raw) as ModelConfigData;
            if (parsed && Array.isArray(parsed.enabled)) local = parsed;
          }
        } catch {}

        const rawBackend = backend?.models ?? [];
        // 能力元数据按 `provider:model` 索引；catalog 里的也算上，
        // 用户在设置页启用后卡片能立刻显示上下文与能力标签。
        const metaMap: Record<string, ModelListEntryInfo> = {};
        for (const m of [...rawBackend, ...(backend?.catalog ?? [])]) {
          metaMap[`${m.provider_id}:${m.model_id}`] = m;
        }
        setMeta(metaMap);
        const backendIds = new Set(rawBackend.map((m) => `${m.provider_id}:${m.model_id}`));
        const backendEnabled = rawBackend.map((m) => ({ providerId: m.provider_id, modelId: m.model_id }));
        const intersected = local.enabled.filter((e) => backendIds.has(`${e.providerId}:${e.modelId}`));
        const enabled = backend === null
          ? local.enabled
          : intersected.length > 0
            ? intersected
            : backendEnabled;

        // orphan-aware 默认模型：优先有效的本地默认，其次后端默认，最后取首项。
        const validIds = new Set(enabled.map((e) => `${e.providerId}:${e.modelId}`));
        const defaultModel =
          (local.defaultModel && validIds.has(local.defaultModel)) ? local.defaultModel
            : (backend?.default_model && validIds.has(backend.default_model)) ? backend.default_model
              : (enabled.length > 0 ? `${enabled[0].providerId}:${enabled[0].modelId}` : "");

        const next: ModelConfigData = { enabled, defaultModel };
        if (backend !== null) storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(next));
        setConfig(next);
        setSelectedModel((prev) =>
          (prev && validIds.has(prev)) ? prev : (validIds.has(defaultModel) ? defaultModel : "")
        );
      } catch {}
    };

    // 配置写入（设置页各写入点统一走 writeModelConfigAndNotify）会广播
    // 事件；除读镜像外再 reconcile 一次，兜住"只写后端的外部变更"
    // （如手机面/手工改 config.yaml 后设置页 load() 镜像回写的场景）。
    const onChanged = () => {
      refresh();
      void reconcile();
    };
    window.addEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
    const onStorage = (e: StorageEvent) => {
      if (e.key === MODEL_CONFIG_KEY) {
        refresh();
        void reconcile();
      }
    };
    window.addEventListener("storage", onStorage);
    void reconcile();

    return () => {
      cancelled = true;
      window.removeEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  useEffect(() => {
    if (defaultModel) setSelectedModel((prev) => (prev === "" ? defaultModel : prev));
  }, [defaultModel]);

  // 后端权威 builtin 清单（覆盖静态兜底可能缺失的厂商）
  const [metaIds, setMetaIds] = useState<Set<string>>(new Set());
  useEffect(() => {
    let ok = true;
    getProviderMeta()
      .then((list) => {
        if (ok) setMetaIds(new Set(list.map((m) => m.id)));
      })
      .catch(() => {
        /* 分组降级用静态清单，拉取失败不打扰用户 */
      });
    return () => {
      ok = false;
    };
  }, []);

  const models = useMemo(() => {
    return config.enabled.map((m) => {
      const id = `${m.providerId}:${m.modelId}`;
      return {
        id,
        display: m.modelId,
        name: friendlyModelName(m.modelId),
        provider: m.providerId,
        info: meta[id],
      };
    });
  }, [config, meta]);

  const byProvider = useMemo<ModelGroup[]>(() => {
    const builtin = metaIds.size > 0 ? (id: string) => BUILTIN_PROVIDER_IDS.has(id) || metaIds.has(id) : (id: string) => BUILTIN_PROVIDER_IDS.has(id);
    const map = new Map<string, ModelGroup>();
    for (const m of models) {
      const isCustom = !builtin(m.provider);
      const isFree = m.info?.is_free === true;
      const entry = map.get(m.provider);
      if (entry) {
        entry.models.push(m);
      } else {
        map.set(m.provider, {
          provider: m.provider,
          label: providerLabel(m.provider, m.info),
          models: [m],
          isCustom: isFree ? false : isCustom,
          isFree,
        });
      }
    }
    return Array.from(map.values());
  }, [models, metaIds]);

  const currentDisplay = useMemo(() => {
    if (!models || models.length === 0) {
      return "未配置，请先配置模型";
    }
    const found = models.find((m) => m.id === selectedModel);
    return found ? found.name : (selectedModel ? friendlyModelName(selectedModel.split(":").slice(1).join(":")) : "未配置，请先配置模型");
  }, [models, selectedModel]);

  return { models, byProvider, selectedModel, setSelectedModel, currentDisplay, defaultModel };
}
