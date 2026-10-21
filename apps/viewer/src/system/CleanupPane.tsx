// 磁盘清理页：默认「垃圾扫描」，点磁盘进「大文件清理」。
//
// 交互逻辑照主流清理工具（Windows 磁盘清理 / CCleaner / BleachBit）：
// 1. **默认是垃圾扫描**——按规则列出"确定是垃圾"的项，按类别分组，
//    每项写清楚**清理依据**与**规则出处**，并给出可释放空间。
//    不做全盘扫描：打开面板不该等一分钟。
// 2. **点某个磁盘**才进入该盘视图：结果分「建议清理」（规则命中的可删项）
//    与「大文件」（仅参考，按体积排序）两块，各自标明依据。
// 3. 删除永远需要人勾选 + 二次确认；服务端会重新扫描再删，不信前端路径。

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  HardDrive,
  Loader2,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { Treemap, type TreemapNode } from "../Treemap";
import type {
  CleanItem,
  CleanResult,
  CleanupCandidate,
  CleanupProgress,
  DiskRow,
  DriveScanReport,
  RemoveResult,
  ScanReport,
} from "./types";
import { errorText, formatBytes, formatIdle, useAnimatedNumber } from "./format";

export function CleanupPane() {
  const [disks, setDisks] = useState<DiskRow[]>([]);
  const [view, setView] = useState<"junk" | "drive">("junk");
  const [drive, setDrive] = useState<string | null>(null);
  // 进度事件：Rust 每完成一条规则 / 每 400ms 推一次，前端据此画进度条。
  const [progress, setProgress] = useState<CleanupProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void invoke<DiskRow[]>("sysmon_disks")
      .then(setDisks)
      .catch((cause) => setError(errorText(cause)));
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<CleanupProgress>("cleanup-progress", (event) => {
      if (!disposed) setProgress(event.payload);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const openDrive = useCallback((target: string) => {
    setDrive(target);
    setView("drive");
  }, []);

  return (
    <div className="pane">
      {view === "junk" ? (
        <JunkScanView disks={disks} progress={progress} error={error} onOpenDrive={openDrive} />
      ) : (
        <DriveCleanupView
          drive={drive}
          progress={progress}
          error={error}
          onBack={() => setView("junk")}
        />
      )}
    </div>
  );
}

// ── 垃圾扫描（默认视图） ──────────────────────────────────────────────

function JunkScanView({
  disks,
  progress,
  error,
  onOpenDrive,
}: {
  disks: DiskRow[];
  progress: CleanupProgress | null;
  error: string | null;
  onOpenDrive: (drive: string) => void;
}) {
  const [report, setReport] = useState<ScanReport | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [result, setResult] = useState<CleanResult | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);

  const scan = useCallback(async () => {
    setScanning(true);
    setLocalError(null);
    setResult(null);
    try {
      const next = await invoke<ScanReport>("cleanup_scan", { drive: null });
      setReport(next);
      // 默认勾选：可用、有体积、且不是危险项。危险项（更新缓存）必须人手动选。
      setSelected(
        new Set(
          next.items
            .filter((item) => item.bytes > 0 && item.available && !item.risky)
            .map((item) => item.id),
        ),
      );
    } catch (cause) {
      setLocalError(errorText(cause));
    } finally {
      setScanning(false);
    }
  }, []);

  // 进入面板就扫一次——垃圾扫描是秒级的，不需要用户点。
  useEffect(() => {
    void scan();
  }, [scan]);

  useEffect(() => {
    if (!confirming) return;
    const timer = window.setTimeout(() => setConfirming(false), 5000);
    return () => window.clearTimeout(timer);
  }, [confirming]);

  const groups = useMemo(() => {
    const map = new Map<string, CleanItem[]>();
    for (const item of report?.items ?? []) {
      const list = map.get(item.group) ?? [];
      list.push(item);
      map.set(item.group, list);
    }
    // 组内按体积降序：用户先看到"最大的那类"。
    for (const list of map.values()) list.sort((a, b) => b.bytes - a.bytes);
    return Array.from(map.entries());
  }, [report]);

  const items = report?.items ?? [];
  const selectedItems = items.filter((item) => selected.has(item.id));
  const selectedBytes = selectedItems.reduce((sum, item) => sum + item.bytes, 0);
  const animatedSelected = useAnimatedNumber(selectedBytes);
  const totalBytes = Math.max(1, report?.totalBytes ?? 0);
  const foundCount = items.filter((item) => item.bytes > 0).length;

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const runClean = useCallback(async () => {
    if (!confirming) {
      setConfirming(true);
      return;
    }
    setConfirming(false);
    setCleaning(true);
    setLocalError(null);
    try {
      const outcome = await invoke<CleanResult>("cleanup_execute", {
        ids: Array.from(selected),
        drive: null,
      });
      setResult(outcome);
      await scan();
    } catch (cause) {
      setLocalError(errorText(cause));
    } finally {
      setCleaning(false);
    }
  }, [confirming, scan, selected]);

  const busy = scanning || cleaning;

  return (
    <>
      <div className="clean-head">
        <div className="clean-head-main">
          <span className="clean-head-title">垃圾扫描</span>
          <span className="clean-head-sub">
            {report
              ? `发现 ${foundCount} 类垃圾 · 共 ${formatBytes(report.totalBytes)} · ${report.totalFiles.toLocaleString()} 个文件`
              : scanning
                ? "正在按规则扫描…"
                : "尚未扫描"}
          </span>
          {report?.truncated && (
            <span className="clean-head-warn">
              <AlertTriangle size={12} aria-hidden="true" />
              扫描达到时间预算，结果可能不完整
            </span>
          )}
        </div>
        <button type="button" className="pane-btn" onClick={() => void scan()} disabled={busy}>
          {scanning ? (
            <Loader2 size={14} className="spin" />
          ) : (
            <RefreshCw size={14} />
          )}
          {scanning ? "扫描中…" : "重新扫描"}
        </button>
      </div>

      <div className="drive-strip">
        <span className="drive-strip-label">按磁盘查看大文件与可清理项</span>
        {disks.map((disk) => {
          const usedPct = disk.totalBytes
            ? ((disk.totalBytes - disk.freeBytes) / disk.totalBytes) * 100
            : 0;
          return (
            <button
              type="button"
              key={disk.drive}
              className="drive-chip"
              onClick={() => onOpenDrive(disk.drive)}
              title={`${disk.mount} · 可用 ${formatBytes(disk.freeBytes)} / ${formatBytes(disk.totalBytes)}`}
            >
              <span className="drive-chip-letter">
                <HardDrive size={11} aria-hidden="true" />
                {disk.drive}
              </span>
              <span className="drive-chip-bar" aria-hidden="true">
                <span
                  className={`drive-chip-fill${usedPct >= 85 ? " is-high" : usedPct >= 70 ? " is-warn" : ""}`}
                  style={{ transform: `scaleX(${usedPct / 100})` }}
                />
              </span>
              <span className="drive-chip-meta">{formatBytes(disk.freeBytes)} 可用</span>
            </button>
          );
        })}
      </div>

      {busy && progress && (
        <div className="progress-wrap">
          <div className="progress is-determinate">
            <div
              className="progress-fill"
              style={{
                transform: `scaleX(${Math.min(
                  100,
                  (progress.done / Math.max(1, progress.total)) * 100,
                ) / 100})`,
              }}
            />
          </div>
          <span className="progress-label">
            {cleaning
              ? `正在清理：${progress.title ?? ""}（${progress.done}/${progress.total}）`
              : `正在扫描：${progress.title ?? ""}（${progress.done}/${progress.total}）`}
          </span>
        </div>
      )}

      {(localError || error) && <p className="pane-error">{localError || error}</p>}
      {result && (
        <div className="result-banner">
          <CheckCircle2 size={15} aria-hidden="true" />
          <span>
            已清理 <strong>{result.deleted}</strong> 个文件，释放{" "}
            <strong>{formatBytes(result.freedBytes)}</strong>
            {result.skipped > 0 && ` · ${result.skipped} 个被占用已跳过`}
            {result.messages.length > 0 && ` · ${result.messages[0]}`}
          </span>
        </div>
      )}

      <div className="clean-list">
        {groups.map(([group, groupItems]) => {
          const groupBytes = groupItems.reduce((sum, item) => sum + item.bytes, 0);
          return (
            <section className="clean-group" key={group}>
              <header className="clean-group-head">
                <h4 className="clean-group-title">{group}</h4>
                <span className="clean-group-total">{formatBytes(groupBytes)}</span>
              </header>
              {groupItems.map((item) => (
                <CleanRow
                  key={item.id}
                  item={item}
                  share={Math.min(100, (item.bytes / totalBytes) * 100)}
                  checked={selected.has(item.id)}
                  disabled={busy || !item.available || item.bytes === 0}
                  onToggle={() => toggle(item.id)}
                />
              ))}
            </section>
          );
        })}
        {!report && !scanning && !localError && <p className="pane-empty">尚未扫描。</p>}
        {report && foundCount === 0 && (
          <p className="pane-empty">没有发现垃圾——系统挺干净的。</p>
        )}
      </div>

      <footer className="clean-foot">
        <span className="dim">
          已选 {selectedItems.length} 项 · 可释放{" "}
          <strong className="clean-total">{formatBytes(animatedSelected)}</strong>
          {report && ` · 扫描用时 ${report.elapsedMs} ms`}
        </span>
        <button
          type="button"
          className={`pane-btn${confirming ? " is-danger" : ""}`}
          onClick={() => void runClean()}
          disabled={busy || selectedItems.length === 0}
        >
          {cleaning ? <Loader2 size={14} className="spin" /> : <Trash2 size={14} />}
          {confirming ? "确认清理（不可恢复）" : "清理选中项"}
        </button>
      </footer>
    </>
  );
}

/** 一条清理项：勾选框 + 名称 + 依据 + 规则出处 + 体积。 */
function CleanRow({
  item,
  share,
  checked,
  disabled,
  onToggle,
}: {
  item: CleanItem;
  share: number;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
}) {
  return (
    <label
      className={`clean-item${disabled ? " is-disabled" : ""}${checked ? " is-checked" : ""}`}
      title={[
        ...item.roots,
        ...(item.rootsTotal > item.roots.length
          ? [`…共 ${item.rootsTotal} 个位置`]
          : []),
      ].join("\n")}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={onToggle}
      />
      <span className="clean-main">
        <span className="clean-name">
          {item.title}
          {item.risky && (
            <span className="clean-tag is-warn">
              <AlertTriangle size={11} aria-hidden="true" />
              谨慎
            </span>
          )}
          {item.needsAdmin && <span className="clean-tag">需管理员</span>}
        </span>
        <span className="clean-reason">{item.hint}</span>
        <span className="clean-source">
          规则来源：{item.source}
          {item.rootsTotal > 1 && ` · ${item.rootsTotal} 个位置`}
        </span>
        <span className="clean-bar" aria-hidden="true">
          <span className="clean-bar-fill" style={{ transform: `scaleX(${share / 100})` }} />
        </span>
      </span>
      <span className="clean-size">
        {formatBytes(item.bytes)}
        <span className="dim"> · {item.files.toLocaleString()} 个</span>
      </span>
    </label>
  );
}

// ── 大文件清理（点磁盘进入） ──────────────────────────────────────────

function DriveCleanupView({
  drive,
  progress,
  error,
  onBack,
}: {
  drive: string | null;
  progress: CleanupProgress | null;
  error: string | null;
  onBack: () => void;
}) {
  const [report, setReport] = useState<DriveScanReport | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [result, setResult] = useState<RemoveResult | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);
  const [showTree, setShowTree] = useState(false);
  const [treePath, setTreePath] = useState<string | null>(null);

  const scan = useCallback(async () => {
    if (!drive) return;
    setBusy(true);
    setLocalError(null);
    setResult(null);
    try {
      const next = await invoke<DriveScanReport>("cleanup_scan_drive", { drive });
      setReport(next);
      // 一个都不勾：这些是用户目录里的东西，必须由人明确选择。
      setSelected(new Set());
      setTreePath(null);
    } catch (cause) {
      setLocalError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }, [drive]);

  useEffect(() => {
    void scan();
  }, [scan]);

  useEffect(() => {
    if (!confirming) return;
    const timer = window.setTimeout(() => setConfirming(false), 5000);
    return () => window.clearTimeout(timer);
  }, [confirming]);

  const toggle = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const selectedBytes = useMemo(() => {
    if (!report) return 0;
    return [...report.suggested, ...report.big]
      .filter((entry) => selected.has(entry.path))
      .reduce((sum, entry) => sum + entry.bytes, 0);
  }, [report, selected]);

  const clean = useCallback(async () => {
    if (!report || !drive) return;
    if (!confirming) {
      setConfirming(true);
      return;
    }
    setConfirming(false);
    setBusy(true);
    setLocalError(null);
    try {
      // 按删除方式分两批：目录（构建产物）与文件（其余）。
      const picked = (list: { path: string; target?: string }[]) =>
        list.filter((entry) => selected.has(entry.path));
      const artifacts = picked(report.suggested.filter((item) => item.target === "artifact"));
      const files = [
        ...picked(report.suggested.filter((item) => item.target !== "artifact")),
        ...picked(report.big),
      ];

      let freed = 0;
      let removed = 0;
      let failed = 0;
      for (const [kind, paths] of [
        ["artifact", artifacts.map((item) => item.path)],
        ["file", files.map((item) => item.path)],
      ] as Array<[string, string[]]>) {
        if (paths.length === 0) continue;
        const outcome = await invoke<RemoveResult>("cleanup_remove_entries", {
          kind,
          paths,
          drive,
        });
        freed += outcome.freedBytes;
        removed += outcome.removed;
        failed += outcome.failed;
      }
      setResult({ freedBytes: freed, removed, failed, messages: [] });
      await scan();
    } catch (cause) {
      setLocalError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }, [confirming, drive, report, scan, selected]);

  // 目录体积树：Rust 给的是扁平列表（parent → children），这里按 parent 分组。
  const dirChildren = useMemo(() => {
    const map = new Map<string, { path: string; bytes: number }[]>();
    for (const node of report?.dirs ?? []) {
      const list = map.get(node.parent) ?? [];
      list.push({ path: node.path, bytes: node.bytes });
      map.set(node.parent, list);
    }
    for (const list of map.values()) list.sort((a, b) => b.bytes - a.bytes);
    return map;
  }, [report]);

  const treeNodes = useMemo<TreemapNode[]>(() => {
    const rootKey = drive ? `${drive}\\` : "";
    return (dirChildren.get(treePath ?? rootKey) ?? []).map((node) => ({
      path: node.path,
      name: node.path.split("\\").filter(Boolean).pop() ?? node.path,
      bytes: node.bytes,
    }));
  }, [dirChildren, treePath, drive]);

  const treeCrumbs = useMemo(() => {
    if (!treePath || !drive) return [];
    const prefix = `${drive}\\`;
    const rest = treePath.startsWith(prefix) ? treePath.slice(prefix.length) : treePath;
    let acc = prefix;
    return rest
      .split("\\")
      .filter(Boolean)
      .map((part) => {
        acc += `${part}\\`;
        return { name: part, path: acc };
      });
  }, [treePath, drive]);

  const suggestedBytes = report?.suggested.reduce((sum, item) => sum + item.bytes, 0) ?? 0;

  return (
    <>
      <div className="clean-head">
        <div className="clean-head-main">
          <button type="button" className="clean-back" onClick={onBack}>
            <ArrowLeft size={13} aria-hidden="true" />
            返回垃圾扫描
          </button>
          <span className="clean-head-title">{drive} 大文件清理</span>
          <span className="clean-head-sub">
            {report
              ? `扫过 ${report.dirsScanned.toLocaleString()} 个目录 · 用时 ${report.elapsedMs} ms`
              : busy
                ? "正在遍历磁盘…"
                : ""}
          </span>
          {report?.truncated && (
            <span className="clean-head-warn">
              <AlertTriangle size={12} aria-hidden="true" />
              已达扫描预算，结果不完整
            </span>
          )}
        </div>
        <button type="button" className="pane-btn" onClick={() => void scan()} disabled={busy}>
          {busy ? (
            <Loader2 size={14} className="spin" />
          ) : (
            <RefreshCw size={14} />
          )}
          重新扫描
        </button>
      </div>

      {busy && progress?.phase === "drive" && (
        <div className="progress-wrap">
          <div className="progress is-indeterminate">
            <div className="progress-fill" />
          </div>
          <span className="progress-label">
            已扫描 {progress.done.toLocaleString()} 个目录 · 用时 {progress.elapsedMs ?? 0} ms
          </span>
        </div>
      )}

      {(localError || error) && <p className="pane-error">{localError || error}</p>}
      {result && (
        <div className="result-banner">
          <CheckCircle2 size={15} aria-hidden="true" />
          <span>
            已清理 <strong>{result.removed}</strong> 项，释放{" "}
            <strong>{formatBytes(result.freedBytes)}</strong>
            {result.failed > 0 && ` · ${result.failed} 项未成功`}
          </span>
        </div>
      )}

      <div className="clean-list">
        <section className="clean-group">
          <header className="clean-group-head">
            <h4 className="clean-group-title">建议清理</h4>
            <span className="clean-group-total">
              {report?.suggested.length ?? 0} 项 · {formatBytes(suggestedBytes)}
              {report && report.suggestedTotal > report.suggested.length
                ? `（共命中 ${report.suggestedTotal} 项）`
                : ""}
            </span>
          </header>
          <p className="clean-note">
            按规则命中的可删除项——每条都写明依据。默认全不勾选，请自行确认。
          </p>
          {report?.suggested.map((item) => (
            <CandidateRow
              key={item.path}
              candidate={item}
              checked={selected.has(item.path)}
              disabled={busy}
              onToggle={() => toggle(item.path)}
            />
          ))}
          {report && report.suggested.length === 0 && (
            <p className="pane-empty">这块盘没有命中清理规则。</p>
          )}
        </section>

        <section className="clean-group">
          <header className="clean-group-head">
            <h4 className="clean-group-title">大文件</h4>
            <span className="clean-group-total">
              {report?.big.length ?? 0} 项 ·{" "}
              {formatBytes(report?.big.reduce((sum, item) => sum + item.bytes, 0) ?? 0)}
            </span>
          </header>
          <p className="clean-note">
            仅作参考：按体积排序，不预判可删。勾选后才会删除，请自行确认这些文件是否还需要。
          </p>
          {report?.big.map((item) => (
            <label
              key={item.path}
              className={`clean-item${selected.has(item.path) ? " is-checked" : ""}`}
              title={item.path}
            >
              <input
                type="checkbox"
                checked={selected.has(item.path)}
                disabled={busy}
                onChange={() => toggle(item.path)}
              />
              <span className="clean-main">
                <span className="clean-path">{item.path}</span>
                <span className="clean-reason">{item.reason}</span>
                <span className="clean-source">最后修改：{formatIdle(item.modified)}</span>
              </span>
              <span className="clean-size">{formatBytes(item.bytes)}</span>
            </label>
          ))}
          {report && report.big.length === 0 && (
            <p className="pane-empty">没有超过 200 MB 的文件。</p>
          )}
        </section>

        {report && report.dirs.length > 0 && (
          <section className="clean-group">
            <header className="clean-group-head">
              <h4 className="clean-group-title">目录体积</h4>
              <button
                type="button"
                className="clean-toggle"
                onClick={() => setShowTree((prev) => !prev)}
              >
                {showTree ? "收起" : "展开"}
              </button>
            </header>
            {showTree && (
              <>
                <div className="treemap-bar">
                  <button
                    type="button"
                    className="treemap-crumb"
                    onClick={() => setTreePath(null)}
                    disabled={!treePath}
                  >
                    盘根
                  </button>
                  {treeCrumbs.map((crumb) => (
                    <button
                      type="button"
                      key={crumb.path}
                      className="treemap-crumb"
                      onClick={() => setTreePath(crumb.path)}
                    >
                      {crumb.name}
                    </button>
                  ))}
                  <span className="dim">
                    共 {formatBytes(treeNodes.reduce((sum, node) => sum + node.bytes, 0))}
                  </span>
                </div>
                <Treemap
                  nodes={treeNodes}
                  onSelect={(node) => setTreePath(node.path)}
                  selectedPath={treePath}
                />
              </>
            )}
          </section>
        )}
      </div>

      <footer className="clean-foot">
        <span className="dim">
          已选 {selected.size} 项 · 可释放{" "}
          <strong className="clean-total">{formatBytes(selectedBytes)}</strong>
        </span>
        <button
          type="button"
          className={`pane-btn${confirming ? " is-danger" : ""}`}
          onClick={() => void clean()}
          disabled={busy || selected.size === 0}
        >
          {busy ? <Loader2 size={14} className="spin" /> : <Trash2 size={14} />}
          {confirming ? "确认清理（不可恢复）" : "清理勾选项"}
        </button>
      </footer>
    </>
  );
}

/** 一条建议清理项：类别 + 路径 + 依据。 */
function CandidateRow({
  candidate,
  checked,
  disabled,
  onToggle,
}: {
  candidate: CleanupCandidate;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
}) {
  return (
    <label
      className={`clean-item${checked ? " is-checked" : ""}`}
      title={candidate.path}
    >
      <input type="checkbox" checked={checked} disabled={disabled} onChange={onToggle} />
      <span className="clean-main">
        <span className="clean-name">
          <span className="clean-tag">{candidate.category}</span>
          <span className="clean-path">{candidate.path}</span>
        </span>
        <span className="clean-reason">依据：{candidate.reason}</span>
        <span className="clean-source">
          最后修改：{formatIdle(candidate.modified)}
          {candidate.target === "artifact" && " · 整个目录可删"}
        </span>
      </span>
      <span className="clean-size">
        {formatBytes(candidate.bytes)}
        <span className="dim"> · {candidate.files.toLocaleString()} 个</span>
      </span>
    </label>
  );
}
