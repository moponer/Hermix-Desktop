import { useState, useEffect, useCallback } from "react";
import { Save, Check, AlertCircle, RefreshCw, ChevronRight, CheckCircle } from "lucide-react";
import * as api from "../../api";
import { storage } from "../../utils/storageCompat";
import { writeModelConfigAndNotify } from "../../hooks/useCurrentModel";
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
  // Track models for custom providers: key=custom-{id}, value=modelId[]
  const [customModels, setCustomModels] = useState<Record<string, string[]>>({});
  // Track which custom provider IDs have an API key configured (from
  // /api/custom-providers → has_key). This is the authoritative source
  // because it reads config.yaml directly, unlike GET /api/env which
  // may read the wrong path before the backend is recompiled.
  const [customIdsWithKey, setCustomIdsWithKey] = useState<Set<string>>(new Set());
  // Load custom providers from backend on mount, sync to config
  useEffect(() => {
    api.getCustomProviders().then((providers) => {
      const cm: Record<string, string[]> = {};
      const withKey = new Set<string>();
      const enabledSet = new Set(config.enabled.map((m) => `${m.providerId}:${m.modelId}`));
      const newEnabled = [...config.enabled];
      providers.forEach((p) => {
        if (p.has_key) withKey.add(p.id);
        const models = p.models.filter(Boolean);
        if (models.length > 0) {
          cm[p.id] = models;
          models.forEach((mid) => {
            const key = `${p.id}:${mid}`;
            if (!enabledSet.has(key)) {
              newEnabled.push({ providerId: p.id, modelId: mid });
            }
          });
        }
      });
      if (Object.keys(cm).length > 0 || newEnabled.length > config.enabled.length) {
        setCustomModels(cm);
        setConfig((prev) => {
          // 否则 mount 时把 custom provider 的模型加进 enabled → React state 改了但
          // localStorage 永远空 → 重启后 ChatView 弹窗看不到这些模型。
          const next = { ...prev, enabled: newEnabled };
          writeModelConfigAndNotify(next);
          return next;
        });
      }
      // Always sync has_key state (even if no models — provider may exist
      // with a key but zero models fetched yet).
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

  // ── Custom provider model handlers ──
  const updateCustomModels = useCallback((providerId: string, models: string[]) => {
    setCustomModels((prev) => {
      const next = { ...prev, [providerId]: models };
      return next;
    });
    setConfig((prev) => {
      const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
      const newEnabled = [...prev.enabled];
      models.forEach((modelId) => {
        const key = `${providerId}:${modelId}`;
        if (!existing.has(key)) newEnabled.push({ providerId, modelId });
      });
      const next = { ...prev, enabled: newEnabled, defaultModel: prev.defaultModel || (models.length > 0 ? `${providerId}:${models[0]}` : prev.defaultModel) };
      writeModelConfigAndNotify(next);
      return next;
    });
  }, []);

  const refreshCustomProviders = useCallback(() => {
    api.getCustomProviders().then((providers) => {
      const cm: Record<string, string[]> = {};
      providers.forEach((p) => {
        const models = p.models.filter(Boolean);
        if (models.length > 0) {
          cm[p.id] = models;
        }
      });
      setCustomModels(cm);
      // Re-add any models from providers into config
      setConfig((prev) => {
        const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
        const newEnabled = [...prev.enabled];
        Object.entries(cm).forEach(([pid, models]) => {
          models.forEach((mid) => {
            const key = `${pid}:${mid}`;
            if (!existing.has(key)) newEnabled.push({ providerId: pid, modelId: mid });
          });
        });
        const next = { ...prev, enabled: newEnabled };
        writeModelConfigAndNotify(next);
        return next;
      });
    }).catch((e) => { console.warn("[SettingsView] auto-correct sync failed:", e); });
    // Also refresh env-var state so activeKeys (including CUSTOM_*_API_KEY)
    // reflects the latest backend state without a full page refresh.
    api.getEnvVars().then((envVars) => {
      const keys: Record<string, boolean> = {};
      PROVIDERS.forEach((p) => { keys[p.id] = !!envVars[p.keyEnvVar]?.is_set; });
      for (const [varName, info] of Object.entries(envVars ?? {})) {
        const m = /^CUSTOM_([A-Za-z0-9_-]+)_API_KEY$/.exec(varName);
        if (m && (info as { is_set?: boolean })?.is_set) {
          // Normalise to lowercase — provider ids are always lowercase
          // (sanitized from user input via .toLowerCase()).
          keys[`custom-${m[1].toLowerCase().replace(/_/g, "-")}`] = true;
        }
      }
      setActiveKeys(keys);
    }).catch((e) => { console.warn("[SettingsView] env keys fetch failed:", e); });
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
        <div className="p-3">
          <div className="text-[11px] text-text-muted uppercase tracking-wider mb-2 px-2">供应商</div>
          {PROVIDERS.map((p) => {
            const isActive = selectedProvider?.id === p.id;
            const connected = activeKeys[p.id];
            return (
              <button key={p.id} onClick={() => selectProvider(p)} className={`w-full flex items-center gap-2 px-3 py-2.5 rounded-lg text-left text-sm ${isActive ? "bg-accent/10 text-accent" : "text-text-secondary hover:bg-bg-tertiary hover:text-text-primary"}`}>
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
          {/* Custom provider entry */}
          <div className="mt-3 pt-3 border-t border-border-primary px-2">
            <button
              onClick={() => setSelectedProvider({ id: "_custom", name: "自定义", baseUrl: "", keyEnvVar: "", description: "兼容 OpenAI API 的任意服务" })}
              className={`w-full flex items-center gap-2 px-3 py-2.5 rounded-lg text-left text-sm ${selectedProvider?.id === "_custom" ? "bg-accent/10 text-accent" : "text-text-secondary hover:bg-bg-tertiary hover:text-text-primary"}`}
            >
              <ProviderIcon providerId="custom" size={18} title="自定义" />
              <div className="flex-1 min-w-0">
                <div className="font-medium text-xs truncate">自定义</div>
                <div className="text-[10px] text-text-muted truncate">兼容 OpenAI API 的任意服务</div>
              </div>
              <ChevronRight size={12} className="text-text-muted shrink-0" />
            </button>
            {/* Intentionally omitted: the "自定义" group used to render a
                light card per custom provider here, duplicating the right
                column's CustomProviderSection ("已配置的提供商"). Custom
                provider management lives only there now, so no dead state
                is left behind. */}
          </div>
        </div>
      </div>

      {/* 右侧：选中供应商的配置 */}
      <div className="flex-1 overflow-y-auto min-h-0">
        {selectedProvider ? (
          selectedProvider.id === "_custom" ? (
            <CustomProviderSection
              customModels={customModels}
              onModelsFetched={updateCustomModels}
              onRefresh={refreshCustomProviders}
              toggleModel={toggleModel}
              setDefault={setDefault}
              config={config}
              setConfig={setConfig}
            />
          ) : (
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
                          <button onClick={() => toggleModel(selectedProvider.id, modelId)} className={`w-4 h-4 rounded border flex items-center justify-center transition-colors ${isEnabled ? "bg-accent border-accent" : "bg-bg-card border-border-secondary hover:border-accent"}`}>
                            {isEnabled
                              ? <svg width="9" height="9" viewBox="0 0 9 9" fill="none"><path d="M1.5 4.5l2 2 4-4" stroke="white" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/></svg>
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
                                className="bg-bg-tertiary border border-border-secondary rounded px-1 py-0.5 text-[10px] text-text-muted outline-none"
                              >
                                <option value="">默认</option>
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
          )
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
      onRefresh();
      // 不会把 modelId 加进 config.enabled，也不会持久化 → ChatView 弹窗看不到。
      // 与内置 provider 的 fetchModels 一致，把新模型 merge 进 config.enabled 并写盘，
      // 否则 ChatView 弹窗看不到这些模型。
      setConfig((prev) => {
        const existing = new Set(prev.enabled.map((m) => `${m.providerId}:${m.modelId}`));
        const newEnabled = [...prev.enabled];
        models.forEach((modelId) => {
          if (!existing.has(`${id}:${modelId}`)) newEnabled.push({ providerId: id, modelId });
        });
        const next = { ...prev, enabled: newEnabled };
        writeModelConfigAndNotify(next);
        return next;
      });
      setSuccess(`成功获取 ${models.length} 个模型`);
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
      onRefresh();
      setSuccess(`成功获取 ${models.length} 个模型`);
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

      {/* Saved providers list */}
      {providers.length > 0 && (
        <div className="space-y-2">
          <div className="text-[11px] text-text-muted uppercase tracking-wider">已配置的提供商</div>
          {providers.map((p) => (
            <div key={p.id} className="flex items-center gap-3 px-3 py-2.5 rounded-lg bg-bg-card border border-border-primary text-xs">
              <div className="flex-1 min-w-0">
                <div className="font-medium text-text-primary truncate">
                  {p.display_name || p.id}
                  {p.has_key && <span className="ml-1.5 w-1.5 h-1.5 rounded-full bg-green-400 inline-block" />}
                </div>
                <div className="text-[10px] text-text-muted truncate mt-0.5">{p.base_url || "—"} · {p.models.length} 模型</div>
              </div>
              <button onClick={() => handleTestConnection(p.id)} disabled={testing === p.id} className="shrink-0 px-2 py-1 rounded text-[10px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10 disabled:opacity-40">
                {testing === p.id ? "..." : "测试"}
              </button>
              <button onClick={() => handleFetchModelsForSavedProvider(p.id)} disabled={fetching} className="shrink-0 px-2 py-1 rounded text-[10px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10 disabled:opacity-40">
                模型
              </button>
              <button onClick={() => handleEdit(p)} className="shrink-0 px-2 py-1 rounded text-[10px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10">
                编辑
              </button>
              <button onClick={() => handleDelete(p.id, p.models)} className="shrink-0 px-2 py-1 rounded text-[10px] bg-red-500/10 text-red-400 hover:bg-red-500/20">
                删除
              </button>
            </div>
          ))}
        </div>
      )}

      {/* ── Custom provider models (enable/disable + set-default) ──
          Models render in a 2-column grid with tight rows, so 5 models
          take ~96px instead of 200+px and the "新建提供商" form below
          stays visible. The "设为默认" button stays at the right of each
          row (only when enabled), preserving the original interaction. */}
      {Object.keys(customModels).length > 0 && (
        <div>
          <div className="text-[11px] text-text-muted uppercase tracking-wider mb-2">自定义模型 ({Object.keys(customModels).length})</div>
          {Object.entries(customModels).map(([providerId, modelList]) => (
            <div key={providerId} className="mb-3">
              <div className="text-[11px] text-text-secondary font-medium mb-1">
                {providerId.replace(/^custom-/, "")} · {modelList.length} 个
              </div>
              <div className="grid grid-cols-2 gap-1.5 max-h-32 overflow-y-auto rounded-lg border border-border-primary bg-bg-card p-2">
                {modelList.map((modelId) => {
                  const fullId = `${providerId}:${modelId}`;
                  const isEnabled = config.enabled.some((m) => `${m.providerId}:${m.modelId}` === fullId);
                  const isDefault = config.defaultModel === fullId;
                  const ctx = config.modelContexts?.[fullId];
                  return (
                    <div key={`${providerId}-${modelId}`} className={`flex items-center gap-1.5 px-2 py-1 rounded text-[11px] ${isDefault ? "bg-accent/10 border border-accent/30" : "hover:bg-bg-tertiary"}`}>
                      <button onClick={() => toggleModel(providerId, modelId)} className={`w-3.5 h-3.5 rounded border flex items-center justify-center shrink-0 transition-colors ${isEnabled ? "bg-accent border-accent" : "bg-bg-card border-border-secondary hover:border-accent"}`}>
                        {isEnabled
                          ? <svg width="8" height="8" viewBox="0 0 8 8" fill="none"><path d="M1.5 4l2 2 3-3" stroke="white" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/></svg>
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
                              // Sync to backend
                              if (providerId.startsWith("custom-")) {
                                const ctx = { ...config.modelContexts, [fullId]: v ? Number(v) : 0 };
                                api.updateCustomProviderModelContexts(providerId, ctx).catch((e) => { console.warn("[SettingsView] modelContext sync failed:", e); });
                              }
                            }}
                            className="bg-bg-tertiary border border-border-secondary rounded px-1 py-0.5 text-[9px] text-text-muted outline-none"
                          >
                            <option value="">默认</option>
                            {CONTEXT_WINDOW_OPTIONS.map((o) => (
                              <option key={o.value} value={o.value}>{o.label}</option>
                            ))}
                          </select>
                          <button onClick={() => setDefault(fullId)} className={`shrink-0 px-1.5 py-0.5 rounded text-[9px] ${isDefault ? "bg-accent text-accent-on-text" : "border border-border-secondary text-text-muted hover:border-accent hover:text-accent"}`}>
                            {isDefault ? "默认" : "设为默认"}
                          </button>
                        </>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
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
