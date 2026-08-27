import { useState, useEffect, useCallback, useMemo } from "react";
import { Check, AlertCircle, RefreshCw, ChevronRight, Star, Trash2, CheckCircle, Save } from "lucide-react";
import * as api from "../../api";
import { storage } from "../../utils/storageCompat";
import { writeModelConfigAndNotify, MODEL_CONFIG_CHANGED_EVENT } from "../../hooks/useCurrentModel";
import { ProviderIcon } from "../ProviderIcon";
import { useSafeTimeout } from "../../hooks/useSafeTimeout";
import { apiUrl } from "../../api";
import {
  ProviderDef,
  PROVIDERS,
  MODEL_CONFIG_KEY,
  loadModelConfig,
  loadModelCache,
  saveModelCache,
  CONTEXT_WINDOW_OPTIONS,
  resolveContextWindow,
  defaultOptionLabel,
  type ModelConfigData,
} from "./settingsUtils";

// ── Model Tab ──────────────────────────────────────
//
// 选择器优先布局（三段式垂直）：
//   1. 默认模型 — 顶部紧凑的两级 [Provider ▾] [Model ▾] 选择器（与聊天主窗
//      ModelSelector 同款体验）；每次操作触发"✓ 已保存"绿色 banner + 行内
//      700ms 绿闪，保存体感明确
//   2. 已启用模型 — 紧凑列表：每行 provider chip + model + 上下文窗口 +
//      ⭐/默认 + ✕ 移除；这是用户操作的主界面
//   3. 供应商 — 折叠面板，展开后填 API Key + 获取模型；首次自动 mount-fetch
//      仍保留以保持原使用体感

