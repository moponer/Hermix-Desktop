// 文件索引页：索引服务的真实状态、建索引进度、重建入口。
//
// 这一页要回答三个问题，每个都得是**真数字**：
// 1. 索引服务在不在跑？（提权进程，普通"结束进程"对它无效）
// 2. 每个卷索引了多少条目、占了多少字节、多久没同步了？
// 3. 现在在建索引的话，到哪一步了？还要多久？
//
// 「索引缓存复用」是这里最该讲清楚的事：有快照就直接装载（秒级可搜），
// 之后靠 USN Journal 增量保鲜——只有 Journal 失效才需要全量重建。

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  Database,
  HardDrive,
  Loader2,
  Play,
  RefreshCw,
  Square,
} from "lucide-react";
import type { BuildProgress, FilesearchStatus, IndexStatus } from "./types";
import { errorText, formatAgo, formatBytes } from "./format";

/** 阶段 → 人话。进度条上的那一句。 */
const PHASE_TEXT: Record<string, string> = {
  scanning: "枚举 MFT",
  sizing: "读取文件体积",
  indexing: "建立索引表",
  saving: "写入索引快照",
  done: "已完成",
  failed: "失败",
};

export function IndexPane() {
  const [state, setState] = useState<FilesearchStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const next = await invoke<FilesearchStatus>("viewer_filesearch_status");
      setState(next);
      setError(null);
    } catch (cause) {
      setError(errorText(cause));
    }
  }, []);

  useEffect(() => {
    void load();
    // 有索引在建时轮询更勤一点（进度要跟得上），空闲时 3 秒一次足够。
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [load]);

  const start = useCallback(async () => {
    setBusy(true);
    setNotice(null);
    try {
      setNotice(await invoke<string>("viewer_filesearch_start"));
      await load();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }, [load]);

  const stop = useCallback(async () => {
    setBusy(true);
    setNotice(null);
    try {
      setNotice(await invoke<string>("viewer_filesearch_stop"));
      await load();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }, [load]);

  const rebuild = useCallback(
    async (volume?: string) => {
      setBusy(true);
      setNotice(null);
      try {
        setNotice(
          await invoke<string>("viewer_filesearch_rebuild", { volume: volume ?? null }),
        );
        await load();
      } catch (cause) {
        setError(errorText(cause));
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  const volumes = state?.status?.volumes ?? [];
  const skipped = state?.status?.skipped ?? {};
  const building = volumes.filter((volume) => volume.build && !volume.build.done);
  const totalEntries = volumes.reduce((sum, volume) => sum + volume.totalEntries, 0);
  const totalBytes = volumes.reduce((sum, volume) => sum + volume.indexedBytes, 0);

  return (
    <div className="pane">
      <div className="index-head">
        <div className="index-head-main">
          <span className="clean-head-title">文件索引</span>
          <span className="clean-head-sub">
            {state?.running
              ? `服务运行中 · 共 ${totalEntries.toLocaleString()} 条 · 已索引 ${formatBytes(totalBytes)}`
              : "服务未运行"}
          </span>
        </div>
        <div className="index-head-actions">
          {state?.running ? (
            <button type="button" className="pane-btn" onClick={() => void stop()} disabled={busy}>
              <Square size={13} aria-hidden="true" />
              停止服务
            </button>
          ) : (
            <button type="button" className="pane-btn" onClick={() => void start()} disabled={busy}>
              <Play size={13} aria-hidden="true" />
              启动索引服务
            </button>
          )}
          <button
            type="button"
            className="pane-btn"
            onClick={() => void rebuild()}
            disabled={busy || !state?.running}
            title="按需重建：已有快照的卷直接装载，只有失效的卷才全量重建"
          >
            {busy ? (
              <Loader2 size={13} className="spin" />
            ) : (
              <RefreshCw size={13} aria-hidden="true" />
            )}
            重建索引
          </button>
        </div>
      </div>

      {error && <p className="pane-error">{error}</p>}
      {notice && <p className="pane-notice">{notice}</p>}
      {!state?.running && state?.reason && (
        <p className="pane-hint">{state.reason}——启动后需要管理员权限读取 MFT。</p>
      )}

      {building.map((volume) => (
        <BuildProgressBar key={volume.volume ?? "?"} build={volume.build!} />
      ))}

      <div className="index-table" role="table" aria-label="各卷索引状态">
        <div className="index-row index-head-row" role="row">
          <span role="columnheader">卷</span>
          <span role="columnheader" className="is-num">
            条目数
          </span>
          <span role="columnheader" className="is-num">
            已索引体积
          </span>
          <span role="columnheader">上次同步</span>
          <span role="columnheader">状态</span>
          <span role="columnheader">操作</span>
        </div>
        <div className="index-scroll">
          {volumes.map((volume) => (
            <IndexRow
              key={volume.volume ?? "?"}
              volume={volume}
              busy={busy}
              onRebuild={() => void rebuild(volume.volume ?? undefined)}
            />
          ))}
          {volumes.length === 0 && (
            <p className="pane-empty">还没有卷的索引——启动服务后会自动建立。</p>
          )}
        </div>
      </div>

      {Object.keys(skipped).length > 0 && (
        <div className="index-skipped">
          <span className="clean-head-warn">
            <AlertTriangle size={12} aria-hidden="true" />
            以下卷未建立索引
          </span>
          {Object.entries(skipped).map(([volume, reason]) => (
            <span key={volume} className="index-skip-row">
              <strong>{volume}</strong>
              <span className="dim">{reason}</span>
            </span>
          ))}
        </div>
      )}

      <p className="pane-hint index-foot-note">
        <Database size={12} aria-hidden="true" />
        有快照的卷直接装载（秒级可搜），之后靠 USN Journal 增量保鲜；只有 Journal
        失效才会全量重建。
      </p>
    </div>
  );
}

/** 建索引进度条：阶段 + 真实数字（条目数 / 字节数 / 已用时）。 */
function BuildProgressBar({ build }: { build: BuildProgress }) {
  const phase = PHASE_TEXT[build.phase] ?? build.phase;
  return (
    <div className="index-progress">
      <div className="index-progress-head">
        <span className="clean-head-title">{build.volume} 正在建索引</span>
        <span className="dim">{phase}</span>
      </div>
      <div className="progress is-indeterminate">
        <div className="progress-fill" />
      </div>
      <span className="progress-label">
        已处理 {build.entries.toLocaleString()} 个条目
        {build.bytes > 0 && ` · 已读 ${formatBytes(build.bytes)}`} · 用时{" "}
        {(build.elapsedMs / 1000).toFixed(1)} 秒
      </span>
    </div>
  );
}

/** 一个卷的索引状态行。 */
function IndexRow({
  volume,
  busy,
  onRebuild,
}: {
  volume: IndexStatus;
  busy: boolean;
  onRebuild: () => void;
}) {
  const stateText = useMemo(() => {
    if (volume.build && !volume.build.done) return "建索引中";
    if (volume.needsRebuild) return "待重建";
    switch (volume.monitorState) {
      case "Watching":
        return "监听中";
      case "Syncing":
        return "同步中";
      case "Error":
        return "出错";
      case "NeedsRebuild":
        return "待重建";
      default:
        return volume.isIndexed ? "已就绪" : "未建立";
    }
  }, [volume]);

  return (
    <div className="index-row" role="row">
      <span role="cell" className="index-volume">
        <HardDrive size={12} aria-hidden="true" />
        {volume.volume ?? "—"}
      </span>
      <span role="cell" className="is-num mono">
        {volume.totalEntries.toLocaleString()}
      </span>
      <span role="cell" className="is-num mono">
        {formatBytes(volume.indexedBytes)}
      </span>
      <span role="cell" className="dim">
        {formatAgo(volume.lastSyncAt)}
      </span>
      <span role="cell" className="index-state">
        {stateText}
        {volume.snapshotLoaded && <span className="chip">快照复用</span>}
        {volume.indexSource && <span className="chip">{volume.indexSource}</span>}
      </span>
      <span role="cell">
        <button
          type="button"
          className="inline-btn"
          onClick={onRebuild}
          disabled={busy}
          title="重建该卷的索引"
          aria-label={`重建 ${volume.volume ?? ""} 的索引`}
        >
          <RefreshCw size={12} />
        </button>
      </span>
    </div>
  );
}
