import { useState, useEffect, useCallback, useMemo } from "react";
import { Check, AlertCircle, RefreshCw, ChevronRight, Star, Trash2, CheckCircle, Save, AlertTriangle } from "lucide-react";
import * as api from "../../api";
import { storage } from "../../utils/storageCompat";
import { writeModelConfigAndNotify, MODEL_CONFIG_CHANGED_EVENT, setModelPersistErrorHandler } from "../../hooks/useCurrentModel";
import { ProviderIcon } from "../ProviderIcon";
import { useSafeTimeout } from "../../hooks/useSafeTimeout";
import {
  useContextWindows,
  type ContextWindowEntry,
} from "../../hooks/useContextWindows";
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
  isChatModel,
  pickDefaultModel,
  mergeFetchedModels,
  type ModelConfigData,
} from "./settingsUtils";

// ── 厂商模型探测 / 配置合并 ─────────────────────────

/** 直接向厂商 `/v1/models` 取模型清单（前端直连厂商域名：CSP `connect-src
 *  已放行 https:`，CORS 由厂商返回 ACAO 头）。任何失败都返回空列表，不抛。 */
async function probeProviderModels(keyEnvVar: string, baseUrl: string): Promise<string[]> {
  const MAX_RETRIES = 2;
  try {
    const reveal = await api.fetchJSON<{ value?: string }>("/api/env/reveal", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: keyEnvVar }),
    });
    const apiKey = reveal.value;
    if (!apiKey) return [];
    for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
      try {
        const resp = await fetch(`${baseUrl}/models`, {
          headers: { Authorization: `Bearer ${apiKey}` },
        });
        if (!resp.ok) {
          if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
          continue;
        }
        const data = await resp.json();
        return ((data.data || []) as { id: string }[]).map((m) => m.id).sort();
      } catch {
        if (attempt < MAX_RETRIES) await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
      }
    }
  } catch (e) {
    console.warn("[ModelTab] reveal api key failed:", e);
  }
  return [];
}

/**
 * 把探测到的模型并入配置。
 *
 * 只自动启用**可对话**模型（`isChatModel`）：厂商 `/v1/models` 返回的是全量
 * 目录，含 embedding / rerank / tts / 图像模型，全量塞进「已启用模型」会让
 * 列表爆炸，还可能把非对话模型顶成默认。未自动启用的模型仍留在 modelCache，
 * 可在聊天窗口的模型选择器里手动勾选。
 *
 * 默认模型只在原本为空时补一个，优先命中后端权威清单（`defaults`）。
 */
