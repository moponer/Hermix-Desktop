import { useState } from "react";
import { Save, Check } from "lucide-react";
import { reportError } from "../../lib/reportError";
import * as api from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import { useSafeTimeout } from "../../hooks/useSafeTimeout";
import { toastError } from "../../hooks/useToast";
import { Button, Card, Input } from "../ui";
import { type AdvancedConfig, DEFAULT_ADVANCED_CONFIG } from "./settingsUtils";
import { SAVE, SAVED } from "../strings";

// ── Advanced Tab ──────────────────────────────────────

export function AdvancedTab() {
  const safeSetTimeout = useSafeTimeout();
  const [config, setConfig] = useState<AdvancedConfig>(() => {
    try {
      const raw = storage.getItem(STORAGE_KEYS.advancedConfig);
      if (raw) return { ...DEFAULT_ADVANCED_CONFIG, ...JSON.parse(raw) };
    } catch (e) { reportError("settings-advanced", e); }
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
      /* 真实原因不能只写 console：用户要知道是后端没起来还是配置被拒，
         以及下一步做什么。 */
      toastError(
        `保存高级设置失败：${e instanceof Error ? e.message : String(e)}。请确认后端服务已启动，然后重试。`,
      );
      setSaved(false);
    }
  };

  return (
    <div className="space-y-6">
      {/* Compression Settings */}
      <Card>
        <div className="text-sm font-medium text-text-primary mb-3">压缩设置</div>
        <div className="space-y-3">
          <Input
            type="number"
            size="lg"
            label="最小摘要 Token 数"
            hint="摘要生成的最小 token 预算"
            value={config.min_summary_tokens}
            onChange={(e) => handleChange("min_summary_tokens", Number(e.target.value))}
          />
          <Input
            type="number"
            size="lg"
            label="最大摘要 Token 数"
            hint="摘要输出的 token 上限"
            value={config.summary_tokens_ceiling}
            onChange={(e) => handleChange("summary_tokens_ceiling", Number(e.target.value))}
          />
        </div>
      </Card>

      {/* Sub-agent Settings */}
      <Card>
        <div className="text-sm font-medium text-text-primary mb-3">子 Agent 设置</div>
        <div className="space-y-3">
          <Input
            type="number"
            size="lg"
            label="超时时间（秒）"
            hint="单个子 Agent 执行的超时时间"
            value={config.sub_agent_timeout_secs}
            onChange={(e) => handleChange("sub_agent_timeout_secs", Number(e.target.value))}
          />
          <Input
            type="number"
            size="lg"
            label="最小轮次"
            hint="子 Agent 最少执行轮次"
            value={config.sub_agent_min_turns}
            onChange={(e) => handleChange("sub_agent_min_turns", Number(e.target.value))}
          />
          <Input
            type="number"
            size="lg"
            label="最大轮次上限"
            hint="子 Agent 最多执行轮次"
            value={config.sub_agent_max_turns_cap}
            onChange={(e) => handleChange("sub_agent_max_turns_cap", Number(e.target.value))}
          />
          <Input
            type="number"
            size="lg"
            label="委托最大迭代"
            hint="委托任务的最大迭代次数"
            value={config.delegation_max_iterations}
            onChange={(e) => handleChange("delegation_max_iterations", Number(e.target.value))}
          />
        </div>
      </Card>

      {/* Save Button */}
      <Button
        variant="accent"
        onClick={handleSave}
        icon={saved ? <Check className="icon-md" /> : <Save className="icon-md" />}
      >
        {saved ? SAVED : SAVE}
      </Button>
    </div>
  );
}
