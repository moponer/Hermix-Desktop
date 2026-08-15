import { useState, useEffect, useMemo } from "react";
import { useCurrentModel, MODEL_CONFIG_CHANGED_EVENT } from "../hooks/useCurrentModel";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { getModelList, saveModelList, apiUrl } from "../api";

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

    let cancelled = false;
    const reconcile = async () => {
      try {
        const [backend, customProviders] = await Promise.all([
          getModelList(),
          fetch(apiUrl("/api/custom-providers"))
            .then((r) => (r.ok ? r.json() : null))
            .then((providers: { id: string; models: string[] }[] | null) => {
              const out: { providerId: string; modelId: string }[] = [];
              for (const p of providers || []) {
                for (const mid of p.models || []) out.push({ providerId: p.id, modelId: mid });
              }
              return out;
            })
            .catch(() => [] as { providerId: string; modelId: string }[]),
        ]);
        if (cancelled) return;

        let local: ModelConfigData = { enabled: [], defaultModel: "" };
        try {
          const raw = storage.getItem(MODEL_CONFIG_KEY);
          if (raw) {
            const parsed = JSON.parse(raw) as ModelConfigData;
            if (parsed && Array.isArray(parsed.enabled)) local = parsed;
          }
        } catch {}

        const union = new Map<string, { providerId: string; modelId: string }>();
        const add = (p: string, m: string) => {
          if (!p || !m) return;
          union.set(`${p}:${m}`, { providerId: p, modelId: m });
        };
        for (const e of local.enabled) add(e.providerId, e.modelId);
        try {
          const cacheRaw = storage.getItem(STORAGE_KEYS.modelCache);
          if (cacheRaw) {
            const cache = JSON.parse(cacheRaw) as Record<string, string[]>;
            for (const [pid, mids] of Object.entries(cache)) {
              for (const mid of mids || []) add(pid, mid);
            }
          }
        } catch {}
        for (const e of backend?.models || []) add(e.provider_id, e.model_id);
        for (const e of customProviders) add(e.providerId, e.modelId);

        const defaultCandidate =
          local.defaultModel ||
          backend?.default_model ||
          "";
        if (defaultCandidate && !union.has(defaultCandidate)) {
          const parts = defaultCandidate.split(":", 2);
          if (parts[0] && parts[1]) union.set(defaultCandidate, { providerId: parts[0], modelId: parts[1] });
        }

        const enabled = Array.from(union.values());
        const defaultModel =
          local.defaultModel ||
          backend?.default_model ||
          (enabled.length > 0 ? `${enabled[0].providerId}:${enabled[0].modelId}` : "");
        const next: ModelConfigData = { enabled, defaultModel };
        storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(next));
        try {
          const cache: Record<string, string[]> = {};
          for (const m of enabled) {
            if (!cache[m.providerId]) cache[m.providerId] = [];
            if (!cache[m.providerId].includes(m.modelId)) cache[m.providerId].push(m.modelId);
          }
          storage.setItem(STORAGE_KEYS.modelCache, JSON.stringify(cache));
        } catch {}
        setConfig(next);
        setSelectedModel((prev) => prev || defaultModel);
        saveModelList(
          enabled.map((e) => ({ provider_id: e.providerId, model_id: e.modelId })),
          defaultModel || null,
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