export function ModelTab() {
  const safeSetTimeout = useSafeTimeout();
  const [config, setConfig] = useState<ModelConfigData>(loadModelConfig);
  const [fetchedModels, setFetchedModels] = useState<Record<string, string[]>>(loadModelCache);
  const [loadingProvider, setLoadingProvider] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [info, setInfo] = useState("");
  const [activeKeys, setActiveKeys] = useState<Record<string, boolean>>({});
  const [customIdsWithKey, setCustomIdsWithKey] = useState<Set<string>>(new Set());
  const [expandedProviders, setExpandedProviders] = useState<Set<string>>(new Set());
  const [providerKeyInput, setProviderKeyInput] = useState<Record<string, string>>({});
  // 单行短暂绿闪用的 key（fid），700ms 后自清
  const [flashKey, setFlashKey] = useState<string | null>(null);

  // ── Custom provider has_key (authoritative from server) ──
  useEffect(() => {
    api.getCustomProviders().then((providers) => {
      const withKey = new Set<string>();
      providers.forEach((p) => { if (p.has_key) withKey.add(p.id); });
      setCustomIdsWithKey(withKey);
    }).catch((e) => { console.warn("[ModelTab] custom provider fetch failed:", e); });
  }, []);

  // ── Auto-fetch on mount: 保留原行为（configured builtin 一次性拉取并启用）──
  useEffect(() => {
    api.getEnvVars().then(async (envVars) => {
      const keys: Record<string, boolean> = {};
      PROVIDERS.forEach((p) => { keys[p.id] = !!envVars[p.keyEnvVar]?.is_set; });
      for (const [varName, info] of Object.entries(envVars ?? {})) {
        const m = /^CUSTOM_([A-Za-z0-9_-]+)_API_KEY$/.exec(varName);
        if (m && (info as { is_set?: boolean })?.is_set) {
          const raw = m[1].toLowerCase().replace(/_/g, "-");
          const normalized = raw.replace(/^custom-/, "");
          keys[`custom-${normalized}`] = true;
        }
      }
      setActiveKeys(keys);

      // 从缓存恢复（fetch 失败时不让 UI 全空）
      const cached = loadModelCache();
      if (Object.keys(cached).length > 0) setFetchedModels(cached);
      const cachedCfg = loadModelConfig();
      if (cachedCfg.enabled.length > 0) setConfig(cachedCfg);

      const fetchPromises = PROVIDERS.filter((p) => keys[p.id]).map(async (p) => {
        const MAX_RETRIES = 2;
        for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
          try {
            const revealResp = await fetch(apiUrl("/api/env/reveal"), {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ key: p.keyEnvVar }),
            });
            if (!revealResp.ok) {
              if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
              continue;
            }
            const { value: apiKey } = await revealResp.json();
            if (!apiKey) break;
            const modelsResp = await fetch(`${p.baseUrl}/models`, {
              headers: { Authorization: `Bearer ${apiKey}` },
            });
            if (!modelsResp.ok) {
              if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
              continue;
            }
            const data = await modelsResp.json();
            const models: string[] = (data.data || []).map((m: { id: string }) => m.id).sort();
            if (models.length > 0) {
              setFetchedModels((prev) => ({ ...prev, [p.id]: models }));
              setConfig((prev) => {
                const existing = new Set(prev.enabled.map((m) => fullId(m.providerId, m.modelId)));
                const newEnabled = [...prev.enabled];
                models.forEach((modelId) => {
                  if (!existing.has(`${p.id}:${modelId}`)) newEnabled.push({ providerId: p.id, modelId });
                });
                const newDefault = prev.defaultModel || (newEnabled.length > 0 ? `${p.id}:${models[0]}` : "");
                const next = { enabled: newEnabled, defaultModel: newDefault, modelContexts: prev.modelContexts ?? {} };
                writeModelConfigAndNotify(next);
                return next;
              });
            }
            break;
          } catch (err) {
            if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
          }
        }
      });
      await Promise.allSettled(fetchPromises);
    }).catch((err) => { console.error("[ModelTab] mount env fetch error:", err); });
  }, []);

  // 缓存 fetchedModels → localStorage
  useEffect(() => { saveModelCache(fetchedModels); }, [fetchedModels]);

  // Banner 自动 3s 清除
  useEffect(() => {
    if (!info && !error) return;
    const t = window.setTimeout(() => { setInfo(""); setError(""); }, 3000);
    return () => window.clearTimeout(t);
  }, [info, error]);

  const providerName = useCallback((pid: string): string => {
    const def = PROVIDERS.find((p) => p.id === pid);
    return def?.name ?? pid.replace(/^custom-/, "");
  }, []);

  const flashSaved = useCallback((fullId: string) => {
    setInfo("✓ 已保存");
    setFlashKey(fullId);
    window.setTimeout(() => setFlashKey((k) => (k === fullId ? null : k)), 700);
  }, []);

  const setDefault = useCallback((providerId: string, modelId: string) => {
    const fid = fullId(providerId, modelId);
    setConfig((prev) => {
      if (prev.defaultModel === fid) return prev;
      const newConfig: ModelConfigData = { ...prev, defaultModel: fid, modelContexts: prev.modelContexts ?? {} };
      safeSetTimeout(async () => {
        writeModelConfigAndNotify(newConfig);
        try {
          const backendCfg = await api.getConfig();
          backendCfg.model = fid;
          await api.updateConfig(backendCfg);
        } catch (e) { console.warn("setDefault backend sync failed:", e); }
      }, 0);
      return newConfig;
    });
    flashSaved(fid);
  }, [safeSetTimeout, flashSaved]);

  const toggleModel = useCallback((providerId: string, modelId: string) => {
    const fid = fullId(providerId, modelId);
    setConfig((prev) => {
      const key = fid;
      const exists = prev.enabled.some((m) => fullId(m.providerId, m.modelId) === key);
      const newEnabled = exists
        ? prev.enabled.filter((m) => fullId(m.providerId, m.modelId) !== key)
        : [...prev.enabled, { providerId, modelId }];
      const defaultCleared = exists && prev.defaultModel === key;
      const newConfig: ModelConfigData = {
        enabled: newEnabled,
        defaultModel: defaultCleared ? "" : prev.defaultModel,
        modelContexts: prev.modelContexts ?? {},
      };
      safeSetTimeout(async () => {
        writeModelConfigAndNotify(newConfig);
        if (defaultCleared) {
          try {
            const backendCfg = await api.getConfig();
            backendCfg.model = "";
            await api.updateConfig(backendCfg);
          } catch (e) { console.warn("toggleModel backend sync failed:", e); }
        }
      }, 0);
      return newConfig;
    });
    flashSaved(fid);
  }, [safeSetTimeout, flashSaved]);

  const setModelContext = useCallback((providerId: string, modelId: string, ctx: number | null) => {
    const fid = fullId(providerId, modelId);
    setConfig((prev) => {
      const nextContexts = { ...(prev.modelContexts ?? {}) };
      if (ctx == null || ctx === 0) delete nextContexts[fid];
      else nextContexts[fid] = ctx;
      const next = { ...prev, modelContexts: nextContexts };
      safeSetTimeout(async () => {
        writeModelConfigAndNotify(next);
        // custom provider 还要把 modelContexts 推给后端
        if (providerId.startsWith("custom-")) {
          try { await api.updateCustomProviderModelContexts(providerId, nextContexts); }
          catch (e) { console.warn("setModelContext sync failed:", e); }
        }
      }, 0);
      return next;
    });
    flashSaved(fid);
  }, [safeSetTimeout, flashSaved]);

  /** 从供应商折叠面板触发的获取模型:可接受 (a) 用户在面板里输入的 key
   *  → 写 env + 拉取；(b) 已配置 key → 直接拉取。 */
  const fetchModelsForProvider = useCallback(async (p: ProviderDef) => {
    setLoadingProvider(p.id);
    setError("");
    try {
      let apiKey = "";
      const typedKey = providerKeyInput[p.id]?.trim();
      if (typedKey) {
        await api.setEnvVar(p.keyEnvVar, typedKey);
        apiKey = typedKey;
        setActiveKeys((prev) => ({ ...prev, [p.id]: true }));
      } else {
        // 探测后端 env 已有的 key
        const reveal = await fetch(apiUrl("/api/env/reveal"), {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ key: p.keyEnvVar }),
        });
        if (reveal.ok) {
          const data = await reveal.json();
          apiKey = data.value ?? "";
        }
      }
      if (!apiKey) {
        setError("请填写 API Key 后再获取");
        setLoadingProvider(null);
        return;
      }
      const resp = await fetch(`${p.baseUrl}/models`, { headers: { Authorization: `Bearer ${apiKey}` } });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data = await resp.json();
      const models: string[] = (data.data || []).map((m: { id: string }) => m.id).sort();
      if (models.length === 0) { setError("未返回任何模型"); setLoadingProvider(null); return; }
      setFetchedModels((prev) => ({ ...prev, [p.id]: models }));
      setConfig((prev) => {
        const existing = new Set(prev.enabled.map((m) => fullId(m.providerId, m.modelId)));
        const newEnabled = [...prev.enabled];
        models.forEach((mid) => {
          if (!existing.has(fullId(p.id, mid))) newEnabled.push({ providerId: p.id, modelId: mid });
        });
        const newDefault = prev.defaultModel || (newEnabled.length > 0 ? fullId(p.id, models[0]) : "");
        const next = { ...prev, enabled: newEnabled, defaultModel: newDefault, modelContexts: prev.modelContexts ?? {} };
        writeModelConfigAndNotify(next);
        return next;
      });
      try {
        const r = await api.autoCorrectConfig();
        if (r.report) setInfo(`✓ ${r.message}`);
      } catch {}
    } catch (e) {
      setError(`获取失败: ${e instanceof Error ? e.message : String(e)}`);
    }
    setLoadingProvider(null);
  }, [providerKeyInput]);

  // ── Derived ──
  const enabledRows = useMemo(() => {
    return config.enabled
      .filter((e) => {
        // custom provider 若 key 已被后端删除,这里不再展示（防僵尸）
        return e.providerId.startsWith("custom-")
          ? customIdsWithKey.has(e.providerId)
          : true;
      })
      .map((e) => {
        const fid = fullId(e.providerId, e.modelId);
        return {
          ...e,
          fid,
          providerName: providerName(e.providerId),
          isCustom: e.providerId.startsWith("custom-"),
          isDefault: config.defaultModel === fid,
          ctx: config.modelContexts?.[fid],
        };
      });
  }, [config.enabled, config.defaultModel, config.modelContexts, customIdsWithKey, providerName]);

  const selectableProviders = useMemo(() => {
    const out: { id: string; name: string }[] = [];
    PROVIDERS.forEach((p) => {
      if (activeKeys[p.id]) out.push({ id: p.id, name: p.name });
    });
    customIdsWithKey.forEach((id) => out.push({ id, name: providerName(id) }));
    return out;
  }, [activeKeys, customIdsWithKey, providerName]);

  const defaultProvId = config.defaultModel?.split(":")[0] ?? "";
  const defaultModelId = config.defaultModel?.split(":")[1] ?? "";
  const defaultModels = fetchedModels[defaultProvId] ?? [];
  const hasAnyConfigurable = selectableProviders.length > 0;

  const toggleProviderExpanded = (id: string) => {
    setExpandedProviders((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  return (
    <div className="flex flex-col h-full">
      {/* ── sticky save/error banner ── */}
      {(info || error) && (
        <div
          role="status"
          className={`sticky top-0 z-10 flex items-center gap-2 px-4 py-2 text-[12px] border-b ${
            error
              ? "bg-red-500/10 border-red-500/30 text-red-400"
              : "bg-emerald-500/10 border-emerald-500/30 text-emerald-400"
          }`}
        >
          {error ? <AlertCircle size={14} /> : <Check size={14} />}
          <span>{info || error}</span>
        </div>
      )}

      <div className="flex-1 overflow-y-auto">
        {/* ── SECTION 1: 默认模型 ── */}
        <section className="px-6 py-5 border-b border-border-primary">
          <div className="flex items-center gap-2 mb-3">
            <Star size={14} className="text-accent" />
            <h2 className="text-[15px] font-semibold text-text-primary">默认模型</h2>
          </div>
          {hasAnyConfigurable ? (
            <div className="flex flex-wrap items-center gap-2 max-w-2xl">
              <select
                value={defaultProvId}
                onChange={(e) => {
                  const newProv = e.target.value;
                  if (newProv === defaultProvId) return;
                  const newModel = fetchedModels[newProv]?.[0] ?? "";
                  if (newModel) setDefault(newProv, newModel);
                  else setInfo("→请先在「供应商」中获取该提供商的模型");
                }}
                className="bg-bg-card border border-border-primary rounded px-2.5 py-1.5 text-[13px] text-text-primary outline-none min-w-[140px]"
              >
                <option value="">选择供应商</option>
                {selectableProviders.map((p) => (
                  <option key={p.id} value={p.id}>{p.name}</option>
                ))}
              </select>
              <select
                value={defaultModelId}
                onChange={(e) => {
                  if (defaultProvId && e.target.value) setDefault(defaultProvId, e.target.value);
                }}
                disabled={!defaultProvId}
                className="bg-bg-card border border-border-primary rounded px-2.5 py-1.5 text-[13px] text-text-primary outline-none min-w-[180px] disabled:opacity-40 font-mono"
              >
                <option value="">选择模型</option>
                {defaultModels.map((m) => (
                  <option key={m} value={m}>{m}</option>
                ))}
              </select>
              {config.defaultModel && (
                <span className="text-[11px] text-text-faint ml-1">⭐ 当前默认</span>
              )}
            </div>
          ) : (
            <p className="text-[12px] text-text-muted">尚未配置任何供应商,展开下方「供应商」配置 API Key 后获取模型即可。</p>
          )}
          {!config.defaultModel && hasAnyConfigurable && (
            <p className="text-[11px] text-warning mt-2">⚠ 未设置默认模型,聊天无法开始</p>
          )}
        </section>

        {/* ── SECTION 2: 已启用模型 ── */}
        <section className="px-6 py-5 border-b border-border-primary">
          <div className="flex items-center gap-2 mb-3">
            <Check size={14} className="text-text-secondary" />
            <h2 className="text-[15px] font-semibold text-text-primary">
              已启用模型
              <span className="ml-1 text-text-muted text-[12px] font-normal">({enabledRows.length})</span>
            </h2>
          </div>
          {enabledRows.length === 0 ? (
            <p className="text-[12px] text-text-muted italic py-3">尚未启用任何模型。展开下方「供应商」→ 获取模型 → 在「已启用模型」勾选启用。</p>
          ) : (
            <ul className="space-y-1.5">
              {enabledRows.map((row) => (
                <EnabledModelRow
                  key={row.fid}
                  row={row}
                  flashed={flashKey === row.fid}
                  onSetDefault={() => setDefault(row.providerId, row.modelId)}
                  onRemove={() => toggleModel(row.providerId, row.modelId)}
                  onSetContext={(v) => setModelContext(row.providerId, row.modelId, v)}
                />
              ))}
            </ul>
          )}
        </section>

        {/* ── SECTION 3: 供应商折叠面板 ── */}
        <section className="px-6 py-5">
          <h2 className="text-[15px] font-semibold text-text-primary mb-3">供应商</h2>
          <div className="space-y-1.5">
            {PROVIDERS.map((p) => (
              <ProviderAccordionItem
                key={p.id}
                provider={p}
                expanded={expandedProviders.has(p.id)}
                configured={activeKeys[p.id]}
                modelCount={fetchedModels[p.id]?.length ?? 0}
                loading={loadingProvider === p.id}
                keyInput={providerKeyInput[p.id] ?? ""}
                onToggle={() => toggleProviderExpanded(p.id)}
                onKeyInputChange={(v) => setProviderKeyInput((prev) => ({ ...prev, [p.id]: v }))}
                onFetch={() => fetchModelsForProvider(p)}
              />
            ))}
          </div>
          <p className="mt-4 text-[11px] text-text-muted italic">自定义 OpenAI 兼容服务商请前往「自定义提供商」标签页配置。</p>
        </section>
      </div>
    </div>
  );
}

// ── 已启用模型行 ──
function EnabledModelRow({ row, flashed, onSetDefault, onRemove, onSetContext }: {
  row: {
    providerId: string;
    modelId: string;
    providerName: string;
    isDefault: boolean;
    isCustom: boolean;
    ctx?: number;
    fid: string;
  };
  flashed: boolean;
  onSetDefault: () => void;
  onRemove: () => void;
  onSetContext: (v: number | null) => void;
}) {
  return (
    <li
      className={`flex items-center gap-2 px-3 py-2 rounded-lg text-[12px] border transition-colors ${
        flashed
          ? "bg-emerald-500/10 border-emerald-500/40"
          : row.isDefault
            ? "bg-accent/10 border-accent/30"
            : "bg-bg-card border-border-primary hover:bg-bg-tertiary"
      }`}
    >
      <ProviderIcon providerId={row.providerId} size={16} title={row.providerName} />
      <span className="font-mono text-text-primary truncate min-w-0 flex-1">{row.modelId}</span>
      <span className="text-[10px] text-text-faint uppercase shrink-0">
        {row.isCustom ? "CUSTOM" : "BUILTIN"}
      </span>
      <select
        value={row.ctx ?? ""}
        onChange={(e) => onSetContext(e.target.value ? Number(e.target.value) : null)}
        title="上下文窗口;空=后端 catalog 兜底"
        className="bg-bg-tertiary border border-border-secondary rounded px-1.5 py-0.5 text-[10px] text-text-muted outline-none"
      >
        <option value="">{defaultOptionLabel(resolveContextWindow(row.modelId, row.ctx, null))}</option>
        {CONTEXT_WINDOW_OPTIONS.map((o) => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
      {row.isDefault ? (
        <span className="text-[10px] text-accent font-semibold shrink-0">⭐ 默认</span>
      ) : (
        <button
          onClick={onSetDefault}
          className="text-[10px] px-2 py-0.5 rounded border border-border-secondary text-text-muted hover:border-accent hover:text-accent shrink-0"
        >
          设为默认
        </button>
      )}
      <button
        onClick={onRemove}
        title="取消启用"
        className="p-0.5 rounded text-text-muted hover:text-red-400 hover:bg-red-500/10 shrink-0"
      >
        <Trash2 size={12} />
      </button>
    </li>
  );
}

// ── 供应商折叠面板 ──
function ProviderAccordionItem({ provider, expanded, configured, modelCount, loading, keyInput, onToggle, onKeyInputChange, onFetch }: {
  provider: ProviderDef;
  expanded: boolean;
  configured: boolean;
  modelCount: number;
  loading: boolean;
  keyInput: string;
  onToggle: () => void;
  onKeyInputChange: (v: string) => void;
  onFetch: () => void;
}) {
  return (
    <div className="rounded-lg border border-border-primary bg-bg-card overflow-hidden">
      <button
        onClick={onToggle}
        className="w-full flex items-center gap-2 px-3 py-2 text-left text-[12px] hover:bg-bg-tertiary"
      >
        <ChevronRight
          size={14}
          className={`shrink-0 text-text-muted transition-transform duration-150 ${expanded ? "rotate-90" : ""}`}
        />
        <ProviderIcon providerId={provider.id} size={16} title={provider.name} />
        <div className="flex-1 min-w-0">
          <div className="font-medium text-text-primary truncate">{provider.name}</div>
          <div className="text-[10px] text-text-muted truncate">{provider.description}</div>
        </div>
        {configured && <span className="w-1.5 h-1.5 rounded-full bg-green-400 shrink-0" />}
        {modelCount > 0 && (
          <span className="text-[10px] text-text-faint shrink-0">{modelCount} 模型</span>
        )}
      </button>
      {expanded && (
        <div className="border-t border-border-primary p-3 bg-bg-primary space-y-2">
          <div className="flex gap-2">
            <input
              type="password"
              value={keyInput}
              onChange={(e) => onKeyInputChange(e.target.value)}
              placeholder={configured ? "已配置,输入新 Key 以更新" : `输入 ${provider.name} API Key`}
              className="flex-1 bg-bg-tertiary border border-border-primary rounded px-2.5 py-1.5 text-[12px] text-text-primary placeholder-text-muted outline-none"
              onKeyDown={(e) => { if (e.key === "Enter") onFetch(); }}
            />
            <button
              onClick={onFetch}
              disabled={loading}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded text-[12px] bg-accent hover:bg-accent-hover text-accent-on-text disabled:opacity-40 shrink-0"
            >
              <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
              {loading ? "获取中" : configured ? "重新获取" : "获取模型"}
            </button>
          </div>
          <p className="text-[10px] text-text-muted">
            {modelCount > 0
              ? `已获取 ${modelCount} 个模型 → 切到上方「已启用模型」勾选启用`
              : "填写 API Key 后点击获取,模型会缓存到本地并自动启用"}
          </p>
        </div>
      )}
    </div>
  );
}

// ── helpers ──
function fullId(providerId: string, modelId: string): string {
  return `${providerId}:${modelId}`;
}

// ── Custom Provider Section (form + model list) ──────────

export function CustomProviderSection({
  customModels,
  onModelsFetched,
  onRefresh,
  toggleModel,
  setDefault,
  config,
  setConfig,
}: {
  customModels: Record<string, string[]>;
  onModelsFetched: (id: string, models: string[]) => void;
  onRefresh: () => void;
  toggleModel: (providerId: string, modelId: string) => void;
  setDefault: (fullId: string) => void;
  config: ModelConfigData;
  setConfig: React.Dispatch<React.SetStateAction<ModelConfigData>>;
}) {
  const [name, setName] = useState("");
  const [apiUrl, setApiUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [modelInput, setModelInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [fetching, setFetching] = useState(false);
  const [testing, setTesting] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [success, setSuccess] = useState("");
  const [providers, setProviders] = useState<api.CustomProviderSummary[]>([]);
  const [editingId, setEditingId] = useState<string | null>(null);
  // 互斥展开：同时只允许一个 provider 卡片展开。null = 全部折叠。
  const [expandedProviderId, setExpandedProviderId] = useState<string | null>(null);
  const [, setProviderModels] = useState<Record<string, string[]>>({});

  useEffect(() => {
    api.getCustomProviders().then((list) => setProviders(list)).catch((e) => { console.warn("[SettingsView] getCustomProviders failed:", e); });
  }, []);

  const safeId = name.trim()
    ? `custom-${name.trim().toLowerCase().replace(/[^a-z0-9一-龥_-]/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "")}`
    : "";

  const handleFetchModelsForSavedProvider = async (id: string) => {
    setFetching(true);
    setError("");
    try {
      const resp = await api.fetchCustomProviderModels(id);
      if (resp.error) { setError(`获取失败: ${resp.error}`); setFetching(false); return; }
      const models = resp.models.map((m) => m.id).sort();
      if (models.length === 0) { setError("未返回任何模型"); setFetching(false); return; }
      setProviderModels((prev) => ({ ...prev, [id]: models }));
      onModelsFetched(id, models);
      const probedWithWindow = resp.models.filter((m) => typeof m.context_window === "number" && (m.context_window as number) > 0);
      if (probedWithWindow.length > 0) {
        setConfig((prev) => {
          const current = prev.modelContexts ?? {};
          const nextContexts: Record<string, number> = { ...current };
          let changed = false;
          for (const m of probedWithWindow) {
            const key = `${id}:${m.id}`;
            const cw = m.context_window as number;
            if (!current[key] || current[key] === 0) {
              nextContexts[key] = cw;
              changed = true;
            }
          }
          if (!changed) return prev;
          const nextCfg = { ...prev, modelContexts: nextContexts };
          writeModelConfigAndNotify(nextCfg);
          api.updateCustomProviderModelContexts(id, nextContexts).catch((e) => { console.warn("[CustomProviderSection] sync probed contexts failed:", e); });
          return nextCfg;
        });
      }
      api.getCustomProviders().then((list) => setProviders(list)).catch(() => { /* non-fatal */ });
      onRefresh();
      const probedHint = probedWithWindow.length > 0
        ? `, ${probedWithWindow.length} 个已自动填入探测的 context_window`
        : "";
      setSuccess(`成功获取 ${models.length} 个模型${probedHint},请在「自定义模型」区域勾选要启用的`);
    } catch (e) {
      setError(`获取模型失败: ${e instanceof Error ? e.message : String(e)}`);
    }
    setFetching(false);
  };

  const handleTestConnection = async (id: string) => {
    setTesting(id);
    try {
      const resp = await api.testCustomProviderConnection(id);
      if (resp.ok) {
        setSuccess(`连接成功 (HTTP ${resp.status})，返回 ${resp.models?.length || 0} 个模型`);
      } else {
        setError(`连接失败 (HTTP ${resp.status}): ${resp.error || ""}`);
      }
    } catch (e) {
      setError(`连接失败: ${e instanceof Error ? e.message : String(e)}`);
    }
    setTesting(null);
  };

  const handleEdit = (p: api.CustomProviderSummary) => {
    setEditingId(p.id);
    setName(p.display_name || p.id.replace(/^custom-/, ""));
    setApiUrl(p.base_url);
    setApiKey("");
    setModelInput(p.models.join(", "));
    setError("");
    setSuccess("");
  };

  const handleCancelEdit = () => {
    setEditingId(null);
    setName("");
    setApiUrl("");
    setApiKey("");
    setModelInput("");
    setError("");
    setSuccess("");
  };

  const handleDelete = async (id: string, _models: string[]) => {
    try {
      await api.deleteCustomProvider(id);
      setProviders((prev) => prev.filter((p) => p.id !== id));
      setProviderModels((prev) => { const copy = { ...prev }; delete copy[id]; return copy; });
      if (editingId === id) handleCancelEdit();
      setSuccess("已删除");
      onRefresh();
    } catch (e) {
      setError(`删除失败: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  const handleFetchModels = async () => {
    if (!safeId) { setError("请填写提供商名称"); return; }
    if (!apiKey.trim()) { setError("请填写 API Key"); return; }
    if (!apiUrl.trim()) { setError("请填写 API URL"); return; }

    setSaving(true);
    setError("");
    try {
      await api.createCustomProvider({
        id: safeId,
        display_name: name.trim(),
        base_url: apiUrl.trim().replace(/\/+$/, ""),
        api_key: apiKey.trim(),
        models: modelInput.trim() || undefined,
      });
      const updated = await api.getCustomProviders();
      setProviders(updated);
      setSaving(false);
      setFetching(true);
      const resp = await api.fetchCustomProviderModels(safeId);
      if (resp.error) { setError(`获取失败: ${resp.error}`); setFetching(false); return; }
      const models = resp.models.map((m) => m.id).sort();
      if (models.length === 0) { setError("未返回任何模型"); setFetching(false); return; }
      setModelInput(models.join(", "));
      setProviderModels((prev) => ({ ...prev, [safeId]: models }));
      onModelsFetched(safeId, models);
      const probedWithWindow = resp.models.filter((m) => typeof m.context_window === "number" && (m.context_window as number) > 0);
      setConfig((prev) => {
        const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
        const newEnabled = [...prev.enabled];
        models.forEach((modelId) => {
          const key = `${safeId}:${modelId}`;
          if (!existing.has(key)) newEnabled.push({ providerId: safeId, modelId });
        });
        const currentContexts = prev.modelContexts ?? {};
        const nextContexts: Record<string, number> = { ...currentContexts };
        for (const m of probedWithWindow) {
          const key = `${safeId}:${m.id}`;
          if (!currentContexts[key] || currentContexts[key] === 0) {
            nextContexts[key] = m.context_window as number;
          }
        }
        const next = {
          ...prev,
          enabled: newEnabled,
          defaultModel: prev.defaultModel || (models.length > 0 ? `${safeId}:${models[0]}` : prev.defaultModel),
          modelContexts: nextContexts,
        };
        writeModelConfigAndNotify(next);
        if (probedWithWindow.length > 0) {
          api.updateCustomProviderModelContexts(safeId, nextContexts).catch((e) => { console.warn("[CustomProviderSection] sync probed contexts (new) failed:", e); });
        }
        return next;
      });
      api.getCustomProviders().then((list) => setProviders(list)).catch(() => { /* non-fatal */ });
      onRefresh();
      setSuccess(`成功获取 ${models.length} 个模型${probedWithWindow.length > 0 ? `, ${probedWithWindow.length} 个已自动填入探测的 context_window` : ""}`);
      setEditingId(null); setName(""); setApiUrl(""); setApiKey(""); setModelInput("");
    } catch (e: unknown) {
      if (e instanceof Error) {
        if (e.message.includes("获取失败")) { /* pass — will show on next line */ }
        else { setError(`保存或获取失败: ${e.message}`); }
      } else if (typeof e === 'string') {
        if (!e.includes("获取失败")) setError(`保存或获取失败: ${e}`);
      }
      setFetching(false);
    }
    setSaving(false);
    setFetching(false);
  };

  const handleSave = async () => {
    const targetId = editingId || safeId;
    if (!targetId) { setError("请填写提供商名称"); return; }
    if (!editingId && !apiKey.trim()) { setError("请填写 API Key"); return; }
    setSaving(true);
    setError("");
    try {
      await api.createCustomProvider({
        id: targetId,
        display_name: editingId ? name.trim() || undefined : name.trim(),
        base_url: apiUrl.trim().replace(/\/+$/, ""),
        api_key: editingId ? apiKey.trim() : apiKey.trim(),
        models: modelInput.trim() || undefined,
      });
      const updated = await api.getCustomProviders();
      setProviders(updated);
      if (modelInput.trim()) {
        const models = modelInput.split(",").map((m) => m.trim()).filter(Boolean);
        if (models.length > 0) {
          setProviderModels((prev) => ({ ...prev, [targetId]: models }));
          onModelsFetched(targetId, models);
          if (editingId) {
            setConfig((prev) => {
              const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
              const newEnabled = [...prev.enabled];
              let changed = false;
              models.forEach((modelId) => {
                const key = `${targetId}:${modelId}`;
                if (!existing.has(key)) {
                  newEnabled.push({ providerId: targetId, modelId });
                  changed = true;
                }
              });
              if (changed) {
                const next = { ...prev, enabled: newEnabled };
                writeModelConfigAndNotify(next);
                return next;
              }
              return prev;
            });
          }
        }
      }
      try {
        const corrected = await api.autoCorrectConfig();
        if (corrected.report) {
          setSuccess(editingId ? `已更新并自动路由：${corrected.message}` : `已保存并自动路由：${corrected.message}`);
        } else {
          setSuccess(editingId ? "已更新" : "已保存");
        }
      } catch { /* non-fatal */ }
      setConfig((prev) => {
        if (!prev.defaultModel) {
          const models = modelInput.split(",").map((m) => m.trim()).filter(Boolean);
          if (models.length > 0) {
            const next = { ...prev, defaultModel: `${targetId}:${models[0]}` };
            writeModelConfigAndNotify(next);
            return next;
          }
        }
        return prev;
      });
      onRefresh();
      setEditingId(null); setName(""); setApiUrl(""); setApiKey(""); setModelInput("");
    } catch (e) {
      setError(`保存失败: ${e instanceof Error ? e.message : String(e)}`);
    }
    setSaving(false);
  };

  return (
    <div className="p-6 space-y-6">
      <div>
        <h3 className="text-base font-semibold text-text-primary mb-1">自定义提供商</h3>
        <p className="text-xs text-text-muted">配置兼容 OpenAI API 的任意 LLM 服务</p>
      </div>

      {providers.length > 0 && (
        <div className="space-y-2">
          <div className="text-[11px] text-text-muted uppercase tracking-wider">已配置的提供商</div>
          {providers.map((p) => {
            const isExpanded = expandedProviderId === p.id;
            const models = customModels[p.id] ?? [];
            const enabledCount = models.filter((m) =>
              config.enabled.some((e) => `${e.providerId}:${e.modelId}` === `${p.id}:${m}`)
            ).length;
            return (
              <div key={p.id} className="rounded-lg bg-bg-card border border-border-primary overflow-hidden">
                <div className="flex items-stretch gap-2 px-3 py-2 text-xs">
                  <button
                    onClick={() => setExpandedProviderId(isExpanded ? null : p.id)}
                    aria-expanded={isExpanded}
                    className="flex items-center gap-2 flex-1 min-w-0 text-left hover:text-accent transition-colors"
                  >
                    <ChevronRight size={14} className={`shrink-0 text-text-muted transition-transform duration-150 ${isExpanded ? "rotate-90" : ""}`} />
                    <div className="flex-1 min-w-0">
                      <div className="font-medium text-text-primary truncate flex items-center gap-1.5">
                        {p.display_name || p.id}
                        {p.has_key && <span className="w-1.5 h-1.5 rounded-full bg-green-400 inline-block shrink-0" />}
                      </div>
                      <div className="text-[10px] text-text-muted truncate mt-0.5">
                        {p.base_url || "—"} · {models.length} 模型{models.length > 0 ? ` · 已启用 ${enabledCount}` : ""}
                      </div>
                    </div>
                  </button>
                  <div className="flex items-center gap-1 shrink-0">
                    <button onClick={(e) => { e.stopPropagation(); handleTestConnection(p.id); }} disabled={testing === p.id} className="px-2 py-1 rounded text-[10px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10 disabled:opacity-40">
                      {testing === p.id ? "..." : "测试"}
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); handleFetchModelsForSavedProvider(p.id); }} disabled={fetching} className="px-2 py-1 rounded text-[10px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10 disabled:opacity-40">
                      模型
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); handleEdit(p); }} className="px-2 py-1 rounded text-[10px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10">
                      编辑
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); handleDelete(p.id, p.models); }} className="px-2 py-1 rounded text-[10px] bg-red-500/10 text-red-400 hover:bg-red-500/20">
                      删除
                    </button>
                  </div>
                </div>
                {isExpanded && (
                  <div className="border-t border-border-primary p-3 bg-bg-primary">
                    {models.length === 0 ? (
                      <div className="text-[11px] text-text-muted italic text-center py-3">
                        暂无模型,点上方「模型」按钮重新获取
                      </div>
                    ) : (
                      <div className="grid grid-cols-2 gap-1.5 max-h-64 overflow-y-auto">
                        {models.map((modelId) => {
                          const fullId = `${p.id}:${modelId}`;
                          const isEnabled = config.enabled.some((m) => `${m.providerId}:${m.modelId}` === fullId);
                          const isDefault = config.defaultModel === fullId;
                          const ctx = config.modelContexts?.[fullId];
                          return (
                            <div key={modelId} className={`flex items-center gap-1.5 px-2 py-1.5 rounded text-[11px] ${isDefault ? "bg-accent/10 border border-accent/30" : "hover:bg-bg-tertiary"}`}>
                              <button onClick={() => {
                                toggleModel(p.id, modelId);
                                const wasEnabled = config.enabled.some((m) => `${m.providerId}:${m.modelId}` === fullId);
                                const newCount = wasEnabled ? config.enabled.length - 1 : config.enabled.length + 1;
                                setSuccess(wasEnabled ? `✓ 已取消 · 剩 ${newCount} 个模型启用中` : `✓ 已保存 · 共 ${newCount} 个模型启用中`);
                                setTimeout(() => setSuccess(""), 2500);
                              }} className={`w-4 h-4 rounded border-2 flex items-center justify-center shrink-0 transition-colors ${isEnabled ? "bg-accent border-accent" : "bg-transparent border-text-muted hover:border-accent"}`}>
                                {isEnabled
                                  ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M2 5l2 2 4-4" stroke="white" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"/></svg>
                                  : null}
                              </button>
                              <span className={`flex-1 min-w-0 truncate font-mono ${isEnabled ? "text-text-primary" : "text-text-muted"}`}>{modelId}</span>
                              {isEnabled && (
                                <>
                                  <select
                                    value={ctx ?? ""}
                                    onChange={(e) => {
                                      const v = e.target.value;
                                      setConfig((prev) => {
                                        const next = { ...prev, modelContexts: { ...(prev.modelContexts ?? {}), [fullId]: v ? Number(v) : 0 } };
                                        storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(next));
                                        return next;
                                      });
                                      api.updateCustomProviderModelContexts(p.id, { ...config.modelContexts, [fullId]: v ? Number(v) : 0 }).catch((e) => { console.warn("[SettingsView] modelContext sync failed:", e); });
                                    }}
                                    title="上下文窗口大小；空 = 后端 catalog 兜底（通常 128K）"
                                    className="bg-bg-tertiary border border-border-secondary rounded px-1 py-0.5 text-[9px] text-text-muted outline-none"
                                  >
                                    <option value="">{defaultOptionLabel(resolveContextWindow(modelId, ctx, p.model_contexts?.[modelId]))}</option>
                                    {CONTEXT_WINDOW_OPTIONS.map((o) => (
                                      <option key={o.value} value={o.value}>{o.label}</option>
                                    ))}
                                  </select>
                                  <button onClick={() => {
                                    setDefault(fullId);
                                    setSuccess(`✓ 默认模型已设为 ${modelId}`);
                                    setTimeout(() => setSuccess(""), 2500);
                                  }} className={`shrink-0 px-1.5 py-0.5 rounded text-[9px] ${isDefault ? "bg-accent text-accent-on-text" : "border border-text-muted text-text-secondary hover:border-accent hover:text-accent"}`}>
                                    {isDefault ? "默认" : "设为默认"}
                                  </button>
                                </>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {error && (
        <div className="flex items-center gap-2 px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/30 text-sm text-red-400">
          <AlertCircle size={14} /> {error}
        </div>
      )}
      {success && (
        <div className="flex items-center gap-2 px-3 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/30 text-sm text-emerald-400">
          <CheckCircle size={14} /> {success}
        </div>
      )}

      {/* ── Add new / Edit provider form ── */}
      <div className="space-y-3 pt-2 border-t border-border-primary">
        <div className="flex items-center justify-between">
          <div className="text-[11px] text-text-muted uppercase tracking-wider">
            {editingId ? "编辑提供商" : "新建提供商"}
          </div>
          {editingId && (
            <button onClick={handleCancelEdit} className="text-[10px] text-text-muted hover:text-text-primary">
              取消编辑
            </button>
          )}
        </div>

        <div>
          <label className="block text-xs font-medium text-text-primary mb-1">名称</label>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如：我的服务"
            className="w-full bg-bg-tertiary border border-border-primary rounded-lg px-3 py-2 text-sm text-text-primary placeholder-text-muted outline-none"
          />
          {!editingId && safeId && <p className="text-[10px] text-text-muted mt-1">模型 ID 前缀: {safeId}:</p>}
          {editingId && <p className="text-[10px] text-text-muted mt-1">提供商 ID: {editingId}</p>}
        </div>

        <div>
          <label className="block text-xs font-medium text-text-primary mb-1">API URL</label>
          <input
            value={apiUrl}
            onChange={(e) => setApiUrl(e.target.value)}
            placeholder="https://api.example.com/v1"
            className="w-full bg-bg-tertiary border border-border-primary rounded-lg px-3 py-2 text-sm text-text-primary placeholder-text-muted outline-none"
          />
        </div>

        <div>
          <label className="block text-xs font-medium text-text-primary mb-1">API Key</label>
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder={editingId ? "留空则保持原 Key 不变" : "sk-..."}
            className="w-full bg-bg-tertiary border border-border-primary rounded-lg px-3 py-2 text-sm text-text-primary placeholder-text-muted outline-none"
          />
        </div>

        <div>
          <label className="block text-xs font-medium text-text-primary mb-1">模型名称</label>
          <div className="flex gap-2">
            <input
              value={modelInput}
              onChange={(e) => setModelInput(e.target.value)}
              placeholder="gpt-4o, claude-3-opus（多个用逗号分隔）"
              className="flex-1 bg-bg-tertiary border border-border-primary rounded-lg px-3 py-2 text-sm text-text-primary placeholder-text-muted outline-none"
            />
            <button
              onClick={handleFetchModels}
              disabled={fetching || saving || !apiUrl.trim() || !apiKey.trim()}
              className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-sm bg-accent hover:bg-accent-hover text-accent-on-text disabled:opacity-40"
            >
              {fetching ? <RefreshCw size={14} className="animate-spin" /> : saving ? "保存中..." : <Check size={14} />}
              {fetching ? "获取中" : saving ? "保存中..." : "获取模型"}
            </button>
          </div>
          <p className="text-[10px] text-text-muted mt-1">填写名称、API URL 和 API Key 后点击"获取模型"自动填充，或手动输入</p>
        </div>

        <button
          onClick={handleSave}
          disabled={saving || (!editingId && !apiKey.trim())}
          className="flex items-center gap-2 px-4 py-2 rounded-lg bg-accent hover:bg-accent-hover text-accent-on-text text-sm disabled:opacity-40"
        >
          <Save size={14} /> {saving ? "保存中..." : editingId ? "更新配置" : "保存配置"}
        </button>
      </div>

    </div>
  );
}

// ── Custom Providers Tab (独立 sub-tab,与 ModelTab 并列) ──
//
// 单一权威来源 = localStorage。挂载时立即从 loadModelCache() 拉一份
// （修旧 bug：useState({}) 导致切回 tab 后模型列表空白）。
export function CustomProvidersTab() {
  const safeSetTimeout = useSafeTimeout();
  const [config, setConfig] = useState<ModelConfigData>(loadModelConfig);
  // BUGFIX: 旧版 useState({}) 造成切回 tab 时「暂无模型」。挂载即从缓存拉，
  //       任何写后即可见；MODEL_CONFIG_CHANGED_EVENT 也会再刷新一次。
  const [customModels, setCustomModels] = useState<Record<string, string[]>>(loadModelCache);

  // 监听其他 tab/组件写入的 MODEL_CONFIG_CHANGED_EVENT,把本组件的
  // config + 缓存都重新拉一遍(单一权威来源 = localStorage)。
  useEffect(() => {
    const refresh = () => {
      try {
        const raw = storage.getItem(MODEL_CONFIG_KEY);
        if (raw) setConfig({ modelContexts: {}, ...JSON.parse(raw) });
        setCustomModels(loadModelCache());
      } catch {}
    };
    const onChanged = () => refresh();
    window.addEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
    const onStorage = (e: StorageEvent) => { if (e.key === MODEL_CONFIG_KEY) refresh(); };
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  const toggleModel = useCallback((providerId: string, modelId: string) => {
    setConfig((prev) => {
      const key = `${providerId}:${modelId}`;
      const exists = prev.enabled.some((m) => `${m.providerId}:${m.modelId}` === key);
      const newEnabled = exists
        ? prev.enabled.filter((m) => `${m.providerId}:${m.modelId}` !== key)
        : [...prev.enabled, { providerId, modelId }];
      const defaultCleared = exists && prev.defaultModel === key;
      const newConfig: ModelConfigData = { enabled: newEnabled, defaultModel: defaultCleared ? "" : prev.defaultModel, modelContexts: prev.modelContexts ?? {} };
      safeSetTimeout(async () => {
        writeModelConfigAndNotify(newConfig);
        if (defaultCleared) {
          try {
            const backendCfg = await api.getConfig();
            backendCfg.model = "";
            await api.updateConfig(backendCfg);
          } catch (e) { console.warn("CustomProvidersTab toggleModel: sync cleared default failed", e); }
        }
      }, 0);
      return newConfig;
    });
  }, []);

  const setDefault = useCallback((fullId: string) => {
    setConfig((prev) => {
      const newConfig = { ...prev, defaultModel: fullId };
      safeSetTimeout(async () => {
        writeModelConfigAndNotify(newConfig);
        try {
          const backendCfg = await api.getConfig();
          backendCfg.model = fullId;
          await api.updateConfig(backendCfg);
        } catch (e) { console.warn("CustomProvidersTab setDefault: sync model failed", e); }
      }, 0);
      return newConfig;
    });
  }, []);

  return (
    <div className="p-6">
      <CustomProviderSection
        customModels={customModels}
        onModelsFetched={(id, models) => setCustomModels((prev) => ({ ...prev, [id]: models }))}
        onRefresh={() => { /* no-op: CustomProvidersTab 不依赖 activeKeys */ }}
        toggleModel={toggleModel}
        setDefault={setDefault}
        config={config}
        setConfig={setConfig}
      />
    </div>
  );
}
