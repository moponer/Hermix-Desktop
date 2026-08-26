import { useState, useEffect, useMemo } from "react";
import { useCurrentModel, MODEL_CONFIG_CHANGED_EVENT } from "../hooks/useCurrentModel";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { getModelList } from "../api";

export const BUILTIN_PROVIDER_IDS = new Set([
  "minimax", "openai", "deepseek", "dashscope", "kimi",
  "siliconflow", "openrouter", "zhipu", "volcengine",
  "anthropic", "copilot", "qwen", "moonshot",
  "baichuan", "tencent", "spark", "yi",
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
    try {
      const cacheRaw = storage.getItem(STORAGE_KEYS.modelCache);
      const defRaw = storage.getItem(MODEL_CONFIG_KEY);
      const def = defRaw ? ((JSON.parse(defRaw) as ModelConfigData).defaultModel ?? "") : "";
      if (cacheRaw) {
        const cache = JSON.parse(cacheRaw) as Record<string, string[]>;
        const enabled: { providerId: string; modelId: string }[] = [];
        for (const [pid, mids] of Object.entries(cache)) {
          for (const mid of mids || []) enabled.push({ providerId: pid, modelId: mid });
        }
        if (enabled.length > 0) {
          return { enabled, defaultModel: def };
        }
      }
    } catch {}
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

    // reconcile: mount 时只同步 defaultModel 兜底(localStorage 缺失时),
    // 不主动 union / 不主动覆盖后端 enabled 列表。
    //
    // 历史教训: 旧实现从 4 个 source（localStorage.enabled / cache /
    // backend.models / customProviders)做 union 再写回 localStorage +
    // saveModelList → 用户在「自定义模型」区域取消勾选一个 model,
    // 下次 mount 时又从 customProviders 把它 union 回来,等于
    // 取消勾选彻底白做。同时 saveModelList 主动覆盖后端,跟
    // toggleModel 的"用户主动 commit"语义冲突。
    //
    // 新规则: enabled list 单一来源 = localStorage, 只在用户主动
    // toggle / setDefault 时变化；reconcile 只在下列场景兜底:
    //   1) localStorage 缺失 → 用 backend.enabled 起步一次（首次安装场景）
    //   2) localStorage 的 defaultModel 空 / 后端有 → 用后端 defaultModel
    //   3) 任何场景都不主动调 saveModelList
    let cancelled = false;
    const reconcile = async () => {
      try {
        const backend = await getModelList().catch(() => null);
        if (cancelled) return;

        let local: ModelConfigData = { enabled: [], defaultModel: "" };
        let localMissing = false;
        try {
          const raw = storage.getItem(MODEL_CONFIG_KEY);
          if (raw) {
            const parsed = JSON.parse(raw) as ModelConfigData;
            if (parsed && Array.isArray(parsed.enabled)) local = parsed;
          } else {
            localMissing = true;
          }
        } catch {}

        // 仅在 localStorage 完全缺失时（首次安装 / 清缓存）才兜底
        // 用后端的 enabled —— 避免 mount 时反复覆盖用户已经勾选的状态。
        const enabled: ModelConfigData["enabled"] = localMissing && backend?.models?.length
          ? backend.models.map((m) => ({ providerId: m.provider_id, modelId: m.model_id }))
          : local.enabled;

        const defaultModel =
          local.defaultModel ||
          backend?.default_model ||
          (enabled.length > 0 ? `${enabled[0].providerId}:${enabled[0].modelId}` : "");

        const next: ModelConfigData = { enabled, defaultModel };
        storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(next));
        setConfig(next);
        setSelectedModel((prev) => prev || defaultModel);
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

  const models = useMemo(() => {
    return config.enabled.map((m) => ({ id: `${m.providerId}:${m.modelId}`, display: m.modelId, provider: m.providerId }));
  }, [config]);

  const byProvider = useMemo(() => {
    const map = new Map<string, { provider: string; models: typeof models; isCustom: boolean }>();
    for (const m of models) {
      const isCustom = !BUILTIN_PROVIDER_IDS.has(m.provider);
      const entry = map.get(m.provider);
      if (entry) {
        entry.models.push(m);
      } else {
        map.set(m.provider, { provider: m.provider, models: [m], isCustom });
      }
    }
    return Array.from(map.values());
  }, [models]);

  const availableExtras = useMemo(() => {
    try {
      const raw = storage.getItem(STORAGE_KEYS.modelCache);
      if (!raw) return [] as typeof models;
      const cache = JSON.parse(raw) as Record<string, string[]>;
      const enabledIds = new Set(models.map((m) => m.id));
      const out: typeof models = [];
      for (const [pid, mids] of Object.entries(cache)) {
        for (const mid of mids || []) {
          const id = `${pid}:${mid}`;
          if (enabledIds.has(id)) continue;
          out.push({ id, display: mid, provider: pid });
        }
      }
      return out;
    } catch {
      return [] as typeof models;
    }
  }, [models]);

  const currentDisplay = useMemo(() => {
    if (!models || models.length === 0) {
      return "未配置，请先配置模型";
    }
    const found = models.find((m) => m.id === selectedModel);
    return found ? found.display : (selectedModel || "未配置，请先配置模型");
  }, [models, selectedModel]);

  return { models, byProvider, availableExtras, selectedModel, setSelectedModel, currentDisplay, defaultModel };
}