// 实现已抽到 settingsUtils.ts（纯函数 + 单测覆盖，此前埋在本文件里没法测）。

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
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(null);
  const [providerKeyInput, setProviderKeyInput] = useState<Record<string, string>>({});
  // 单行短暂绿闪用的 key（fid），700ms 后自清
  const [flashKey, setFlashKey] = useState<string | null>(null);
  // 后端权威默认模型（provider id → model），用于首次自动选中可对话模型
  const [providerDefaults, setProviderDefaults] = useState<Record<string, string>>({});

  // 写后端失败必须可见：localStorage 写了、config.yaml 没写时，UI 显示"已配置"
  // 而聊天报"未配置模型"。此前这个失败被静默吞掉。
  useEffect(() => {
    setModelPersistErrorHandler((e) => {
      setError(`已保存到本地，但写入后端失败：${e instanceof Error ? e.message : String(e)}`);
    });
    return () => setModelPersistErrorHandler(null);
  }, []);

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

      // 后端权威的"每家该用哪个模型"清单 —— 首次自动选中优先命中它，
      // 而不是厂商 /v1/models 全量目录里字典序第一个（那可能是 embedding）。
      const defaults = await api.getProviderDefaults().catch(() => ({} as Record<string, string>));
      setProviderDefaults(defaults);

      // localStorage 缺失（新机器 / WebView2 缓存被清）→ 用后端持久化列表起步
      // 一次，避免"config.yaml 有配置、设置面板却显示未启用"的两端不一致。
      let base = loadModelConfig();
      let rehydrated = false;
      if (storage.getItem(MODEL_CONFIG_KEY) === null) {
        const backend = await api.getModelList().catch(() => null);
        if (backend && backend.models.length > 0) {
          base = {
            enabled: backend.models.map((m) => ({ providerId: m.provider_id, modelId: m.model_id })),
            defaultModel: backend.default_model ?? "",
            modelContexts: {},
          };
          rehydrated = true;
        }
      }

      const probed = await Promise.all(
        PROVIDERS.filter((p) => keys[p.id]).map(async (p) => ({
          id: p.id,
          models: await probeProviderModels(p.keyEnvVar, p.baseUrl),
        })),
      );
      const usable = probed.filter((r) => r.models.length > 0);
      if (usable.length > 0) {
        const mergedCache = { ...cached };
        usable.forEach((r) => { mergedCache[r.id] = r.models; });
        setFetchedModels(mergedCache);
      }

      const next = mergeFetchedModels(base, usable, defaults);
      setConfig(next);
      // await 后才结束：写后端失败会经 persistErrorHandler 弹 banner，不再静默。
      // `rehydrated` 为真时即使内容没变也要落盘一次，把后端恢复的清单写回 local
      // storage，让设置面板与聊天窗口的模型列表立刻一致。
      if (rehydrated || next !== base) await writeModelConfigAndNotify(next);
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
    setInfo("已保存");
    setFlashKey(fullId);
    window.setTimeout(() => setFlashKey((k) => (k === fullId ? null : k)), 700);
  }, []);

  const setDefault = useCallback((providerId: string, modelId: string) => {
    const fid = fullId(providerId, modelId);
    setConfig((prev) => {
      if (prev.defaultModel === fid) return prev;
      const newConfig: ModelConfigData = { ...prev, defaultModel: fid, modelContexts: prev.modelContexts ?? {} };
      safeSetTimeout(async () => {
        // `PUT /api/model/list` 已把 `config.model` 同步成 default_model（后端
        // config.rs 保证），无需再多一次 getConfig/updateConfig 全量回写 ——
        // 那次回写是"读磁盘→整份覆盖"，会把刚落盘的选择器列表抹掉。
        await writeModelConfigAndNotify(newConfig);
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
        // 先 await 落盘选择器列表，再回读整份 config 改 model：否则 getConfig 可能
        // 读到未更新的 config.yaml，PUT /api/config 整份覆盖时把刚写的选择器抹掉。
        await writeModelConfigAndNotify(newConfig);
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
        try {
          const data = await api.fetchJSON<{ value?: string }>("/api/env/reveal", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ key: p.keyEnvVar }),
          });
          apiKey = data.value ?? "";
        } catch {
          // No reveal — fall through to "请填写 API Key" below.
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
      const models: string[] = ((data.data || []) as { id: string }[]).map((m) => m.id).sort();
      if (models.length === 0) { setError("未返回任何模型"); setLoadingProvider(null); return; }
      setFetchedModels((prev) => ({ ...prev, [p.id]: models }));

      // 先 await 落盘再调 auto-correct：否则 auto-correct 会抢在前端写盘之前，
      // 按 provider 优先级把用户刚选的默认模型改掉（UI 显示 A、实际跑 B）。
      const next = mergeFetchedModels(config, [{ id: p.id, models }], providerDefaults);
      setConfig(next);
      await writeModelConfigAndNotify(next);

      const chatCount = models.filter(isChatModel).length;
      let message = chatCount < models.length
        ? `已启用 ${chatCount} 个对话模型；另有 ${models.length - chatCount} 个非对话模型未启用，可在聊天窗口的模型选择器里手动勾选`
        : "";
      try {
        const r = await api.autoCorrectConfig();
        if (r.report) message = r.message;
      } catch {}
      if (message) setInfo(message);
    } catch (e) {
      setError(`获取失败: ${e instanceof Error ? e.message : String(e)}`);
    }
    setLoadingProvider(null);
  }, [providerKeyInput, config, providerDefaults]);

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

  // 上下文窗口由后端权威解析 —— 后端拥有 catalog，前端不再手抄一份。
  const enabledFids = useMemo(() => enabledRows.map((r) => r.fid), [enabledRows]);
  const contextWindows = useContextWindows(enabledFids);

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

  // select 下拉直接控制 selectedProviderId，无需 toggle 函数

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
          {error ? <AlertCircle className="icon-md" /> : <Check className="icon-md" />}
          <span>{info || error}</span>
        </div>
      )}

      <div className="flex-1 overflow-y-auto">
        {/* ── SECTION 1: 默认模型 ── */}
        <section className="px-6 py-5 border-b border-border-primary">
          <div className="flex items-center gap-2 mb-3">
            <Star className="icon-md text-accent" />
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
                <span className="text-[11px] text-text-faint ml-1"><Star size={10} className="inline-block align-[-1px] mr-0.5" />当前默认</span>
              )}
            </div>
          ) : (
            <p className="text-[12px] text-text-muted">尚未配置任何供应商，展开下方「供应商」配置 API Key 后获取模型即可。</p>
          )}
          {!config.defaultModel && hasAnyConfigurable && (
            <p className="text-[11px] text-warning mt-2"><AlertTriangle className="icon-sm inline-block align-[-1px] mr-1" />未设置默认模型，聊天无法开始</p>
          )}
        </section>

        {/* ── SECTION 2: 已启用模型 ── */}
        <section className="px-6 py-5 border-b border-border-primary">
          <div className="flex items-center gap-2 mb-3">
            <Check className="icon-md text-text-secondary" />
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
                  backend={contextWindows[row.fid]}
                  flashed={flashKey === row.fid}
                  onSetDefault={() => setDefault(row.providerId, row.modelId)}
                  onRemove={() => toggleModel(row.providerId, row.modelId)}
                  onSetContext={(v) => setModelContext(row.providerId, row.modelId, v)}
                />
              ))}
            </ul>
          )}
        </section>

        {/* ── SECTION 3: 供应商选择器 ── */}
        <section className="px-6 py-5">
          <h2 className="text-[15px] font-semibold text-text-primary mb-3">供应商</h2>
          <div className="flex items-center gap-3">
            <select
              value={selectedProviderId ?? ""}
              onChange={(e) => setSelectedProviderId(e.target.value || null)}
              className="flex-1 bg-bg-card border border-border-primary rounded-lg px-3 py-2.5 text-[13px] text-text-primary outline-none focus:border-accent appearance-none cursor-pointer"
            >
              <option value="" disabled>选择供应商...</option>
              {PROVIDERS.map((p) => (
                <option key={p.id} value={p.id}>
                  {activeKeys[p.id] && <Check className="icon-sm inline-block align-[-1px] mr-0.5" />}{p.name} — {p.description}
                </option>
              ))}
            </select>
            {selectedProviderId && (
              <button
                onClick={() => setSelectedProviderId(null)}
                className="text-[12px] text-text-muted hover:text-text-primary px-2 py-1 shrink-0"
                title="收起"
              >
                收起
              </button>
            )}
          </div>

          {/* 选中后的配置面板 */}
          {selectedProviderId && (() => {
            const p = PROVIDERS.find((x) => x.id === selectedProviderId);
            if (!p) return null;
            const configured = activeKeys[p.id] ?? false;
            const modelCount = fetchedModels[p.id]?.length ?? 0;
            const loading = loadingProvider === p.id;
            const keyInput = providerKeyInput[p.id] ?? "";
            return (
              <div className="mt-3 rounded-lg border border-border-primary bg-bg-card overflow-hidden">
                <div className="flex items-center gap-2 px-3 py-2.5 border-b border-border-primary">
                  <ProviderIcon providerId={p.id} className="icon-lg" title={p.name} />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-[13px] text-text-primary">{p.name}</div>
                    <div className="text-[11px] text-text-muted">{p.description}</div>
                  </div>
                  {configured && <span className="w-2 h-2 rounded-full bg-green-400 shrink-0" title="已配置 API Key" />}
                  {modelCount > 0 && (
                    <span className="text-[11px] text-text-faint shrink-0">{modelCount} 模型</span>
                  )}
                </div>
                <div className="p-3 space-y-2">
                  <div className="flex gap-2">
                    <input
                      type="password"
                      value={keyInput}
                      onChange={(e) => setProviderKeyInput((prev) => ({ ...prev, [p.id]: e.target.value }))}
                      placeholder={configured ? "已配置，输入新 Key 以更新" : `输入 ${p.name} API Key`}
                      className="flex-1 bg-bg-tertiary border border-border-primary rounded px-2.5 py-1.5 text-[12px] text-text-primary placeholder-text-muted outline-none"
                      onKeyDown={(e) => { if (e.key === "Enter") fetchModelsForProvider(p); }}
                    />
                    <button
                      onClick={() => fetchModelsForProvider(p)}
                      disabled={loading}
                      className="flex items-center gap-1.5 px-3 py-1.5 rounded text-[12px] bg-accent hover:bg-accent-hover text-accent-on-text disabled:opacity-40 shrink-0"
                    >
                      <RefreshCw className={`icon-sm ${loading ? "animate-spin" : ""}`} />
                      {loading ? "获取中" : configured ? "重新获取" : "获取模型"}
                    </button>
                  </div>
                  <p className="text-[11px] text-text-muted">
                    {modelCount > 0
                      ? `已获取 ${modelCount} 个模型 → 切到上方「已启用模型」勾选启用`
                      : "填写 API Key 后点击获取，模型会缓存到本地并自动启用"}
                  </p>
                </div>
              </div>
            );
          })()}
          <p className="mt-4 text-[11px] text-text-muted italic">自定义 OpenAI 兼容服务商请前往「自定义模型」标签页配置。</p>
        </section>
      </div>
    </div>
  );
}

