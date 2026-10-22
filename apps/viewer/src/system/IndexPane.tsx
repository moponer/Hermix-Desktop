// 文件索引页：两种模式，由用户自己选。
//
// ① **MFT 索引模式（需管理员）**：用户点「授权并启动 MFT 索引」→ 系统弹
//    一次 UAC → 提权 helper 读 NTFS 的 `$MFT` → 全盘索引 + Everything 式
//    全盘搜索 + USN 增量保鲜。**提权只由这一个按钮触发**，页面其它地方
//    （搜索、磁盘体积）不会替用户弹 UAC。
// ② **非管理员扫描模式（默认）**：不授权也能用——按当前权限遍历能读到的
//    目录，给出文件数、体积与大文件。读不到的地方如实计数，不假装扫全。
//
// 「默认是哪种模式」这件事必须写在页面上：用户不点授权，那就是模式 ②。

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  Database,
  FolderSearch,
  HardDrive,
  KeyRound,
  Loader2,
  RefreshCw,
  ShieldCheck,
  Square,
} from "lucide-react";
import type {
  BuildProgress,
  FilesearchStatus,
  IndexStatus,
  LocalScanReport,
  PrivilegeState,
} from "./types";
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
  const [privilege, setPrivilege] = useState<PrivilegeState | null>(null);
  const [state, setState] = useState<FilesearchStatus | null>(null);
  const [local, setLocal] = useState<LocalScanReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [scanning, setScanning] = useState(false);

  const loadPrivilege = useCallback(async () => {
    try {
      setPrivilege(await invoke<PrivilegeState>("viewer_filesearch_privilege"));
    } catch (cause) {
      setError(errorText(cause));
    }
  }, []);

  const loadStatus = useCallback(async () => {
    try {
      setState(await invoke<FilesearchStatus>("viewer_filesearch_status"));
    } catch (cause) {
      setError(errorText(cause));
    }
  }, []);

  useEffect(() => {
    void loadPrivilege();
    void loadStatus();
    // MFT 模式下进度要跟得上（建索引时 2 秒一次），非 MFT 模式 5 秒一次足够。
    const timer = window.setInterval(() => {
      void loadPrivilege();
      void loadStatus();
    }, 2000);
    return () => window.clearInterval(timer);
  }, [loadPrivilege, loadStatus]);

  /** 授权入口：唯一会弹 UAC 的地方。 */
  const authorize = useCallback(async () => {
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      setNotice(await invoke<string>("viewer_filesearch_start"));
      await loadPrivilege();
      await loadStatus();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }, [loadPrivilege, loadStatus]);

  const stop = useCallback(async () => {
    setBusy(true);
    setNotice(null);
    try {
      setNotice(await invoke<string>("viewer_filesearch_stop"));
      await loadPrivilege();
      await loadStatus();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }, [loadPrivilege, loadStatus]);

  const rebuild = useCallback(
    async (volume?: string) => {
      setBusy(true);
      setNotice(null);
      try {
        setNotice(
          await invoke<string>("viewer_filesearch_rebuild", { volume: volume ?? null }),
        );
        await loadStatus();
      } catch (cause) {
        setError(errorText(cause));
      } finally {
        setBusy(false);
      }
    },
    [loadStatus],
  );

  /** 非管理员扫描：不提权，按当前权限遍历。 */
  const scanLocal = useCallback(async () => {
    setScanning(true);
    setNotice(null);
    setError(null);
    try {
      setLocal(await invoke<LocalScanReport>("viewer_filesearch_local_scan"));
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setScanning(false);
    }
  }, []);

  const mftMode = privilege?.mode === "mft";
  const volumes = state?.status?.volumes ?? [];
  const skipped = state?.status?.skipped ?? {};
  const building = volumes.filter((volume) => volume.build && !volume.build.done);
  const totalEntries = volumes.reduce((sum, volume) => sum + volume.totalEntries, 0);
  const totalBytes = volumes.reduce((sum, volume) => sum + volume.indexedBytes, 0);

  return (
    <div className="pane">
      {/* ── 模式卡片：说清现在是哪种模式、另一种怎么切 ─────────────── */}
      <div className={`mode-card${mftMode ? " is-mft" : ""}`}>
        <span className="mode-icon">
          {mftMode ? (
            <ShieldCheck size={16} aria-hidden="true" />
          ) : (
            <FolderSearch size={16} aria-hidden="true" />
          )}
        </span>
        <div className="mode-main">
          <span className="mode-title">
            {mftMode ? "MFT 索引模式（已授权）" : "非管理员扫描模式（默认）"}
          </span>
          <span className="mode-desc">
            {mftMode
              ? "索引服务已获授权，能读取 NTFS 的 $MFT：全盘文件名搜索、目录体积秒级出结果、USN 增量保鲜。"
              : "未授权：只统计您有权限访问的目录，不建立全盘索引，也没有全盘文件名搜索。需要全盘能力请点右侧授权。"}
          </span>
          {!mftMode && (
            <span className="mode-note">
              <AlertTriangle size={12} aria-hidden="true" />
              授权会弹一次系统管理员确认——点「授权并启动 MFT 索引」才会弹；不点就一直用扫描模式。
            </span>
          )}
        </div>
        <div className="mode-actions">
          {mftMode ? (
            <>
              <button
                type="button"
                className="pane-btn"
                onClick={() => void stop()}
                disabled={busy}
              >
                <Square size={13} aria-hidden="true" />
                停止服务
              </button>
              <button
                type="button"
                className="pane-btn"
                onClick={() => void rebuild()}
                disabled={busy}
                title="按需重建：已有快照的卷直接装载，只有失效的卷才全量重建"
              >
                {busy ? (
                  <Loader2 size={13} className="spin" />
                ) : (
                  <RefreshCw size={13} aria-hidden="true" />
                )}
                重建索引
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                className="pane-btn"
                onClick={() => void scanLocal()}
                disabled={scanning}
                title="按当前权限遍历，不需要管理员"
              >
                {scanning ? (
                  <Loader2 size={13} className="spin" />
                ) : (
                  <FolderSearch size={13} aria-hidden="true" />
                )}
                {scanning ? "扫描中…" : "按当前权限扫描"}
              </button>
              <button
                type="button"
                className="pane-btn is-primary"
                onClick={() => void authorize()}
                disabled={busy}
                title="会弹一次系统管理员授权确认"
              >
                {busy ? (
                  <Loader2 size={13} className="spin" />
                ) : (
                  <KeyRound size={13} aria-hidden="true" />
                )}
                授权并启动 MFT 索引
              </button>
            </>
          )}
        </div>
      </div>

      {error && <p className="pane-error">{error}</p>}
      {notice && <p className="pane-notice">{notice}</p>}

      {mftMode ? (
        <MftSection
          volumes={volumes}
          building={building}
          totalEntries={totalEntries}
          totalBytes={totalBytes}
          skipped={skipped}
          busy={busy}
          onRebuild={(volume) => void rebuild(volume)}
        />
      ) : (
        <ScanSection report={local} scanning={scanning} />
      )}
    </div>
  );
}

