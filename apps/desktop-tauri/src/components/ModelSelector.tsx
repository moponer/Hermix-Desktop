import { useState, useEffect, useMemo } from "react";
import { useCurrentModel, MODEL_CONFIG_CHANGED_EVENT } from "../hooks/useCurrentModel";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { getModelList, getProviderMeta } from "../api";

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
    // 语义（主人拍板"三刀"）：enabled 为空就是空——不再用 modelCache 全量
    // 目录冒充可选列表。未启用模型不会出现在聊天选择器，空态提示去设置启用。
    return { enabled: [], defaultModel: "" };
  });
  const [selectedModel, setSelectedModel] = useState(() =>
    config.defaultModel || (config.enabled[0] ? `${config.enabled[0].providerId}:${config.enabled[0].modelId}` : "")
  );

  useEffect(() => {
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
    const onChanged = () => refresh();
    window.addEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
    const onStorage = (e: StorageEvent) => {
      if (e.key === MODEL_CONFIG_KEY) refresh();
    };
    window.addEventListener("storage", onStorage);

    // reconcile: 统一权威规则（与 ModelTab 的 mount-fetch 对齐 ——
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
    // 统一后规则: 后端可达 → 后端即权威（本地条目中后端没有的剔除，
    // 后端空列表也是权威：全 provider 未配钥时本地残留该清）；
    // 后端不可达（离线）→ 本地镜像兜底，不吞勾选。
    // 本组件永不主动调 saveModelList —— 写路径统一走
    // writeModelConfigAndNotify（ModelTab 统一供应商面板，内置 + 自定义
    // provider 共享同一份 config state），
    // 那里一次写 localStorage + 推后端，事件扇出保持所有消费方一致。
    let cancelled = false;
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

        // 后端可达：本地∩后端 = 权威 enabled 集合（后端空也清残留）；
        // 后端不可达（backend === null）：本地镜像兜底，离线不吞勾选。
        const rawBackend = backend?.models ?? [];
        const backendIds = new Set(rawBackend.map((m) => `${m.provider_id}:${m.model_id}`));
        const enabled = backend === null
          ? local.enabled
          : local.enabled.filter((e) => backendIds.has(`${e.providerId}:${e.modelId}`));

        // orphan-aware 默认模型：优先有效的本地默认，其次后端默认，最后取首项。
        const validIds = new Set(enabled.map((e) => `${e.providerId}:${e.modelId}`));
        const defaultModel =
          (local.defaultModel && validIds.has(local.defaultModel)) ? local.defaultModel
            : (backend?.default_model && validIds.has(backend.default_model)) ? backend.default_model
              : (enabled.length > 0 ? `${enabled[0].providerId}:${enabled[0].modelId}` : "");

        const next: ModelConfigData = { enabled, defaultModel };
        storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(next));
        setConfig(next);
        setSelectedModel((prev) =>
          (prev && validIds.has(prev)) ? prev : (validIds.has(defaultModel) ? defaultModel : "")
        );
      } catch {}
    };
    reconcile();

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
    return config.enabled.map((m) => ({ id: `${m.providerId}:${m.modelId}`, display: m.modelId, provider: m.providerId }));
  }, [config]);

  const byProvider = useMemo(() => {
    const builtin = metaIds.size > 0 ? (id: string) => BUILTIN_PROVIDER_IDS.has(id) || metaIds.has(id) : (id: string) => BUILTIN_PROVIDER_IDS.has(id);
    const map = new Map<string, { provider: string; models: typeof models; isCustom: boolean }>();
    for (const m of models) {
      const isCustom = !builtin(m.provider);
      const entry = map.get(m.provider);
      if (entry) {
        entry.models.push(m);
      } else {
        map.set(m.provider, { provider: m.provider, models: [m], isCustom });
      }
    }
    return Array.from(map.values());
  }, [models, metaIds]);

  const currentDisplay = useMemo(() => {
    if (!models || models.length === 0) {
      return "未配置，请先配置模型";
    }
    const found = models.find((m) => m.id === selectedModel);
    return found ? found.display : (selectedModel || "未配置，请先配置模型");
  }, [models, selectedModel]);

  return { models, byProvider, selectedModel, setSelectedModel, currentDisplay, defaultModel };
}
