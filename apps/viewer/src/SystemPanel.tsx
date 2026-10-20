// 系统资源管理弹窗：进程 + 磁盘清理。
//
// 设计取舍：
// - **弹窗而不是新窗口**：菜单点开就在当前窗口浮出，Esc / 点遮罩关闭，
//   不制造第二个窗口去争夺焦点（用户要求"点击菜单弹出窗口"）。
// - 进程页 3 秒自动刷新，但只在面板可见时跑；结束进程要二次确认，
//   关键进程的服务端会拒绝、按钮也直接禁用。
// - 清理页先扫后勾：默认只勾"安全且有内容"的项，危险项（如更新缓存）
//   永远默认不勾；清理按钮同样二次确认，完成后自动重扫。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  AlertTriangle,
  ChevronRight,
  HardDrive,
  Loader2,
  RefreshCw,
  Search,
  ShieldAlert,
  Trash2,
  X,
} from "lucide-react";

// ── 与 Rust 侧 serde(camelCase) 对齐的类型 ────────────────────────────

interface ProcessRow {
  pid: number;
  parentPid: number | null;
  name: string;
  exe: string | null;
  cmd: string;
  cpu: number;
  memoryMb: number;
  user: string | null;
  status: string;
  startedAt: number;
  description: string | null;
  company: string | null;
  critical: boolean;
}

interface ProcessSnapshot {
  processes: ProcessRow[];
  overview: {
    totalMemoryMb: number;
    usedMemoryMb: number;
    cpuCount: number;
    processCount: number;
  };
}

interface DiskRow {
  name: string;
  mount: string;
  totalBytes: number;
  freeBytes: number;
  removable: boolean;
  drive: string;
}

interface CleanItem {
  id: string;
  title: string;
  hint: string;
  group: string;
  bytes: number;
  files: number;
  risky: boolean;
  needsAdmin: boolean;
  roots: string[];
  available: boolean;
}

interface ScanReport {
  items: CleanItem[];
  totalBytes: number;
  totalFiles: number;
  elapsedMs: number;
  drive: string | null;
}

interface CleanResult {
  freedBytes: number;
  deleted: number;
  skipped: number;
  failed: number;
  messages: string[];
}

// ── 小工具 ────────────────────────────────────────────────────────────

function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

function formatTime(unixSeconds: number): string {
  if (!unixSeconds) return "未知";
  const date = new Date(unixSeconds * 1000);
  return date.toLocaleString();
}

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/** 3 秒自动刷新：只在面板可见时跑，避免后台白烧 CPU（每次快照要采两次样）。 */
function useInterval(callback: () => void, delayMs: number | null) {
  const saved = useRef(callback);
  saved.current = callback;
  useEffect(() => {
    if (delayMs === null) return;
    const timer = window.setInterval(() => saved.current(), delayMs);
    return () => window.clearInterval(timer);
  }, [delayMs]);
}

// ── 弹窗外壳 ──────────────────────────────────────────────────────────

export function SystemPanel({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<"processes" | "cleanup">("processes");

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="syspanel-overlay"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="syspanel" role="dialog" aria-modal="true" aria-label="系统资源管理">
        <header className="syspanel-head">
          <span className="syspanel-title">
            <Activity size={15} aria-hidden="true" />
            系统资源管理
          </span>
          <div className="syspanel-tabs" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={tab === "processes"}
              className={`syspanel-tab${tab === "processes" ? " is-active" : ""}`}
              onClick={() => setTab("processes")}
            >
              进程
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "cleanup"}
              className={`syspanel-tab${tab === "cleanup" ? " is-active" : ""}`}
              onClick={() => setTab("cleanup")}
            >
              磁盘清理
            </button>
          </div>
          <button type="button" className="syspanel-close" onClick={onClose} aria-label="关闭">
            <X size={16} />
          </button>
        </header>
        <div className="syspanel-body">
          {tab === "processes" ? <ProcessPane /> : <CleanupPane />}
        </div>
      </div>
    </div>
  );
}

// ── 进程 ──────────────────────────────────────────────────────────────

const MAX_ROWS = 400;

