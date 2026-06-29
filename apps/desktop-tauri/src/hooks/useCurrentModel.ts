/**
 * `useCurrentModel` — single source of truth for "what model is the user
 * currently using" in the desktop-tauri UI.
 *
 * ## Why this hook exists
 *
 * Before this hook, the same "current model" value was being read from two
 * completely different places in the app:
 *
 *   1. `localStorage.hermix.desktop.model_config.defaultModel`  (set by
 *      `SettingsView` → "模型配置" tab; read by `ChatView` model selector +
 *      top-of-chat `ModelStatusBar`). This is the model the **user** picked.
 *
 *   2. `GET /api/model/info` on the hermix server (the in-process Rust
 *      `AgentService`'s `HermesConfig.model` field, sourced from
 *      `config.yaml`). This is the model the **backend** is running with.
 *
 * The two are not auto-synchronized: editing the UI list, hitting "保存", and
 * `api.updateConfig` / `api.autoCorrectConfig` *do* try to write
 * `defaultModel` into `config.yaml`, but the backend often holds on to its
 * boot-time snapshot and the two drift. `SettingsView` → "后端设置" used
 * the backend source and therefore showed a stale / placeholder value
 * (e.g. `minimax:MiniMax-M2`) when the chat UI was happily running on the
 * user's real choice.
 *
 * ## What this hook does
 *
 * It reads the same localStorage key the rest of the desktop UI uses, and
 * exposes a `refresh()` callback so consumers can re-read after a save
 * (e.g. after `SettingsView` "模型配置" calls `saveModelConfig`).
 *
 * It also subscribes to a custom `hermix:model-config-changed` window event
 * — when any component writes a new model config, it can dispatch that
 * event and every `useCurrentModel` consumer will update immediately
 * without needing prop-drilling.
 *
 * ## Usage
 *
 * ```ts
 * const { defaultModel, provider, modelId, refresh } = useCurrentModel();
 *
 * // defaultModel is the "provider:modelId" string ("siliconflow:Qwen/...")
 * // provider  = defaultModel.split(":")[0]   (or "" if no default set)
 * // modelId   = defaultModel.split(":")[1]   (or "" if no default set)
 * ```
 *
 * If no default is set, `defaultModel === ""` — callers should show their
 * usual "no model selected" UI in that case.
 */
import { useCallback, useEffect, useState } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

/** The localStorage key holding the user's model config (provider + model). */
const MODEL_CONFIG_KEY = STORAGE_KEYS.modelConfig;

/**
 * Window event dispatched by any component that has just written a new
 * model config. Listened to by `useCurrentModel` consumers so the
 * "current model" badge updates without prop-drilling or polling.
 */
export const MODEL_CONFIG_CHANGED_EVENT = "hermix:model-config-changed";

interface ModelConfigData {
  enabled: { providerId: string; modelId: string }[];
  defaultModel: string;
}

function readDefaultModel(): string {
  try {
    const raw = storage.getItem(MODEL_CONFIG_KEY);
    if (!raw) return "";
    const cfg = JSON.parse(raw) as ModelConfigData;
    return (cfg?.defaultModel ?? "").trim();
  } catch {
    return "";
  }
}

export interface CurrentModel {
  /**
   * Full `"provider:modelId"` string the user selected as default.
   * Empty string if no default is set.
   */
  defaultModel: string;
  /** Provider id (`"openai"`, `"siliconflow"`, ...) — or `""` if unset. */
  provider: string;
  /** Model id within the provider — or `""` if unset. */
  modelId: string;
  /**
   * Re-read from localStorage. Cheap; safe to call after a save
   * (returns the post-save value immediately, the event subscriber
   * below will also fire if other components are listening).
   */
  refresh: () => void;
}

/**
 * Resolve the canonical current default model from localStorage.
 *
 * Splits the stored `"provider:modelId"` on the **first** colon so model
 * ids that themselves contain colons (rare but legal, e.g. some Anthropic
 * dated snapshots) survive intact.
 */
export function useCurrentModel(): CurrentModel {
  const [defaultModel, setDefaultModel] = useState<string>(() => readDefaultModel());

  const refresh = useCallback(() => {
    setDefaultModel(readDefaultModel());
  }, []);

  useEffect(() => {
    // Re-read whenever *any* component dispatches the change event.
    // This keeps the SettingsView "后端设置" tab, the chat-view
    // model selector, and the ModelStatusBar in lockstep without
    // any of them having to prop-drill the new value.
    const onChanged = () => refresh();
    window.addEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);

    // Cross-tab sync: another tab editing Settings would change
    // localStorage but not dispatch our event. Listen to the raw
    // `storage` event so a Settings save in one tab updates this
    // tab's badge immediately too.
    const onStorage = (e: StorageEvent) => {
      if (e.key === MODEL_CONFIG_KEY) refresh();
    };
    window.addEventListener("storage", onStorage);

    return () => {
      window.removeEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
      window.removeEventListener("storage", onStorage);
    };
  }, [refresh]);

  const idx = defaultModel.indexOf(":");
  const provider = idx >= 0 ? defaultModel.slice(0, idx) : "";
  const modelId = idx >= 0 ? defaultModel.slice(idx + 1) : "";

  return { defaultModel, provider, modelId, refresh };
}

/**
 * Helper for writers: persist a new model config and notify all
 * `useCurrentModel` consumers in this tab. Call this from
 * `SettingsView` saves so the "后端设置" tab updates immediately
 * without a hard reload.
 */
export function writeModelConfigAndNotify(cfg: ModelConfigData): void {
  storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(cfg));
  window.dispatchEvent(new CustomEvent(MODEL_CONFIG_CHANGED_EVENT));
}
