import { useState, useEffect, useCallback } from "react";
import { RefreshCw, Save } from "lucide-react";
import * as api from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import { useCurrentModel } from "../../hooks/useCurrentModel";
import { toastError } from "../../hooks/useToast";
import { type PythonProbeResult } from "./settingsUtils";

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

  const handleToggleTickLoop = useCallback(async () => {
    const next = !tickLoopRunning;
    setTickLoopLoading(true);
    try {
      const status = await api.toggleTickLoop(next);
      setTickLoopRunning(status.running);
    } catch (e) {
      console.error("TickLoop toggle failed", e);
      toastError("TickLoop 切换失败");
    } finally {
      setTickLoopLoading(false);
    }
  }, [tickLoopRunning]);

  return (
    <div className="p-6 space-y-6 max-w-2xl">
      <div>
        <h2 className="text-lg font-semibold text-text-primary">后端设置</h2>
        <p className="text-xs text-text-muted mt-1">hermix server 连接状态。</p>
      </div>
      <div className="rounded-xl border border-border-primary bg-bg-card p-4">
        <div className="text-sm font-medium text-text-primary mb-2">后端状态</div>
        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${health ? "bg-green-400" : health === false ? "bg-red-400" : "bg-yellow-400"}`} />
          <span className="text-xs text-text-muted">{health === null ? "检测中..." : health ? "运行正常" : "未连接"}</span>
        </div>
        {defaultModel
          ? <div className="mt-3 text-xs text-text-muted">当前模型：<span className="text-text-primary font-medium">{defaultModel}</span></div>
          : <div className="mt-3 text-xs text-text-muted">当前模型：<span className="text-amber-400">未设置（请到「模型配置」选择一个）</span></div>}
        <button onClick={async () => { const ok = await api.checkBackendHealth(); setHealth(ok); }} className="mt-3 flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-border-secondary text-xs text-text-secondary hover:text-text-primary">
          <RefreshCw className="icon-sm" /> 刷新状态
        </button>
      </div>
      <div className="rounded-xl border border-border-primary bg-bg-card p-4">
        <div className="flex items-center justify-between">
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
          <button
            type="button"
            role="switch"
            aria-checked={tickLoopRunning}
            disabled={tickLoopLoading}
            onClick={handleToggleTickLoop}
            className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
              tickLoopRunning ? "bg-accent" : "bg-bg-tertiary"
            } ${tickLoopLoading ? "opacity-60 cursor-wait" : "cursor-pointer"}`}
          >
            <span
              className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white shadow transition-transform ${
                tickLoopRunning ? "translate-x-5" : "translate-x-1"
              }`}
            />
          </button>
        </div>
      </div>
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
      setOverrideStatus(`保存失败: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setOverrideSaving(false);
    }
  }, [overrideInput, runProbe]);

  const selectedRow = probe?.entries.find((e) => e.path === probe.selected);

  return (
    <div className="rounded-xl border border-border-primary bg-bg-card p-4">
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
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-border-secondary text-xs text-text-secondary hover:text-text-primary disabled:opacity-40 shrink-0"
        >
          <RefreshCw className={`icon-sm ${probeLoading ? "animate-spin" : ""}`} />
          {probeLoading ? "检测中" : "重新检测"}
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
                  ? "bg-accent/10 border border-accent/30"
                  : "bg-bg-tertiary/30 border border-border-secondary/40"
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
        <div className="mt-2 flex items-center gap-2 px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/30 text-xs text-red-400">
          {probeErr}
        </div>
      )}

      {/* Override input */}
      <div className="mt-4 pt-3 border-t border-border-primary space-y-2">
        <label className="block text-xs font-medium text-text-primary">
          自定义 Python 路径（可选）
        </label>
        <div className="flex gap-2">
          <input
            value={overrideInput}
            onChange={(e) => setOverrideInput(e.target.value)}
            placeholder="例如 C:\Python311\python.exe"
            className="flex-1 bg-bg-tertiary border border-border-primary rounded-lg px-3 py-1.5 text-xs text-text-primary placeholder-text-muted outline-none font-mono"
          />
          <button
            onClick={handleSaveOverride}
            disabled={overrideSaving}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-accent hover:bg-accent-hover text-accent-on-text disabled:opacity-40 shrink-0"
          >
            <Save className="icon-sm" />
            {overrideSaving ? "保存中" : "保存"}
          </button>
        </div>
        {overrideStatus && (
          <div className="text-[length:var(--text-meta)] text-text-muted">{overrideStatus}</div>
        )}
      </div>
    </div>
  );
}
