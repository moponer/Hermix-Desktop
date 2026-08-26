import { useState, useEffect, useCallback } from "react";
import { Save, Check, AlertCircle, RefreshCw, ChevronRight, CheckCircle } from "lucide-react";
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

export function ModelTab() {
  const safeSetTimeout = useSafeTimeout();
  const [config, setConfig] = useState<ModelConfigData>(loadModelConfig);
  const [selectedProvider, setSelectedProvider] = useState<ProviderDef | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [activeKeys, setActiveKeys] = useState<Record<string, boolean>>({});
  const [fetchedModels, setFetchedModels] = useState<Record<string, string[]>>(loadModelCache);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [info, setInfo] = useState("");
  // Track which custom provider IDs have an API key configured (from
  // /api/custom-providers → has_key). This is the authoritative source
  // because it reads config.yaml directly, unlike GET /api/env which
  // may read the wrong path before the backend is recompiled.
  const [customIdsWithKey, setCustomIdsWithKey] = useState<Set<string>>(new Set());
  // Load custom provider has_key state on mount. (Model list fetching /
  // toggle 逻辑已搬到独立 <CustomProvidersTab /> sub-tab,这里只保留状态栏
  // 用的 has_key 提示。)
  useEffect(() => {
    api.getCustomProviders().then((providers) => {
      const withKey = new Set<string>();
      providers.forEach((p) => { if (p.has_key) withKey.add(p.id); });
      setCustomIdsWithKey(withKey);
    }).catch((e) => { console.warn("[SettingsView] custom provider fetch failed:", e); });
  }, []);

  useEffect(() => {
    api.getEnvVars().then(async (envVars) => {
      const keys: Record<string, boolean> = {};
      PROVIDERS.forEach((p) => { keys[p.id] = !!envVars[p.keyEnvVar]?.is_set; });
      // Also check custom provider env vars.
      // Custom providers store keys as `CUSTOM_{id}_API_KEY`.
      // We extract the provider id (`custom-{id}`) and mark it as configured.
      // NOTE: env var names are UPPERCASE, but provider ids are lowercase
      // (sanitized from the user-supplied name via `.toLowerCase()`).
      // We must normalise the extracted suffix to lowercase so the
      // activeKeys key matches the provider id used everywhere else.
      for (const [varName, info] of Object.entries(envVars ?? {})) {
        const m = /^CUSTOM_([A-Za-z0-9_-]+)_API_KEY$/.exec(varName);
        if (m && (info as { is_set?: boolean })?.is_set) {
          // Strip any existing "custom-" prefix from the captured suffix
          // to prevent double-prefix like "custom-custom-agnes-...".
          const raw = m[1].toLowerCase().replace(/_/g, "-");
          const normalized = raw.replace(/^custom-/, "");
          keys[`custom-${normalized}`] = true;
        }
      }
      setActiveKeys(keys);

      // mount 时先从 localStorage 缓存恢复模型列表：若 API fetch 静默失败
      // （网络/TLS/DNS），不读缓存会让 fetchedModels 永远为空，UI 不显示任何模型。
      const cached = loadModelCache();
      if (Object.keys(cached).length > 0) {
        setFetchedModels(cached);
        // 从缓存恢复 config（如果有）。
        const cachedCfg = loadModelConfig();
        if (cachedCfg.enabled.length > 0) {
          setConfig(cachedCfg);
        }
      }

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
              console.warn(`[SettingsView] reveal failed for ${p.id}: HTTP ${revealResp.status}`);
              if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
              continue;
            }
            const { value: apiKey } = await revealResp.json();
            if (!apiKey) {
              console.warn(`[SettingsView] reveal returned empty key for ${p.id}`);
              break;
            }

            const modelsResp = await fetch(`${p.baseUrl}/models`, {
              headers: { Authorization: `Bearer ${apiKey}` },
            });
            if (!modelsResp.ok) {
              console.warn(`[SettingsView] models fetch failed for ${p.id}: HTTP ${modelsResp.status}`);
              if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
              continue;
            }
            const data = await modelsResp.json();
            const models: string[] = (data.data || []).map((m: { id: string }) => m.id).sort();
            if (models.length > 0) {
              console.info(`[SettingsView] fetched ${models.length} models for ${p.id}:`, models);
              setFetchedModels((prev) => ({ ...prev, [p.id]: models }));
              setConfig((prev) => {
                const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
                const newEnabled = [...prev.enabled];
                models.forEach((modelId) => {
                  if (!existing.has(`${p.id}:${modelId}`)) newEnabled.push({ providerId: p.id, modelId });
                });
                const newDefault = prev.defaultModel || (newEnabled.length > 0 ? `${p.id}:${models[0]}` : "");
                const next = { enabled: newEnabled, defaultModel: newDefault, modelContexts: prev.modelContexts ?? {} };
                writeModelConfigAndNotify(next);
                return next;
              });
            } else {
              console.warn(`[SettingsView] ${p.id} returned 0 models`);
            }
            break; // success or empty — don't retry
          } catch (err) {
            console.error(`[SettingsView] fetch error for ${p.id} (attempt ${attempt + 1}):`, err);
            if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
          }
        }
      });
      await Promise.allSettled(fetchPromises);

      // 延迟重试：如果 fetch 全失败（后端可能还没 ready），
      // 3 秒后再试一次。Tauri 启动时 Rust server 初始化需要几秒，
      // SettingsView mount 时 3000 端口可能还没 accept connections。
      const configuredProviderIds = PROVIDERS.filter((p) => keys[p.id]).map((p) => p.id);
      if (configuredProviderIds.length > 0) {
        safeSetTimeout(async () => {
          try {
            const envVars2 = await api.getEnvVars();
            const keys2: Record<string, boolean> = {};
            PROVIDERS.forEach((p) => { keys2[p.id] = !!envVars2[p.keyEnvVar]?.is_set; });
            for (const p of PROVIDERS.filter((p) => keys2[p.id])) {
              try {
                const revealResp = await fetch(apiUrl("/api/env/reveal"), {
                  method: "POST",
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify({ key: p.keyEnvVar }),
                });
                if (!revealResp.ok) continue;
                const { value: apiKey } = await revealResp.json();
                if (!apiKey) continue;
                const modelsResp = await fetch(`${p.baseUrl}/models`, {
                  headers: { Authorization: `Bearer ${apiKey}` },
                });
                if (!modelsResp.ok) continue;
                const data = await modelsResp.json();
                const models: string[] = (data.data || []).map((m: { id: string }) => m.id).sort();
                if (models.length > 0) {
                  console.info(`[SettingsView] retry fetched ${models.length} models for ${p.id}`);
                  setFetchedModels((prev) => ({ ...prev, [p.id]: models }));
                  setConfig((prev) => {
                    const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
                    const newEnabled = [...prev.enabled];
                    models.forEach((mid) => {
                      if (!existing.has(`${p.id}:${mid}`)) newEnabled.push({ providerId: p.id, modelId: mid });
                    });
                    const next = { enabled: newEnabled, defaultModel: prev.defaultModel || `${p.id}:${models[0]}`, modelContexts: prev.modelContexts ?? {} };
                    writeModelConfigAndNotify(next);
                    return next;
                  });
                }
              } catch (err) {
                console.error(`[SettingsView] delayed retry error for ${p.id}:`, err);
              }
            }
          } catch {}
        }, 3000);
      }
    }).catch((err) => { console.error("[SettingsView] mount env fetch error:", err); });
  }, []);

  useEffect(() => { saveModelCache(fetchedModels); }, [fetchedModels]);

  const selectProvider = (p: ProviderDef) => {
    setSelectedProvider(p);
    setKeyInput("");
    setError("");
  };

  const fetchModels = useCallback(async () => {
    if (!selectedProvider) return;
    const key = keyInput.trim();
    if (!key) { setError("请填写 API Key"); return; }
    setLoading(true);
    setError("");
    setInfo("");
    try {
      const resp = await fetch(`${selectedProvider.baseUrl}/models`, { headers: { Authorization: `Bearer ${key}` } });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data = await resp.json();
      const models: string[] = (data.data || []).map((m: { id: string }) => m.id).sort();
      if (models.length === 0) { setError("未返回任何模型"); setLoading(false); return; }
      setFetchedModels((prev) => ({ ...prev, [selectedProvider.id]: models }));
      await api.setEnvVar(selectedProvider.keyEnvVar, key);
      setActiveKeys((prev) => ({ ...prev, [selectedProvider.id]: true }));
      setConfig((prev) => {
        const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
        const newEnabled = [...prev.enabled];
        models.forEach((modelId) => { if (!existing.has(`${selectedProvider.id}:${modelId}`)) newEnabled.push({ providerId: selectedProvider.id, modelId }); });
        const next = { ...prev, enabled: newEnabled };
        // 与 toggleModel / setDefault 一致，setConfig 之后立即 writeModelConfigAndNotify 同步出去。
        writeModelConfigAndNotify(next);
        return next;
      });
      try {
        const corrected = await api.autoCorrectConfig();
        if (corrected.report) { setInfo(`✅ 已自动路由：${corrected.message}`); }
      } catch (autoErr) { console.warn("auto-correct failed:", autoErr); }
    } catch (e) { setError(`获取失败: ${e instanceof Error ? e.message : String(e)}`); }
    setLoading(false);
  }, [selectedProvider, keyInput]);

  // ── Toggle model enable/disable (auto-saves) ──
  const toggleModel = useCallback((providerId: string, modelId: string) => {
    setConfig((prev) => {
      const key = `${providerId}:${modelId}`;
      const exists = prev.enabled.some((m) => `${m.providerId}:${m.modelId}` === key);
      const newEnabled = exists
        ? prev.enabled.filter((m) => `${m.providerId}:${m.modelId}` !== key)
        : [...prev.enabled, { providerId, modelId }];
      const defaultCleared = exists && prev.defaultModel === key;
      const newConfig: ModelConfigData = { enabled: newEnabled, defaultModel: defaultCleared ? "" : prev.defaultModel, modelContexts: prev.modelContexts ?? {} };
      // Auto-save every toggle immediately — no separate "save" step.
      safeSetTimeout(async () => {
        writeModelConfigAndNotify(newConfig);
        // If the default model was just disabled, sync the cleared
        // default to the backend so the agent loop doesn't keep
        // using a model the user explicitly turned off.
        if (defaultCleared) {
          try {
            const backendCfg = await api.getConfig();
            backendCfg.model = "";
            await api.updateConfig(backendCfg);
          } catch (e) {
            console.warn("toggleModel: failed to sync cleared default to backend", e);
          }
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
        // Also sync the model to the backend so the agent loop
        // uses it immediately — otherwise the frontend localStorage
        // config and the backend config.yaml drift apart.
        try {
          const backendCfg = await api.getConfig();
          backendCfg.model = fullId;
          await api.updateConfig(backendCfg);
        } catch (e) {
          console.warn("setDefault: failed to sync model to backend", e);
        }
      }, 0);
      return newConfig;
    });
  }, []);

  // ── Status banner ──
  let currentDefault = config.defaultModel;
  // Defensive repair: if defaultModel looks like "providerIdmodelId" (no
  // colon separator — can happen with stale localStorage or old backend
  // values), try to split on a known custom provider id boundary.
  if (currentDefault && !currentDefault.includes(":")) {
    for (const cpId of customIdsWithKey) {
      if (currentDefault.startsWith(cpId) && currentDefault.length > cpId.length) {
        currentDefault = `${cpId}:${currentDefault.slice(cpId.length)}`;
        break;
      }
    }
  }
  const currentProvider = currentDefault ? currentDefault.split(":")[0] : null;
  const configuredProviders = PROVIDERS.filter((p) => activeKeys[p.id]);
  const currentProviderConfigured = currentProvider
    ? currentProvider.startsWith("custom-")
      // Two sources: (1) env-var scan (works after backend recompile),
      // (2) /api/custom-providers has_key (works immediately).
      ? !!activeKeys[currentProvider] || customIdsWithKey.has(currentProvider)
      : configuredProviders.some((p) => p.id === currentProvider)
    : false;
  const noDefaultAtAll = !currentDefault;
  const mismatchDetected = noDefaultAtAll || (currentProvider != null && !currentProviderConfigured);
  // Also count custom providers that have keys configured (stored in activeKeys
  // by the CUSTOM_*_API_KEY scan at mount). This ensures the banner still shows
  // mismatch warnings even when only custom providers are available.
  const customConfiguredCount = Object.keys(activeKeys).filter(
    (k) => k.startsWith("custom-") && activeKeys[k]
  ).length;
  const hasAnyConfigured = configuredProviders.length > 0 || customConfiguredCount > 0;
  const showMismatchBanner = mismatchDetected && (hasAnyConfigured || noDefaultAtAll);

  return (
    <div className="flex flex-col h-full">
      {showMismatchBanner && (
        <div className="flex items-start gap-3 px-4 py-3 border-b border-warning/30 bg-warning-soft text-text-primary" role="alert">
          <AlertCircle size={16} className="mt-0.5 shrink-0 text-warning" />
          <div className="flex-1 min-w-0 text-xs leading-relaxed">
            <div className="font-semibold mb-1 text-warning">
              {noDefaultAtAll ? "尚未设置默认模型" : `默认模型「${currentDefault}」使用的 provider 未配置`}
            </div>
            <div className="text-text-secondary">
              {noDefaultAtAll ? "从左侧选一个供应商并点击「获取模型」即可启用。已配置的供应商：" : "已配置的供应商："}
              {(configuredProviders.length > 0 || customConfiguredCount > 0) ? (
                <span className="font-medium text-warning">
                  {[
                    ...configuredProviders.map((p) => p.name),
                    ...Object.keys(activeKeys).filter((k) => k.startsWith("custom-") && activeKeys[k]),
                  ].join("、")}
                </span>
              ) : <span className="italic">无</span>}
              。{noDefaultAtAll ? "保存后即可开始对话。" : "你可以在下方「可用模型」中点 ★ 设为默认，或在左侧为该 provider 添加 Key。"}
            </div>
          </div>
        </div>
      )}
      <div className="flex flex-1 min-h-0">
      {/* 左侧：供应商列表 */}
      <div className="w-56 border-r border-border-primary bg-bg-primary overflow-y-auto shrink-0">
        <div className="py-1">
          <div className="text-[11px] text-text-muted uppercase tracking-wider mb-1 px-2">供应商</div>
          {PROVIDERS.map((p) => {
            const isActive = selectedProvider?.id === p.id;
            const connected = activeKeys[p.id];
            return (
              <button key={p.id} onClick={() => selectProvider(p)} className={`w-full flex items-center gap-2 px-2 py-2 rounded text-left text-sm ${isActive ? "bg-accent/10 text-accent" : "text-text-secondary hover:bg-bg-tertiary hover:text-text-primary"}`}>
                <ProviderIcon providerId={p.id} size={18} title={p.name} />
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-xs truncate">{p.name}</div>
                  <div className="text-[10px] text-text-muted truncate">{p.description}</div>
                </div>
                {connected && <span className="w-1.5 h-1.5 rounded-full bg-green-400 shrink-0" />}
                <ChevronRight size={12} className="text-text-muted shrink-0" />
              </button>
            );
          })}
          {/* "自定义提供商"已提升为顶部独立 sub-tab(由 SystemConfigPanel
              的 modelTab="custom" 渲染),不在左栏底部展示。
              旧实现注释保留参考价值:这里原本渲染一个按钮 + 每 provider
              卡片,与右栏 CustomProviderSection 重复。 */}
        </div>
      </div>

      {/* 右侧：选中供应商的配置。"自定义"已提升为独立 sub-tab,
          不再在左栏底部(避免埋在 builtin 列表末尾看不见)。原本 selectedProvider
          === "_custom" 会渲染内嵌的 CustomProviderSection,现已成为死路径,
          ModelTab 只剩 builtin provider 的 key 输入 + 模型列表。 */}
      <div className="flex-1 overflow-y-auto min-h-0">
        {selectedProvider ? (
          <div className="p-6 max-w-xl">
            <h3 className="text-base font-semibold text-text-primary mb-1">{selectedProvider.name}</h3>
            <p className="text-xs text-text-muted mb-4">{selectedProvider.description}</p>

            {error && (
              <div className="flex items-center gap-2 px-3 py-2 mb-4 rounded-lg bg-red-500/10 border border-red-500/30 text-sm text-red-400">
                <AlertCircle size={14} /> {error}
              </div>
            )}
            {info && (
              <div className="flex items-center gap-2 px-3 py-2 mb-4 rounded-lg bg-emerald-500/10 border border-emerald-500/30 text-sm text-emerald-400">
                <CheckCircle size={14} /> {info}
              </div>
            )}

            <div className="mb-4">
              <label className="block text-xs font-medium text-text-primary mb-1">API Key</label>
                <div className="flex gap-2">
                  <input
                    type="password"
                    value={keyInput}
                    onChange={(e) => setKeyInput(e.target.value)}
                    placeholder={activeKeys[selectedProvider.id] ? "已配置，输入新 Key 以更新" : `输入 ${selectedProvider.name} API Key...`}
                    className="flex-1 bg-bg-tertiary border border-border-primary rounded-lg px-3 py-2 text-sm text-text-primary placeholder-text-muted outline-none"
                    onKeyDown={(e) => e.key === "Enter" && fetchModels()}
                  />
                  <button
                    onClick={fetchModels}
                    disabled={loading || !keyInput.trim()}
                    className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-sm bg-accent hover:bg-accent-hover text-accent-on-text disabled:opacity-40"
                  >
                    {loading ? <RefreshCw size={14} className="animate-spin" /> : <Check size={14} />}
                    {loading ? "获取中" : "获取模型"}
                  </button>
                </div>
              </div>

              {/* 模型列表 */}
              {fetchedModels[selectedProvider.id] && (
                <div>
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-xs font-medium text-text-primary">可用模型（{fetchedModels[selectedProvider.id].length}）</span>
                  </div>
                  <div className="space-y-1 max-h-80 overflow-y-auto rounded-lg border border-border-primary bg-bg-card p-2">
                    {fetchedModels[selectedProvider.id].map((modelId) => {
                      const fullId = `${selectedProvider.id}:${modelId}`;
                      const isEnabled = config.enabled.some((m) => `${m.providerId}:${m.modelId}` === fullId);
                      const isDefault = config.defaultModel === fullId;
                      const ctx = config.modelContexts?.[fullId];
                      return (
                        <div key={modelId} className={`flex items-center gap-2 px-3 py-2 rounded-lg text-xs ${isDefault ? "bg-accent/10 border border-accent/30" : "hover:bg-bg-tertiary"}`}>
                          <button onClick={() => {
                            toggleModel(selectedProvider.id, modelId);
                            const wasEnabled = config.enabled.some((m) => `${m.providerId}:${m.modelId}` === fullId);
                            const newCount = wasEnabled ? config.enabled.length - 1 : config.enabled.length + 1;
                            setInfo(wasEnabled ? `✓ 已取消 · 剩 ${newCount} 个模型启用中` : `✓ 已保存 · 共 ${newCount} 个模型启用中`);
                            setTimeout(() => setInfo(""), 2500);
                          }} className={`w-4 h-4 rounded border-2 flex items-center justify-center transition-colors ${isEnabled ? "bg-accent border-accent" : "bg-transparent border-text-muted hover:border-accent"}`}>
                            {isEnabled
                              ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M2 5l2 2 4-4" stroke="white" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"/></svg>
                              : null}
                          </button>
                          <span className={`flex-1 font-mono min-w-0 truncate ${isEnabled ? "text-text-primary" : "text-text-muted"}`}>{modelId}</span>
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
                                  // Sync to backend for custom providers
                                  if (selectedProvider?.id.startsWith("custom-")) {
                                    const allCtx = { ...config.modelContexts, [fullId]: v ? Number(v) : 0 };
                                    api.updateCustomProviderModelContexts(selectedProvider.id, allCtx).catch((e) => { console.warn("[SettingsView] modelContext sync failed:", e); });
                                  }
                                }}
                                title="上下文窗口大小；空 = 后端 catalog 兜底（通常 128K）"
                                className="bg-bg-tertiary border border-border-secondary rounded px-1 py-0.5 text-[10px] text-text-muted outline-none"
                              >
                                <option value="">{defaultOptionLabel(resolveContextWindow(modelId, ctx, null))}</option>
                                {CONTEXT_WINDOW_OPTIONS.map((o) => (
                                  <option key={o.value} value={o.value}>{o.label}</option>
                                ))}
                              </select>
                              <button onClick={() => setDefault(fullId)} className={`px-2 py-0.5 rounded text-[10px] ${isDefault ? "bg-accent text-accent-on-text" : "border border-border-secondary text-text-muted hover:border-accent hover:text-accent"}`}>
                                {isDefault ? "默认" : "设为默认"}
                              </button>
                            </>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}

              {/* Config is auto-saved on every toggle/set-default. */}
            </div>
          ) : (
            <div className="flex items-center justify-center h-full text-text-muted text-sm">← 选择一个供应商进行配置</div>
          )}
      </div>
      </div>
    </div>
  );
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
  // 避免多 provider 时无限往下撑 + 与对话侧"按 provider 分组"心智模型一致。
  const [expandedProviderId, setExpandedProviderId] = useState<string | null>(null);
  const [, setProviderModels] = useState<Record<string, string[]>>({});

  useEffect(() => {
    api.getCustomProviders().then((list) => setProviders(list)).catch((e) => { console.warn("[SettingsView] getCustomProviders failed:", e); });
  }, []);

  const safeId = name.trim()
    ? `custom-${name.trim().toLowerCase().replace(/[^a-z0-9\u4e00-\u9fff_-]/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "")}`
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
      // 把后端探测的 per-model context_window merge 进 config.modelContexts
      // —— 只填用户**没显式设过**的（`config.modelContexts[fullId]` 缺失
      // 或 0）。后端 fetch_models 已把探测值持久化到 config.yaml 的
      // `LlmProviderConfig.model_contexts`，但前端 localStorage 不知道；
      // 这一步把"探测值"同步到 localStorage，UI 才能在 select 默认选项
      // 显示"默认 (200K·探测)"。
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
          // 同步到后端（custom provider 走 PUT /api/custom-providers/{id}/model-contexts）
          api.updateCustomProviderModelContexts(id, nextContexts).catch((e) => { console.warn("[CustomProviderSection] sync probed contexts failed:", e); });
          return nextCfg;
        });
      }
      // 刷新 provider 列表（拿到最新 model_contexts 用于 select 默认项展示）
      api.getCustomProviders().then((list) => setProviders(list)).catch(() => { /* non-fatal */ });
      onRefresh();
      // 与 reconcile 的"用户主动勾选才 enable"原则一致:fetched 后不
      // 主动 writeModelConfigAndNotify(关于 enabled),让用户在「自定义模型」
      // 区域手动勾选要启用的。已存在于 config.enabled 的保持不动,新
      // 拉到的默认未勾选。
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
      // Step 1: persist the provider
      await api.createCustomProvider({
        id: safeId,
        display_name: name.trim(),
        base_url: apiUrl.trim().replace(/\/+$/, ""),
        api_key: apiKey.trim(),
        models: modelInput.trim() || undefined,
      });
      // Step 2: refresh list
      const updated = await api.getCustomProviders();
      setProviders(updated);
      // Step 3: fetch models
      setSaving(false);
      setFetching(true);
      const resp = await api.fetchCustomProviderModels(safeId);
      if (resp.error) { setError(`获取失败: ${resp.error}`); setFetching(false); return; }
      const models = resp.models.map((m) => m.id).sort();
      if (models.length === 0) { setError("未返回任何模型"); setFetching(false); return; }
      setModelInput(models.join(", "));
      setProviderModels((prev) => ({ ...prev, [safeId]: models }));
      onModelsFetched(safeId, models);
      // 新建流程同步 merge 探测的 per-model context_window（与已配置 provider
      // 处理路径一致：后端已持久化到 config.yaml，前端 localStorage 也要跟上）
      const probedWithWindow = resp.models.filter((m) => typeof m.context_window === "number" && (m.context_window as number) > 0);
      // 新建流程一次性 commit:把 fetched models 默认加进 enabled。
      // updateCustomModels 改完不主动 push,这里由"提交"动作显式
      // 写一次 enabled(等同于"用户预勾选 fetched models")。
      setConfig((prev) => {
        const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
        const newEnabled = [...prev.enabled];
        models.forEach((modelId) => {
          const key = `${safeId}:${modelId}`;
          if (!existing.has(key)) newEnabled.push({ providerId: safeId, modelId });
        });
        // 把探测的 context_window 同步进 modelContexts（新建流程 modelContexts 必为空，全填）
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
        // 同步探测值到后端（与 handleFetchModelsForSavedProvider 一致）
        if (probedWithWindow.length > 0) {
          api.updateCustomProviderModelContexts(safeId, nextContexts).catch((e) => { console.warn("[CustomProviderSection] sync probed contexts (new) failed:", e); });
        }
        return next;
      });
      // 刷新 provider 列表（拿到最新 model_contexts 给 select 默认项展示）
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
          // 编辑模式 commit:把新加入的 model push 进 enabled(用户主动
          // 编辑 modelInput 后 commit,与 reconcile 不主动 union 原则一致——
          // 没在 modelInput 里出现的就不 push,已存在的保持)
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
      // Set default to first model if not yet set
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

      {/* 已配置的提供商(默认折叠,互斥展开,展开后才显示模型勾选区)
          替代旧的"已配置的提供商"+"自定义模型"两块独立渲染。
          - 折叠态:provider 名字 + URL + 模型数 + 已启用数 + 操作按钮
          - 展开态:头部收起,下方显示该 provider 的模型勾选区(2 列网格)
          - 同一时刻只有一个 provider 展开(expandedProviderId 互斥)
          - 头部操作按钮(测试/模型/编辑/删除)stopPropagation,不会触发展开 */}
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
                  {/* 头部可点击区:展开/折叠 */}
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
                  {/* 操作按钮组:stopPropagation 防误触展开 */}
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
                                // 视觉反馈:绿色"已保存"banner,2.5s 自动清。
                                // 否则主人以为勾选没生效 = "没法保存"。
                                const wasEnabled = config.enabled.some((m) => `${m.providerId}:${m.modelId}` === fullId);
                                const newCount = wasEnabled
                                  ? config.enabled.length - 1
                                  : config.enabled.length + 1;
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
// 把"自定义提供商"从 ModelTab 左栏底部提升为独立 sub-tab,不再混在
// builtin provider 列表末尾看不见。CustomProvidersTab 持有自己的
// config state,通过 localStorage + MODEL_CONFIG_CHANGED_EVENT 跟
// ModelTab 保持同步(单一权威来源 = localStorage)。
export function CustomProvidersTab() {
  const safeSetTimeout = useSafeTimeout();
  const [config, setConfig] = useState<ModelConfigData>(loadModelConfig);
  const [customModels, setCustomModels] = useState<Record<string, string[]>>({});

  // 监听其他 tab/组件写入的 MODEL_CONFIG_CHANGED_EVENT,把本组件的
  // config state 重新拉一遍(localStorage 是单一权威来源)。
  useEffect(() => {
    const refresh = () => {
      try {
        const raw = storage.getItem(MODEL_CONFIG_KEY);
        if (raw) setConfig({ modelContexts: {}, ...JSON.parse(raw) });
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

  // CustomProvidersTab 不显示 builtin provider 列表,不需要 activeKeys;
  // CustomProviderSection.handleFetchModelsForSavedProvider 里的 onRefresh()
  // 在此为 no-op(它只 refresh 外面 ModelTab 的 activeKeys 元数据)。
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