// ── 已启用模型行 ──
function EnabledModelRow({ row, flashed, onSetDefault, onRemove, onSetContext, backend }: {
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
  /** 后端权威解析结果（后端拥有 catalog，前端不再镜像）。 */
  backend?: ContextWindowEntry;
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
      <ProviderIcon providerId={row.providerId} className="icon-lg" title={row.providerName} />
      <span className="font-mono text-text-primary truncate min-w-0 flex-1">{row.modelId}</span>
      <span className="text-[11px] text-text-faint uppercase shrink-0">
        {row.isCustom ? "CUSTOM" : "BUILTIN"}
      </span>
      <select
        value={row.ctx ?? ""}
        onChange={(e) => onSetContext(e.target.value ? Number(e.target.value) : null)}
        title="上下文窗口;空=后端 catalog 兜底"
        className="bg-bg-tertiary border border-border-secondary rounded px-1.5 py-0.5 text-[11px] text-text-muted outline-none"
      >
        <option value="">{defaultOptionLabel(resolveContextWindow(row.ctx, backend))}</option>
        {CONTEXT_WINDOW_OPTIONS.map((o) => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
      {row.isDefault ? (
        <span className="text-[11px] text-accent font-semibold shrink-0"><Star size={10} className="inline-block align-[-1px] mr-0.5" />默认</span>
      ) : (
        <button
          onClick={onSetDefault}
          className="text-[11px] px-2 py-0.5 rounded border border-border-secondary text-text-muted hover:border-accent hover:text-accent shrink-0"
        >
          设为默认
        </button>
      )}
      <button
        onClick={onRemove}
        title="取消启用"
        className="p-0.5 rounded text-text-muted hover:text-red-400 hover:bg-red-500/10 shrink-0"
      >
        <Trash2 className="icon-sm" />
      </button>
    </li>
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

  // 上下文窗口由后端权威解析 —— 后端拥有 catalog，前端不再手抄一份。
  const providerModelIds = useMemo(
    () => providers.flatMap((p) => (p.models ?? []).map((m) => `${p.id}:${m}`)),
    [providers],
  );
  const contextWindows = useContextWindows(providerModelIds);

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
      // 默认模型不取字典序第一个（可能是 embedding / rerank 模型）
      const pickedDefault = pickDefaultModel(models);
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
          defaultModel: prev.defaultModel || (pickedDefault ? `${safeId}:${pickedDefault}` : prev.defaultModel),
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
          // 用户手填的模型清单也按"可对话优先"挑默认，不取字典序第一个
          const picked = pickDefaultModel(models);
          if (picked) {
            const next = { ...prev, defaultModel: `${targetId}:${picked}` };
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
                    <ChevronRight className={`icon-md ${`shrink-0 text-text-muted transition-transform duration-base ${isExpanded ? "rotate-90" : ""}`}`} />
                    <div className="flex-1 min-w-0">
                      <div className="font-medium text-text-primary truncate flex items-center gap-1.5">
                        {p.display_name || p.id}
                        {p.has_key && <span className="w-1.5 h-1.5 rounded-full bg-green-400 inline-block shrink-0" />}
                      </div>
                      <div className="text-[11px] text-text-muted truncate mt-0.5">
                        {p.base_url || "—"} · {models.length} 模型{models.length > 0 ? ` · 已启用 ${enabledCount}` : ""}
                      </div>
                    </div>
                  </button>
                  <div className="flex items-center gap-1 shrink-0">
                    <button onClick={(e) => { e.stopPropagation(); handleTestConnection(p.id); }} disabled={testing === p.id} className="px-2 py-1 rounded text-[11px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10 disabled:opacity-40">
                      {testing === p.id ? "..." : "测试"}
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); handleFetchModelsForSavedProvider(p.id); }} disabled={fetching} className="px-2 py-1 rounded text-[11px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10 disabled:opacity-40">
                      模型
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); handleEdit(p); }} className="px-2 py-1 rounded text-[11px] bg-bg-tertiary text-text-muted hover:text-accent hover:bg-accent/10">
                      编辑
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); handleDelete(p.id, p.models); }} className="px-2 py-1 rounded text-[11px] bg-red-500/10 text-red-400 hover:bg-red-500/20">
                      删除
                    </button>
                  </div>
                </div>
                {isExpanded && (
                  <div className="border-t border-border-primary p-3 bg-bg-primary">
                    {models.length === 0 ? (
                      <div className="text-[11px] text-text-muted italic text-center py-3">
                        暂无模型，点上方「模型」按钮重新获取
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
                                setSuccess(wasEnabled ? `已取消 · 剩 ${newCount} 个模型启用中` : `已保存 · 共 ${newCount} 个模型启用中`);
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
                                    className="bg-bg-tertiary border border-border-secondary rounded px-1 py-0.5 text-[11px] text-text-muted outline-none"
                                  >
                                    <option value="">{defaultOptionLabel(resolveContextWindow(ctx, contextWindows[fullId]))}</option>
                                    {CONTEXT_WINDOW_OPTIONS.map((o) => (
                                      <option key={o.value} value={o.value}>{o.label}</option>
                                    ))}
                                  </select>
                                  <button onClick={() => {
                                    setDefault(fullId);
                                    setSuccess(`默认模型已设为 ${modelId}`);
                                    setTimeout(() => setSuccess(""), 2500);
                                  }} className={`shrink-0 px-1.5 py-0.5 rounded text-[11px] ${isDefault ? "bg-accent text-accent-on-text" : "border border-text-muted text-text-secondary hover:border-accent hover:text-accent"}`}>
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
          <AlertCircle className="icon-md" /> {error}
        </div>
      )}
      {success && (
        <div className="flex items-center gap-2 px-3 py-2 rounded-lg bg-emerald-500/10 border border-emerald-500/30 text-sm text-emerald-400">
          <CheckCircle className="icon-md" /> {success}
        </div>
      )}

      {/* ── Add new / Edit provider form ── */}
      <div className="space-y-3 pt-2 border-t border-border-primary">
        <div className="flex items-center justify-between">
          <div className="text-[11px] text-text-muted uppercase tracking-wider">
            {editingId ? "编辑提供商" : "新建提供商"}
          </div>
          {editingId && (
            <button onClick={handleCancelEdit} className="text-[11px] text-text-muted hover:text-text-primary">
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
          {!editingId && safeId && <p className="text-[11px] text-text-muted mt-1">模型 ID 前缀: {safeId}:</p>}
          {editingId && <p className="text-[11px] text-text-muted mt-1">提供商 ID: {editingId}</p>}
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
              {fetching ? <RefreshCw className="icon-md animate-spin" /> : saving ? "保存中..." : <Check className="icon-md" />}
              {fetching ? "获取中" : saving ? "保存中..." : "获取模型"}
            </button>
          </div>
          <p className="text-[11px] text-text-muted mt-1">填写名称、API URL 和 API Key 后点击"获取模型"自动填充，或手动输入</p>
        </div>

        <button
          onClick={handleSave}
          disabled={saving || (!editingId && !apiKey.trim())}
          className="flex items-center gap-2 px-4 py-2 rounded-lg bg-accent hover:bg-accent-hover text-accent-on-text text-sm disabled:opacity-40"
        >
          <Save className="icon-md" /> {saving ? "保存中..." : editingId ? "更新配置" : "保存配置"}
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
