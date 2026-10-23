// 后台进程自动清理 —— 扫描 → 打分 → 列出候选与理由 → 用户确认 → 执行。
//
// 与磁盘清理模块（cleanup.rs）是同一套范式：**前端只展示，服务端说了算**。
// 执行时后端会重跑一遍安全网（关键进程 / 提权 / 服务宿主 / 有窗口 / 名单兜底），
// 前端传什么 PID 都绕不过去。
//
// 判定模型的出处见决策笔记
// docs/notes/implemented/feature/2026-10-23-system-tuner.md。
import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./system-tuner.css";

export interface CleanupCandidate {
  pid: number;
  name: string;
  memoryMb: number;
  score: number;
  risk: "low" | "medium" | "high";
  reasons: string[];
  recommended: boolean;
  blocked: string | null;
}

export interface CleanupScanReport {
  candidates: CleanupCandidate[];
  recommendedCount: number;
  reclaimMb: number;
  idleSecs: number;
  scanned: number;
  elapsedMs: number;
}

interface ExecOutcome {
  pid: number;
  name: string;
  ok: boolean;
  message: string;
}

interface CleanupExecReport {
  outcomes: ExecOutcome[];
  ended: number;
  failed: number;
  freedMb: number;
}

const RISK_LABEL: Record<CleanupCandidate["risk"], string> = {
  low: "建议清理",
  medium: "需人工判断",
  high: "不建议动",
};

function errorText(cause: unknown): string {
  if (cause instanceof Error) return cause.message;
  return String(cause);
}

function formatIdle(seconds: number): string {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.round(seconds / 60)} 分钟`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} 小时`;
  return `${Math.round(seconds / 86400)} 天`;
}

export function ProcessCleanupPane() {
  const [report, setReport] = useState<CleanupScanReport | null>(null);
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [running, setRunning] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const scan = useCallback(async () => {
    setScanning(true);
    setError(null);
    setNotice(null);
    setConfirming(false);
    try {
      const next = await invoke<CleanupScanReport>("sysmon_cleanup_scan");
      setReport(next);
      // 默认只勾"建议清理"档：其余交给人判断。
      setPicked(new Set(next.candidates.filter((c) => c.recommended).map((c) => c.pid)));
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setScanning(false);
    }
  }, []);

  // 一键清理：第一次点击进入确认态，5 秒内再点一次才真执行。
  useEffect(() => {
    if (!confirming) return;
    const timer = window.setTimeout(() => setConfirming(false), 5000);
    return () => window.clearTimeout(timer);
  }, [confirming]);

  const toggle = useCallback((pid: number) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(pid)) next.delete(pid);
      else next.add(pid);
      return next;
    });
  }, []);

  const run = useCallback(async () => {
    if (picked.size === 0) return;
    if (!confirming) {
      setConfirming(true);
      return;
    }
    setConfirming(false);
    setRunning(true);
    setError(null);
    try {
      const outcome = await invoke<CleanupExecReport>("sysmon_cleanup_execute", {
        pids: Array.from(picked),
      });
      const failed = outcome.outcomes.filter((o) => !o.ok);
      setNotice(
        [
          `已结束 ${outcome.ended} 个进程，约释放 ${outcome.freedMb.toFixed(0)} MB`,
          failed.length
            ? `${failed.length} 个未结束：${failed
                .slice(0, 3)
                .map((o) => `${o.name}（${o.message}）`)
                .join("、")}`
            : "",
        ]
          .filter(Boolean)
          .join("；"),
      );
      setPicked(new Set());
      void scan();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setRunning(false);
    }
  }, [confirming, picked, scan]);

  const selected = useMemo(
    () => (report?.candidates ?? []).filter((c) => picked.has(c.pid)),
    [report, picked],
  );
  const selectedMb = useMemo(
    () => selected.reduce((sum, c) => sum + c.memoryMb, 0),
    [selected],
  );

  return (
    <section className="tuner" aria-label="后台进程清理">
      <div className="tuner-head">
        <span className="tuner-title">后台进程清理</span>
        <span className="pane-stats">
          {report ? (
            <>
              <span>
                建议清理 <strong>{report.recommendedCount}</strong> 个
              </span>
              <span>约 {report.reclaimMb.toFixed(0)} MB</span>
              <span>系统已空闲 {formatIdle(report.idleSecs)}</span>
            </>
          ) : (
            <span>扫描后台进程，找出可以安全结束的</span>
          )}
        </span>
        <button
          type="button"
          className="pane-btn"
          onClick={() => void scan()}
          disabled={scanning || running}
        >
          {scanning ? "扫描中…" : "扫描"}
        </button>
        <button
          type="button"
          className="pane-btn"
          onClick={() => void run()}
          disabled={picked.size === 0 || running}
        >
          {running ? "结束中…" : confirming ? "再次点击确认" : `清理选中（${picked.size}）`}
        </button>
      </div>

      {notice && <p className="pane-notice">{notice}</p>}
      {error && <p className="pane-error">{error}</p>}

      {report && report.candidates.length === 0 && (
        <p className="pane-empty">
          没有发现可清理的后台进程（已扫描 {report.scanned} 个进程）。
        </p>
      )}

      {report && report.candidates.length > 0 && (
        <>
          <ul className="tuner-list">
            {report.candidates.map((candidate) => (
              <li
                key={candidate.pid}
                className={
                  candidate.blocked
                    ? "tuner-row is-blocked"
                    : candidate.recommended
                      ? "tuner-row is-recommended"
                      : "tuner-row"
                }
              >
                <label className="tuner-check">
                  <input
                    type="checkbox"
                    checked={picked.has(candidate.pid)}
                    disabled={Boolean(candidate.blocked)}
                    onChange={() => toggle(candidate.pid)}
                  />
                  <span className="tuner-name">{candidate.name}</span>
                  <span className="tuner-pid">PID {candidate.pid}</span>
                </label>
                <span className="tuner-memory">{candidate.memoryMb.toFixed(0)} MB</span>
                <span className={`tuner-chip is-${candidate.risk}`}>
                  {RISK_LABEL[candidate.risk]}
                </span>
                <span className="tuner-reasons">
                  {candidate.blocked
                    ? `不处理：${candidate.blocked}`
                    : candidate.reasons.join(" · ")}
                </span>
              </li>
            ))}
          </ul>
          <p className="pane-hint">
            已选 {selected.length} 个（约 {selectedMb.toFixed(0)} MB）。系统关键进程、提权进程、
            服务宿主、有可见窗口的程序一律不处理；结束前会先请求优雅退出。
          </p>
        </>
      )}
    </section>
  );
}