/** MFT 模式：各卷索引状态 + 建索引进度。 */
function MftSection({
  volumes,
  building,
  totalEntries,
  totalBytes,
  skipped,
  busy,
  onRebuild,
}: {
  volumes: IndexStatus[];
  building: IndexStatus[];
  totalEntries: number;
  totalBytes: number;
  skipped: Record<string, string>;
  busy: boolean;
  onRebuild: (volume?: string) => void;
}) {
  return (
    <>
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
              onRebuild={() => onRebuild(volume.volume ?? undefined)}
            />
          ))}
          {volumes.length === 0 && (
            <p className="pane-empty">还没有卷的索引——授权后会自动建立。</p>
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
        共 {totalEntries.toLocaleString()} 条 · {formatBytes(totalBytes)}。有快照的卷直接装载
        （秒级可搜），之后靠 USN Journal 增量保鲜；只有 Journal 失效才会全量重建。
      </p>
    </>
  );
}

/** 非管理员模式：按当前权限扫出来的结果。 */
function ScanSection({
  report,
  scanning,
}: {
  report: LocalScanReport | null;
  scanning: boolean;
}) {
  if (!report) {
    return (
      <p className="pane-empty">
        {scanning ? "正在按当前权限遍历磁盘…" : "尚未扫描——点右上角「按当前权限扫描」。"}
      </p>
    );
  }
  return (
    <>
      <div className="scan-summary">
        <span className="clean-head-title">扫描结果（普通权限）</span>
        <span className="clean-head-sub">
          {report.volumes.length} 块盘 · {report.totalFiles.toLocaleString()} 个文件 ·{" "}
          {formatBytes(report.totalBytes)} · 用时 {(report.elapsedMs / 1000).toFixed(1)} 秒
        </span>
      </div>

      {report.deniedDirs > 0 && (
        <p className="scan-warn">
          <AlertTriangle size={12} aria-hidden="true" />
          {report.deniedDirs.toLocaleString()} 个目录读不到（权限不足）——本次统计**不完整**，
          授权后可覆盖这些位置。
        </p>
      )}

      <div className="index-table" role="table" aria-label="各盘扫描结果">
        <div className="index-row index-head-row" role="row">
          <span role="columnheader">盘</span>
          <span role="columnheader" className="is-num">
            文件数
          </span>
          <span role="columnheader" className="is-num">
            体积
          </span>
          <span role="columnheader" className="is-num">
            目录数
          </span>
          <span role="columnheader">用时</span>
          <span role="columnheader">完整性</span>
        </div>
        <div className="index-scroll">
          {report.volumes.map((volume) => (
            <div className="index-row" role="row" key={volume.drive}>
              <span role="cell" className="index-volume">
                <HardDrive size={12} aria-hidden="true" />
                {volume.drive}
              </span>
              <span role="cell" className="is-num mono">
                {volume.totalFiles.toLocaleString()}
              </span>
              <span role="cell" className="is-num mono">
                {formatBytes(volume.totalBytes)}
              </span>
              <span role="cell" className="is-num mono">
                {volume.dirsScanned.toLocaleString()}
              </span>
              <span role="cell" className="mono dim">
                {(volume.elapsedMs / 1000).toFixed(1)}s
              </span>
              <span role="cell" className="index-state">
                {volume.truncated ? (
                  <span className="clean-tag is-warn">超时截断</span>
                ) : volume.deniedDirs > 0 ? (
                  <span className="clean-tag is-warn">{volume.deniedDirs} 处读不到</span>
                ) : (
                  <span className="clean-tag">完整</span>
                )}
              </span>
            </div>
          ))}
        </div>
      </div>

      {report.volumes.some((volume) => volume.truncated) && (
        <p className="scan-warn">
          <AlertTriangle size={12} aria-hidden="true" />
          有盘的扫描触发了 45 秒预算上限，结果**被截断**——数字偏小是正常的。
          完整覆盖需要 MFT 索引（授权后由内核直读，不受遍历速度限制）。
        </p>
      )}

      <p className="pane-hint index-foot-note">
        <FolderSearch size={12} aria-hidden="true" />
        扫描模式只做统计，**不建立文件名索引**——全盘搜索需要授权后由 MFT 索引提供。
      </p>
    </>
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
