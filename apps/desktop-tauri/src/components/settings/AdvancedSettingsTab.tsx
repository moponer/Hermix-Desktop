import { useState } from "react";
import { Save, Check } from "lucide-react";
import * as api from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import { useSafeTimeout } from "../../hooks/useSafeTimeout";
import { toastError } from "../../hooks/useToast";
import { type AdvancedConfig, DEFAULT_ADVANCED_CONFIG } from "./settingsUtils";

// ── Advanced Tab ──────────────────────────────────────

export function AdvancedTab() {
  const safeSetTimeout = useSafeTimeout();
  const [config, setConfig] = useState<AdvancedConfig>(() => {
    try {
      const raw = storage.getItem(STORAGE_KEYS.advancedConfig);
      if (raw) return { ...DEFAULT_ADVANCED_CONFIG, ...JSON.parse(raw) };
    } catch {}
    return DEFAULT_ADVANCED_CONFIG;
  });
  const [saved, setSaved] = useState(false);

  const handleChange = (key: keyof AdvancedConfig, value: number) => {
    setConfig((prev) => ({ ...prev, [key]: value }));
    setSaved(false);
  };

  const handleSave = async () => {
    // implementation only wrote to localStorage, which the running
    // agent never read from. Now we PUT the values to the backend
    // via `api.updateConfig`, which writes config.yaml and pushes
    // the new shape to the live agent service. The localStorage
    // copy is kept as a UI-preference mirror so the form re-opens
    // with the same numbers across reloads even if the backend
    // boot order hasn't materialised yet (e.g. cold start before
    // the agent service is wired up).
    try {
      // Must send the FULL config: `updateConfig` only carries the
      // fields we hand it, so a partial body would drop `model`,
      // `llm_providers` and `model_selector` from config.yaml. That
      // exact bug wiped the user's model settings on 2026-10-03.
      // Mirrors SecurityTab's read-modify-write.
      const current = await api.getConfig();
      await api.updateConfig({
        ...current,
        compaction: {
          min_summary_tokens: config.min_summary_tokens,
          summary_tokens_ceiling: config.summary_tokens_ceiling,
        },
        advanced: {
          sub_agent_timeout_secs: config.sub_agent_timeout_secs,
          sub_agent_min_turns: config.sub_agent_min_turns,
          sub_agent_max_turns_cap: config.sub_agent_max_turns_cap,
          delegation_max_iterations: config.delegation_max_iterations,
        },
      });
      storage.setItem(STORAGE_KEYS.advancedConfig, JSON.stringify(config));
      setSaved(true);
      safeSetTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.error("Failed to save advanced config:", e);
      toastError("保存高级设置失败");
      setSaved(false);
    }
  };

  return (
    <div className="space-y-6">
      {/* Compression Settings */}
      <div className="rounded-[var(--radius-lg)] bg-bg-card p-4 shadow-[var(--shadow-paper)]">
        <div className="text-sm font-medium text-text-primary mb-3">压缩设置</div>
        <div className="space-y-3">
          <div>
            <label className="text-xs text-text-muted">最小摘要 Token 数</label>
            <input
              type="number"
              value={config.min_summary_tokens}
              onChange={(e) => handleChange("min_summary_tokens", Number(e.target.value))}
              className="mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
            />
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5">摘要生成的最小 token 预算</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">最大摘要 Token 数</label>
            <input
              type="number"
              value={config.summary_tokens_ceiling}
              onChange={(e) => handleChange("summary_tokens_ceiling", Number(e.target.value))}
              className="mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
            />
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5">摘要输出的 token 上限</p>
          </div>
        </div>
      </div>

      {/* Sub-agent Settings */}
      <div className="rounded-[var(--radius-lg)] bg-bg-card p-4 shadow-[var(--shadow-paper)]">
        <div className="text-sm font-medium text-text-primary mb-3">子 Agent 设置</div>
        <div className="space-y-3">
          <div>
            <label className="text-xs text-text-muted">超时时间（秒）</label>
            <input
              type="number"
              value={config.sub_agent_timeout_secs}
              onChange={(e) => handleChange("sub_agent_timeout_secs", Number(e.target.value))}
              className="mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
            />
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5">单个子 Agent 执行的超时时间</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">最小轮次</label>
            <input
              type="number"
              value={config.sub_agent_min_turns}
              onChange={(e) => handleChange("sub_agent_min_turns", Number(e.target.value))}
              className="mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
            />
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5">子 Agent 最少执行轮次</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">最大轮次上限</label>
            <input
              type="number"
              value={config.sub_agent_max_turns_cap}
              onChange={(e) => handleChange("sub_agent_max_turns_cap", Number(e.target.value))}
              className="mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
            />
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5">子 Agent 最多执行轮次</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">委托最大迭代</label>
            <input
              type="number"
              value={config.delegation_max_iterations}
              onChange={(e) => handleChange("delegation_max_iterations", Number(e.target.value))}
              className="mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
            />
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5">委托任务的最大迭代次数</p>
          </div>
        </div>
      </div>

      {/* Save Button */}
      <button
        onClick={handleSave}
        className="flex items-center gap-2 px-4 py-2 rounded-lg bg-accent text-on-accent text-sm font-medium hover:opacity-90 transition-opacity"
>
        {saved ? <><Check className="icon-md" /> 已保存</> : <><Save className="icon-md" /> 保存设置</>}
      </button>
    </div>
  );
}
