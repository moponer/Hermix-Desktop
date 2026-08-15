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
      await api.updateConfig({
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
      } as unknown as api.HermesConfig);
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
    <div className="p-6 space-y-6 max-w-2xl">
      <div>
        <h2 className="text-lg font-semibold text-text-primary">高级设置</h2>
        <p className="text-xs text-text-muted mt-1">调整压缩、子 Agent 等内部参数。修改后需要重启生效。</p>
      </div>

      {/* Compression Settings */}
      <div className="rounded-xl border border-border-primary bg-bg-card p-4">
        <div className="text-sm font-medium text-text-primary mb-3">压缩设置</div>
        <div className="space-y-3">
          <div>
            <label className="text-xs text-text-muted">最小摘要 Token 数</label>
            <input
              type="number"
              value={config.min_summary_tokens}
              onChange={(e) => handleChange("min_summary_tokens", Number(e.target.value))}
              className="mt-1 w-full px-3 py-1.5 rounded-lg border border-border-secondary bg-bg-primary text-text-primary text-sm"
            />
            <p className="text-[10px] text-text-muted mt-0.5">摘要生成的最小 token 预算</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">最大摘要 Token 数</label>
            <input
              type="number"
              value={config.summary_tokens_ceiling}
              onChange={(e) => handleChange("summary_tokens_ceiling", Number(e.target.value))}
              className="mt-1 w-full px-3 py-1.5 rounded-lg border border-border-secondary bg-bg-primary text-text-primary text-sm"
            />
            <p className="text-[10px] text-text-muted mt-0.5">摘要输出的 token 上限</p>
          </div>
        </div>
      </div>

      {/* Sub-agent Settings */}
      <div className="rounded-xl border border-border-primary bg-bg-card p-4">
        <div className="text-sm font-medium text-text-primary mb-3">子 Agent 设置</div>
        <div className="space-y-3">
          <div>
            <label className="text-xs text-text-muted">超时时间（秒）</label>
            <input
              type="number"
              value={config.sub_agent_timeout_secs}
              onChange={(e) => handleChange("sub_agent_timeout_secs", Number(e.target.value))}
              className="mt-1 w-full px-3 py-1.5 rounded-lg border border-border-secondary bg-bg-primary text-text-primary text-sm"
            />
            <p className="text-[10px] text-text-muted mt-0.5">单个子 Agent 执行的超时时间</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">最小轮次</label>
            <input
              type="number"
              value={config.sub_agent_min_turns}
              onChange={(e) => handleChange("sub_agent_min_turns", Number(e.target.value))}
              className="mt-1 w-full px-3 py-1.5 rounded-lg border border-border-secondary bg-bg-primary text-text-primary text-sm"
            />
            <p className="text-[10px] text-text-muted mt-0.5">子 Agent 最少执行轮次</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">最大轮次上限</label>
            <input
              type="number"
              value={config.sub_agent_max_turns_cap}
              onChange={(e) => handleChange("sub_agent_max_turns_cap", Number(e.target.value))}
              className="mt-1 w-full px-3 py-1.5 rounded-lg border border-border-secondary bg-bg-primary text-text-primary text-sm"
            />
            <p className="text-[10px] text-text-muted mt-0.5">子 Agent 最多执行轮次</p>
          </div>
          <div>
            <label className="text-xs text-text-muted">委托最大迭代</label>
            <input
              type="number"
              value={config.delegation_max_iterations}
              onChange={(e) => handleChange("delegation_max_iterations", Number(e.target.value))}
              className="mt-1 w-full px-3 py-1.5 rounded-lg border border-border-secondary bg-bg-primary text-text-primary text-sm"
            />
            <p className="text-[10px] text-text-muted mt-0.5">委托任务的最大迭代次数</p>
          </div>
        </div>
      </div>

      {/* Save Button */}
      <button
        onClick={handleSave}
        className="flex items-center gap-2 px-4 py-2 rounded-lg bg-accent text-white text-sm font-medium hover:opacity-90 transition-opacity"
      >
        {saved ? <><Check size={14} /> 已保存</> : <><Save size={14} /> 保存设置</>}
      </button>
    </div>
  );
}