function ProcessPane() {
  const [snapshot, setSnapshot] = useState<ProcessSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [selectedPid, setSelectedPid] = useState<number | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmPid, setConfirmPid] = useState<number | null>(null);
  const busyRef = useRef(false);

  const load = useCallback(async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setLoading(true);
    try {
      const next = await invoke<ProcessSnapshot>("sysmon_processes");
      setSnapshot(next);
      setError(null);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      busyRef.current = false;
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);
  useInterval(() => void load(), 3000);

  // 二次确认 4 秒后自动撤销：避免"点了确认按钮一直挂着"。
  useEffect(() => {
    if (confirmPid === null) return;
    const timer = window.setTimeout(() => setConfirmPid(null), 4000);
    return () => window.clearTimeout(timer);
  }, [confirmPid]);

  const rows = useMemo(() => {
    const list = snapshot?.processes ?? [];
    const needle = query.trim().toLowerCase();
    if (!needle) return list.slice(0, MAX_ROWS);
    return list
      .filter(
        (row) =>
          row.name.toLowerCase().includes(needle) ||
          (row.exe ?? "").toLowerCase().includes(needle) ||
          row.cmd.toLowerCase().includes(needle) ||
          String(row.pid) === needle,
      )
      .slice(0, MAX_ROWS);
  }, [snapshot, query]);

  const selected = useMemo(
    () => rows.find((row) => row.pid === selectedPid) ?? null,
    [rows, selectedPid],
  );

  const kill = useCallback(
    async (row: ProcessRow) => {
      if (confirmPid !== row.pid) {
        setConfirmPid(row.pid);
        setNotice(`再次点击「确认结束」以结束 ${row.name}（PID ${row.pid}）`);
        return;
      }
      setConfirmPid(null);
      try {
        const message = await invoke<string>("sysmon_kill_process", {
          pid: row.pid,
          force: true,
        });
        setNotice(message);
      } catch (cause) {
        setNotice(errorText(cause));
      }
      void load();
    },
    [confirmPid, load],
  );

  return (
    <div className="pane">
      <div className="pane-bar">
        <div className="pane-stats">
          {snapshot && (
            <>
              <span>进程 {snapshot.overview.processCount}</span>
              <span>逻辑核心 {snapshot.overview.cpuCount}</span>
              <span>
                内存 {(snapshot.overview.usedMemoryMb / 1024).toFixed(1)} /{" "}
                {(snapshot.overview.totalMemoryMb / 1024).toFixed(1)} GB
              </span>
            </>
          )}
        </div>
        <label className="pane-search">
          <Search size={14} aria-hidden="true" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="按名称 / 路径 / PID 过滤"
            aria-label="过滤进程"
          />
        </label>
        <button
          type="button"
          className="pane-btn"
          onClick={() => void load()}
          disabled={loading}
          title="刷新（每 3 秒自动刷新）"
        >
          {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
          刷新
        </button>
      </div>

      {error && <p className="pane-error">{error}</p>}
      {notice && <p className="pane-notice">{notice}</p>}

      <div className="proc-table" role="table" aria-label="进程列表">
        <div className="proc-row proc-head" role="row">
          <span role="columnheader">名称</span>
          <span role="columnheader">PID</span>
          <span role="columnheader" className="num">
            CPU
          </span>
          <span role="columnheader" className="num">
            内存
          </span>
          <span role="columnheader">用户</span>
          <span role="columnheader">路径</span>
        </div>
        <div className="proc-scroll">
          {rows.map((row) => (
            <button
              type="button"
              role="row"
              key={row.pid}
              className={`proc-row${selectedPid === row.pid ? " is-selected" : ""}${
                row.critical ? " is-critical" : ""
              }`}
              onClick={() => setSelectedPid(row.pid)}
            >
              <span role="cell" className="proc-name" title={row.description ?? row.name}>
                {row.name}
                {row.critical && <ShieldAlert size={12} className="proc-lock" aria-hidden="true" />}
              </span>
              <span role="cell">{row.pid}</span>
              <span role="cell" className="num">
                {row.cpu.toFixed(1)}%
              </span>
              <span role="cell" className="num">
                {row.memoryMb >= 1024
                  ? `${(row.memoryMb / 1024).toFixed(2)} GB`
                  : `${Math.round(row.memoryMb)} MB`}
              </span>
              <span role="cell" className="dim">
                {row.user ?? "—"}
              </span>
              <span role="cell" className="dim ellipsis" title={row.exe ?? ""}>
                {row.exe ?? "—"}
              </span>
            </button>
          ))}
          {rows.length === 0 && !error && (
            <p className="pane-empty">没有匹配的进程。</p>
          )}
        </div>
      </div>

      {selected && (
        <div className="proc-detail">
          <div className="proc-detail-head">
            <strong>{selected.name}</strong>
            <span className="dim">PID {selected.pid}</span>
            {selected.parentPid !== null && (
              <span className="dim">父进程 {selected.parentPid}</span>
            )}
            <span className="dim">{selected.status}</span>
            <button
              type="button"
              className={`pane-btn${confirmPid === selected.pid ? " is-danger" : ""}`}
              onClick={() => void kill(selected)}
              disabled={selected.critical}
              title={
                selected.critical
                  ? "系统关键进程，结束会导致蓝屏或注销"
                  : "结束该进程（需二次确认）"
              }
            >
              <Trash2 size={14} />
              {confirmPid === selected.pid ? "确认结束" : "结束进程"}
            </button>
          </div>
          <dl className="proc-facts">
            <dt>作用</dt>
            <dd>
              {selected.description ?? "（该程序未声明文件说明）"}
              {selected.company && <span className="dim"> · {selected.company}</span>}
            </dd>
            <dt>路径</dt>
            <dd className="mono ellipsis" title={selected.exe ?? ""}>
              {selected.exe ?? "（无权限读取）"}
            </dd>
            <dt>命令行</dt>
            <dd className="mono ellipsis" title={selected.cmd}>
              {selected.cmd || "（无权限读取）"}
            </dd>
            <dt>启动时间</dt>
            <dd>{formatTime(selected.startedAt)}</dd>
          </dl>
        </div>
      )}
      {rows.length >= MAX_ROWS && (
        <p className="pane-hint">
          <ChevronRight size={12} aria-hidden="true" />
          仅显示前 {MAX_ROWS} 条，用上方过滤框缩小范围。
        </p>
      )}
    </div>
  );
}

// ── 磁盘清理 ──────────────────────────────────────────────────────────

function CleanupPane() {
  const [disks, setDisks] = useState<DiskRow[]>([]);
  const [drive, setDrive] = useState<string | null>(null);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CleanResult | null>(null);
  const [confirming, setConfirming] = useState(false);

  useEffect(() => {
    void invoke<DiskRow[]>("sysmon_disks")
      .then(setDisks)
      .catch((cause) => setError(errorText(cause)));
  }, []);

  const scan = useCallback(async (target: string | null) => {
    setScanning(true);
    setError(null);
    setResult(null);
    try {
      const next = await invoke<ScanReport>("cleanup_scan", { drive: target });
      setReport(next);
      // 默认勾选：有内容、不需要管理员、不是危险项。
      setSelected(
        new Set(
          next.items
            .filter((item) => item.bytes > 0 && item.available && !item.risky)
            .map((item) => item.id),
        ),
      );
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setScanning(false);
    }
  }, []);

  useEffect(() => {
    void scan(drive);
  }, [drive, scan]);

  useEffect(() => {
    if (!confirming) return;
    const timer = window.setTimeout(() => setConfirming(false), 5000);
    return () => window.clearTimeout(timer);
  }, [confirming]);

  const items = report?.items ?? [];
  const selectedItems = items.filter((item) => selected.has(item.id));
  const selectedBytes = selectedItems.reduce((sum, item) => sum + item.bytes, 0);

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
    setError(null);
    try {
      const outcome = await invoke<CleanResult>("cleanup_execute", {
        ids: Array.from(selected),
        drive,
      });
      setResult(outcome);
      await scan(drive);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setCleaning(false);
    }
  }, [confirming, drive, scan, selected]);

  const groups = useMemo(() => {
    const map = new Map<string, CleanItem[]>();
    for (const item of items) {
      const list = map.get(item.group) ?? [];
      list.push(item);
      map.set(item.group, list);
    }
    return Array.from(map.entries());
  }, [items]);

  return (
    <div className="pane">
      <div className="pane-bar">
        <div className="drive-chips" role="group" aria-label="选择盘符">
          <button
            type="button"
            className={`drive-chip${drive === null ? " is-active" : ""}`}
            onClick={() => setDrive(null)}
          >
            全部磁盘
          </button>
          {disks.map((disk) => (
            <button
              type="button"
              key={disk.drive}
              className={`drive-chip${drive === disk.drive ? " is-active" : ""}`}
              onClick={() => setDrive(disk.drive)}
              title={`${disk.mount} · 可用 ${formatBytes(disk.freeBytes)} / ${formatBytes(
                disk.totalBytes,
              )}`}
            >
              <HardDrive size={13} aria-hidden="true" />
              {disk.drive}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="pane-btn"
          onClick={() => void scan(drive)}
          disabled={scanning}
        >
          {scanning ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
          {scanning ? "扫描中…" : "重新扫描"}
        </button>
      </div>

      {error && <p className="pane-error">{error}</p>}
      {result && (
        <p className="pane-notice">
          已清理 {result.deleted} 个文件，释放 {formatBytes(result.freedBytes)}
          {result.skipped > 0 && `，${result.skipped} 个被占用已跳过`}
          {result.messages.length > 0 && ` · ${result.messages[0]}`}
        </p>
      )}

      <div className="clean-list">
        {groups.map(([group, groupItems]) => (
          <section className="clean-group" key={group}>
            <h4 className="clean-group-title">{group}</h4>
            {groupItems.map((item) => (
              <label
                key={item.id}
                className={`clean-item${item.available ? "" : " is-disabled"}`}
                title={item.roots.join("\n")}
              >
                <input
                  type="checkbox"
                  checked={selected.has(item.id)}
                  disabled={!item.available || item.bytes === 0}
                  onChange={() => toggle(item.id)}
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
                  <span className="clean-hint">{item.hint}</span>
                </span>
                <span className="clean-size">
                  {formatBytes(item.bytes)}
                  <span className="dim"> · {item.files} 个文件</span>
                </span>
              </label>
            ))}
          </section>
        ))}
        {!report && !error && !scanning && <p className="pane-empty">尚未扫描。</p>}
      </div>

      <footer className="clean-foot">
        <span className="dim">
          已选 {selectedItems.length} 项 · 可释放 {formatBytes(selectedBytes)}
          {report && ` · 扫描用时 ${report.elapsedMs} ms`}
        </span>
        <button
          type="button"
          className={`pane-btn${confirming ? " is-danger" : ""}`}
          onClick={() => void runClean()}
          disabled={cleaning || selectedItems.length === 0}
        >
          {cleaning ? <Loader2 size={14} className="spin" /> : <Trash2 size={14} />}
          {confirming ? "确认清理（不可恢复）" : "清理选中项"}
        </button>
      </footer>
    </div>
  );
}
