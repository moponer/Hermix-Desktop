import { useState, useEffect, useCallback } from "react";
import { RefreshCw, Save, Server, Activity } from "lucide-react";
import * as api from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import { useCurrentModel } from "../../hooks/useCurrentModel";
import { toastError } from "../../hooks/useToast";
import { Card, IconTile, Input, Switch } from "../ui";
import { type PythonProbeResult } from "./settingsUtils";
import { LOADING, SAVE, SAVE_FAILED, SAVING, failure } from "../strings";

// ── Backend Tab ────────────────────────────────────

export function BackendTab() {
  const { defaultModel } = useCurrentModel();
  const [health, setHealth] = useState<boolean | null>(null);
  const [tickLoopRunning, setTickLoopRunning] = useState(false);
  const [tickLoopLoading, setTickLoopLoading] = useState(false);

  useEffect(() => {
    api.checkBackendHealth().then(setHealth).catch((e) => { console.warn("[SettingsView] health check failed:", e); });
    api.getTickLoopStatus().then((s) => setTickLoopRunning(s.running)).catch((e) => { console.warn("[SettingsView] tickloop status failed:", e); });
  }, []);

  const handleToggleTickLoop = useCallback(
    async (next: boolean) => {
      setTickLoopLoading(true);
      try {
        const status = await api.toggleTickLoop(next);
        setTickLoopRunning(status.running);
      } catch (e) {
        console.error("TickLoop toggle failed", e);
        /* 真实原因不能只写 console：用户要知道是后端没起来还是被拒，
           以及下一步做什么。 */
        toastError(
          `TickLoop 切换失败：${e instanceof Error ? e.message : String(e)}。请确认后端服务已启动，然后重试。`,
        );
      } finally {
        setTickLoopLoading(false);
      }
    },
    [],
  );

  return (
    <div className="space-y-6">
      <Card>
        <div className="flex items-center gap-2.5 mb-2">
          <IconTile icon={<Server className="icon-md" />} tone="blue" />
          <div className="text-sm font-medium text-text-primary">后端状态</div>
        </div>
        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${health ? "bg-green-400" : health === false ? "bg-red-400" : "bg-yellow-400"}`} />
          <span className="text-xs text-text-muted">{health === null ? LOADING : health ? "运行正常" : "未连接"}</span>
        </div>
        {defaultModel
          ? <div className="mt-3 text-xs text-text-muted">当前模型：<span className="text-text-primary font-medium">{defaultModel}</span></div>
          : <div className="mt-3 text-xs text-text-muted">当前模型：<span className="text-amber-400">未设置（请到「模型配置」选择一个）</span></div>}
        <button onClick={async () => { const ok = await api.checkBackendHealth(); setHealth(ok); }} className="mt-3 flex items-center gap-1.5 px-3 py-1.5 rounded-[var(--radius-md)] bg-bg-tertiary text-[length:var(--text-ui)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary">
          <RefreshCw className="icon-sm" /> 刷新状态
        </button>
      </Card>
      <Card>
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-start gap-3">
            <IconTile icon={<Activity className="icon-md" />} tone="cyan" />
            <div>
              <div className="text-sm font-medium text-text-primary">TickLoop 后台心跳</div>
              <p className="text-xs text-text-muted mt-0.5">
                启用后 AI 助手保持后台活跃状态，支持持续监控与定时任务。
              </p>
              {tickLoopRunning && (
                <span className="inline-flex items-center gap-1 mt-1 text-[length:var(--text-meta)] text-green-400">
                  <span className="w-1.5 h-1.5 rounded-full bg-green-400 animate-pulse" />
                  运行中
                </span>
              )}
            </div>
          </div>
          <Switch
            checked={tickLoopRunning}
            disabled={tickLoopLoading}
            onChange={(v) => void handleToggleTickLoop(v)}
            label="TickLoop 后台心跳"
          />
        </div>
      </Card>
      <PythonRuntimePanel />
    </div>
  );
}

// Probe only — does NOT auto-bundle Python. The sister slot reserved by
// `python_detect` (`<hermix-dir>/python/`) is left empty by default; the
// installer can opt into bundling later via `build-tauri.bat BUNDLED=1`
// + `tauri.conf.json::bundle.resources`. The UI mirrors the resolver in
// `apps/desktop-tauri/src-tauri/src/commands.rs::python_detect`.

export function PythonRuntimePanel() {
  const [probe, setProbe] = useState<PythonProbeResult | null>(null);
  const [probeLoading, setProbeLoading] = useState(false);
  const [probeErr, setProbeErr] = useState("");
  const [overrideInput, setOverrideInput] = useState("");
  const [overrideSaving, setOverrideSaving] = useState(false);
  const [overrideStatus, setOverrideStatus] = useState("");

  useEffect(() => {
    try {
      const stored = storage.getItem(STORAGE_KEYS.pythonOverride) ?? "";
      setOverrideInput(stored);
    } catch {}
  }, []);

  const runProbe = useCallback(async () => {
    setProbeLoading(true);
    setProbeErr("");
    try {
      const override = overrideInput.trim() || null;
      const result = (await (window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      }).__TAURI_INTERNALS__.invoke("python_detect", { overridePath: override })) as PythonProbeResult;
      setProbe(result);
    } catch (e) {
      setProbeErr(e instanceof Error ? e.message : String(e));
    } finally {
      setProbeLoading(false);
    }
  }, [overrideInput]);

  // Probe once on mount so the user sees the truth without clicking.
  // Intentionally empty deps — runProbe reads stable refs, not state.
  useEffect(() => {
    runProbe();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleSaveOverride = useCallback(async () => {
    const value = overrideInput.trim();
    setOverrideSaving(true);
    setOverrideStatus("");
    try {
      await (window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
      }).__TAURI_INTERNALS__.invoke("set_python_override", { path: value || null });
      try {
        if (value) {
          storage.setItem(STORAGE_KEYS.pythonOverride, value);
        } else {
          storage.removeItem(STORAGE_KEYS.pythonOverride);
        }
      } catch {}
      setOverrideStatus(value ? "已保存覆盖路径，下次 `execute_code` 调用立即生效" : "已清除覆盖路径，恢复默认探测顺序");
      await runProbe();
    } catch (e) {
      setOverrideStatus(failure(SAVE_FAILED, "请重试。", e instanceof Error ? e.message : String(e)));
    } finally {
      setOverrideSaving(false);
    }
  }, [overrideInput, runProbe]);

  const selectedRow = probe?.entries.find((e) => e.path === probe.selected);

  return (
    <Card>
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1">
          <div className="text-sm font-medium text-text-primary">Python 运行时</div>
          <p className="text-xs text-text-muted mt-0.5">
            `execute_code(language="python")` 和 PDF fallback 都会从这里选解释器。
            顺序：override → env → 同目录 sibling → PATH。
          </p>
        </div>
        <button
          onClick={runProbe}
          disabled={probeLoading}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-[var(--radius-md)] bg-bg-tertiary text-[length:var(--text-ui)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary disabled:opacity-40 shrink-0"
        >
          <RefreshCw className={`icon-sm ${probeLoading ? "animate-spin" : ""}`} />
          {probeLoading ? LOADING : "重新检测"}
        </button>
      </div>

      {/* Status badge */}
      <div className="mt-3 flex items-center gap-2">
        <div className={`w-2 h-2 rounded-full ${selectedRow ? "bg-green-400" : probe === null ? "bg-yellow-400" : "bg-red-400"}`} />
        <span className="text-xs text-text-muted">
          {selectedRow
            ? `已选中：${selectedRow.path}${selectedRow.version ? ` (${selectedRow.version})` : ""}`
            : probe === null
              ? "尚未检测"
              : "未找到可用的 Python（请安装或设置 override）"}
        </span>
      </div>

      {/* Probe results table */}
      {probe && (
        <div className="mt-3 space-y-1">
          {probe.entries.map((e, i) => (
            <div
              key={`${e.path}-${i}`}
              className={`flex items-center gap-2 px-2.5 py-1.5 rounded text-[length:var(--text-meta)] font-mono ${
                probe.selected === e.path
                  ? "bg-accent/15"
                  : "bg-bg-tertiary/60"
              }`}
            >
              <span
                className={`w-1.5 h-1.5 rounded-full shrink-0 ${e.exists ? "bg-green-400" : "bg-border-secondary"}`}
                title={e.exists ? "exists" : "missing"}
              />
              <span className="text-text-muted w-28 shrink-0">[{e.source}]</span>
              <span className="text-text-primary truncate flex-1" title={e.path}>{e.path}</span>
              {e.version && <span className="text-text-muted shrink-0">{e.version}</span>}
            </div>
          ))}
        </div>
      )}
      {probeErr && (
        <div className="mt-2 flex items-center gap-2 px-3 py-2 rounded-[var(--radius-md)] bg-red-500/12 text-[length:var(--text-ui)] text-red-400">
          {probeErr}
        </div>
      )}

      {/* Override input */}
      <div className="mt-5 space-y-2">
        <label
          htmlFor="python-override"
          className="block text-[length:var(--text-ui)] font-medium text-text-primary"
        >
          自定义 Python 路径（可选）
        </label>
        <div className="flex gap-2 items-end">
          <Input
            id="python-override"
            size="md"
            containerClassName="flex-1"
            className="font-mono"
            value={overrideInput}
            onChange={(e) => setOverrideInput(e.target.value)}
            placeholder="例如 C:\Python311\python.exe"
          />
          <button
            onClick={handleSaveOverride}
            disabled={overrideSaving}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-accent hover:bg-accent-hover text-on-accent disabled:opacity-40 shrink-0"
          >
            <Save className="icon-sm" />
            {overrideSaving ? SAVING : SAVE}
          </button>
        </div>
        {overrideStatus && (
          <div className="text-[length:var(--text-meta)] text-text-muted">{overrideStatus}</div>
        )}
      </div>
    </Card>
  );
}
