// 系统资源管理弹窗：进程 + 磁盘清理 + 关于。
//
// 设计取舍：
// - **弹窗而不是新窗口**：菜单点开就在当前窗口浮出，Esc / 点遮罩关闭。
// - 数据是**真值**：CPU/内存/磁盘占用全部来自 Rust 侧，进度条来自
//   `cleanup-progress` 事件（每完成一条规则推一次），不是假动效。
// - 动效克制：只做"数值变化"与"进度推进"的过渡，不做无意义的粒子动画，
//   免得刷新时界面一直在抖。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Copy,
  Cpu,
  HardDrive,
  Info,
  Keyboard,
  Loader2,
  MemoryStick,
  Package,
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
    cpuUsage: number;
    uptimeSecs: number;
    hostName: string;
    osVersion: string;
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

interface CleanupProgress {
  phase: "scan" | "clean";
  ruleId: string;
  title: string;
  done: number;
  total: number;
  bytes?: number;
  files?: number;
  deleted?: number;
  freedBytes?: number;
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
  return new Date(unixSeconds * 1000).toLocaleString();
}

function formatUptime(seconds: number): string {
  if (!seconds) return "—";
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return `${minutes} 分钟`;
}

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/** 数值平滑过渡：大数字跳变（1.2GB → 3.4GB）会显得很"炸"，插值一下。 */
function useAnimatedNumber(target: number, durationMs = 420): number {
  const [value, setValue] = useState(target);
  const fromRef = useRef(target);
  const startRef = useRef(0);
  const frameRef = useRef(0);

  useEffect(() => {
    cancelAnimationFrame(frameRef.current);
    fromRef.current = value;
    startRef.current = performance.now();
    const tick = (now: number) => {
      const t = Math.min(1, (now - startRef.current) / durationMs);
      // easeOutCubic：起步快、收尾稳，读数不会"冲过头"。
      const eased = 1 - Math.pow(1 - t, 3);
      setValue(fromRef.current + (target - fromRef.current) * eased);
      if (t < 1) frameRef.current = requestAnimationFrame(tick);
    };
    frameRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frameRef.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, durationMs]);

  return value;
}

/** 固定间隔刷新：只在面板可见时跑，避免后台白烧 CPU（每次快照要采两次样）。 */
function useInterval(callback: () => void, delayMs: number | null) {
  const saved = useRef(callback);
  saved.current = callback;
  useEffect(() => {
    if (delayMs === null) return;
    const timer = window.setInterval(() => saved.current(), delayMs);
    return () => window.clearInterval(timer);
  }, [delayMs]);
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // WebView 里剪贴板权限缺失时静默失败：不为此弹一个错误框。
  }
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
              <Cpu size={13} aria-hidden="true" />
              进程
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "cleanup"}
              className={`syspanel-tab${tab === "cleanup" ? " is-active" : ""}`}
              onClick={() => setTab("cleanup")}
            >
              <Trash2 size={13} aria-hidden="true" />
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
type SortKey = "cpu" | "memory" | "name" | "pid";

function ProcessPane() {
  const [snapshot, setSnapshot] = useState<ProcessSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [selectedPid, setSelectedPid] = useState<number | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmPid, setConfirmPid] = useState<number | null>(null);
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: "cpu", desc: true });
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

  useEffect(() => {
    if (confirmPid === null) return;
    const timer = window.setTimeout(() => setConfirmPid(null), 4000);
    return () => window.clearTimeout(timer);
  }, [confirmPid]);

  const rows = useMemo(() => {
    const list = snapshot?.processes ?? [];
    const needle = query.trim().toLowerCase();
    const filtered = needle
      ? list.filter(
          (row) =>
            row.name.toLowerCase().includes(needle) ||
            (row.exe ?? "").toLowerCase().includes(needle) ||
            row.cmd.toLowerCase().includes(needle) ||
            String(row.pid) === needle,
        )
      : list;

    const sorted = [...filtered].sort((a, b) => {
      const delta =
        sort.key === "name"
          ? a.name.toLowerCase().localeCompare(b.name.toLowerCase())
          : sort.key === "pid"
            ? a.pid - b.pid
            : sort.key === "memory"
              ? a.memoryMb - b.memoryMb
              : a.cpu - b.cpu;
      return sort.desc ? -delta : delta;
    });
    return sorted.slice(0, MAX_ROWS);
  }, [snapshot, query, sort]);

  const selected = useMemo(
    () => (snapshot?.processes ?? []).find((row) => row.pid === selectedPid) ?? null,
    [snapshot, selectedPid],
  );

  // 行内条形图的量纲：CPU 以 100% 为满格（单核口径，超过 100 按 100 截），
  // 内存按"当前列表里最大那行"归一，这样同一屏的高低差才看得出来。
  const maxMemory = useMemo(
    () => Math.max(1, ...rows.map((row) => row.memoryMb)),
    [rows],
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
        setNotice(await invoke<string>("sysmon_kill_process", { pid: row.pid, force: true }));
      } catch (cause) {
        setNotice(errorText(cause));
      }
      void load();
    },
    [confirmPid, load],
  );

  const toggleSort = (key: SortKey) => {
    setSort((prev) => (prev.key === key ? { key, desc: !prev.desc } : { key, desc: key !== "name" }));
  };

  const sortMark = (key: SortKey) => (sort.key === key ? (sort.desc ? "↓" : "↑") : "");

  return (
    <div className="pane">
      {snapshot && <StatHeader overview={snapshot.overview} />}

      <div className="pane-bar">
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
          <button type="button" className="th" role="columnheader" onClick={() => toggleSort("name")}>
            名称 <span className="th-mark">{sortMark("name")}</span>
          </button>
          <button type="button" className="th" role="columnheader" onClick={() => toggleSort("pid")}>
            PID <span className="th-mark">{sortMark("pid")}</span>
          </button>
          <button
            type="button"
            className="th is-num"
            role="columnheader"
            onClick={() => toggleSort("cpu")}
          >
            CPU <span className="th-mark">{sortMark("cpu")}</span>
          </button>
          <button
            type="button"
            className="th is-num"
            role="columnheader"
            onClick={() => toggleSort("memory")}
          >
            内存 <span className="th-mark">{sortMark("memory")}</span>
          </button>
          <span role="columnheader" className="th is-static">
            用户
          </span>
          <span role="columnheader" className="th is-static">
            路径
          </span>
        </div>
        <div className="proc-scroll">
          {rows.map((row) => {
            const cpuPct = Math.min(100, Math.max(0, row.cpu));
            const memPct = Math.min(100, (row.memoryMb / maxMemory) * 100);
            return (
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
                <span role="cell" className="mono dim">
                  {row.pid}
                </span>
                <span role="cell" className="num meter-cell">
                  <span className="meter" aria-hidden="true">
                    <span className="meter-fill is-cpu" style={{ width: `${cpuPct}%` }} />
                  </span>
                  <span className="meter-value">{row.cpu.toFixed(1)}%</span>
                </span>
                <span role="cell" className="num meter-cell">
                  <span className="meter" aria-hidden="true">
                    <span className="meter-fill is-mem" style={{ width: `${memPct}%` }} />
                  </span>
                  <span className="meter-value">
                    {row.memoryMb >= 1024
                      ? `${(row.memoryMb / 1024).toFixed(2)}G`
                      : `${Math.round(row.memoryMb)}M`}
                  </span>
                </span>
                <span role="cell" className="dim ellipsis">
                  {row.user ?? "—"}
                </span>
                <span role="cell" className="dim ellipsis" title={row.exe ?? ""}>
                  {row.exe ?? "—"}
                </span>
              </button>
            );
          })}
          {rows.length === 0 && !error && <p className="pane-empty">没有匹配的进程。</p>}
        </div>
      </div>

      {selected && (
        <div className="proc-detail">
          <div className="proc-detail-head">
            <span className="proc-detail-name">{selected.name}</span>
            <span className="chip">PID {selected.pid}</span>
            {selected.parentPid !== null && <span className="chip">父进程 {selected.parentPid}</span>}
            <span className="chip">{selected.status}</span>
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
              {selected.exe && (
                <button
                  type="button"
                  className="inline-btn"
                  onClick={() => void copyText(selected.exe ?? "")}
                  title="复制路径"
                >
                  <Copy size={11} />
                </button>
              )}
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
        <p className="pane-hint">仅显示前 {MAX_ROWS} 条，用上方过滤框缩小范围。</p>
      )}
    </div>
  );
}

/** 顶部实时仪表：CPU 环 + 内存条 + 主机信息。 */
function StatHeader({ overview }: { overview: ProcessSnapshot["overview"] }) {
  const cpu = Math.min(100, Math.max(0, overview.cpuUsage));
  const animatedCpu = useAnimatedNumber(cpu);
  const memPct =
    overview.totalMemoryMb > 0 ? (overview.usedMemoryMb / overview.totalMemoryMb) * 100 : 0;
  const usedGb = overview.usedMemoryMb / 1024;
  const totalGb = overview.totalMemoryMb / 1024;

  // 颜色只表达"是否吃紧"，不做装饰：70% 以上转黄，85% 以上转红。
  const tone = (pct: number) => (pct >= 85 ? "is-high" : pct >= 70 ? "is-warn" : "");

  return (
    <div className="statbar">
      <div className={`stat-ring ${tone(cpu)}`}>
        <div
          className="stat-ring-gauge"
          style={{ ["--p" as string]: `${animatedCpu.toFixed(1)}%` }}
        />
        <div className="stat-ring-label">
          <Cpu size={12} aria-hidden="true" />
          <strong>{cpu.toFixed(0)}%</strong>
          <span>CPU</span>
        </div>
      </div>

      <div className="stat-mem">
        <div className="stat-mem-head">
          <MemoryStick size={13} aria-hidden="true" />
          <span>
            内存 {usedGb.toFixed(1)} / {totalGb.toFixed(1)} GB
          </span>
          <span className="dim">{memPct.toFixed(0)}%</span>
        </div>
        <div className="stat-bar">
          <div className={`stat-bar-fill ${tone(memPct)}`} style={{ width: `${memPct}%` }} />
        </div>
      </div>

      <div className="stat-meta">
        <span>{overview.processCount} 个进程</span>
        <span>{overview.cpuCount} 逻辑核心</span>
        <span>已运行 {formatUptime(overview.uptimeSecs)}</span>
        {overview.hostName && <span className="dim">{overview.hostName}</span>}
      </div>
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
  const [progress, setProgress] = useState<CleanupProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CleanResult | null>(null);
  const [confirming, setConfirming] = useState(false);

  useEffect(() => {
    void invoke<DiskRow[]>("sysmon_disks")
      .then(setDisks)
      .catch((cause) => setError(errorText(cause)));
  }, []);

  // 进度事件：Rust 每完成一条规则推一次，用来画确定进度条。
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

  const scan = useCallback(async (target: string | null) => {
    setScanning(true);
    setError(null);
    setResult(null);
    setProgress(null);
    try {
      const next = await invoke<ScanReport>("cleanup_scan", { drive: target });
      setReport(next);
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
      setProgress(null);
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
  const animatedSelected = useAnimatedNumber(selectedBytes);
  const totalBytes = Math.max(1, report?.totalBytes ?? 0);

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
    setProgress(null);
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
      setProgress(null);
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

  const busy = scanning || cleaning;
  const pct = progress ? Math.min(100, (progress.done / Math.max(1, progress.total)) * 100) : 0;

  return (
    <div className="pane">
      <div className="drive-cards" role="group" aria-label="选择盘符">
        <button
          type="button"
          className={`drive-card${drive === null ? " is-active" : ""}`}
          onClick={() => setDrive(null)}
        >
          <span className="drive-letter">ALL</span>
          <span className="drive-meta">全部磁盘</span>
        </button>
        {disks.map((disk) => {
          const usedPct = disk.totalBytes ? ((disk.totalBytes - disk.freeBytes) / disk.totalBytes) * 100 : 0;
          return (
            <button
              type="button"
              key={disk.drive}
              className={`drive-card${drive === disk.drive ? " is-active" : ""}`}
              onClick={() => setDrive(disk.drive)}
              title={`${disk.mount} · 可用 ${formatBytes(disk.freeBytes)} / ${formatBytes(disk.totalBytes)}`}
            >
              <span className="drive-letter">
                <HardDrive size={12} aria-hidden="true" />
                {disk.drive}
              </span>
              <span className="drive-bar">
                <span
                  className={`drive-bar-fill${usedPct >= 85 ? " is-high" : usedPct >= 70 ? " is-warn" : ""}`}
                  style={{ width: `${usedPct}%` }}
                />
              </span>
              <span className="drive-meta">
                {formatBytes(disk.freeBytes)} 可用
              </span>
            </button>
          );
        })}
      </div>

      <div className="pane-bar">
        <span className="pane-stats">
          {report
            ? `${report.items.filter((i) => i.bytes > 0).length} 类垃圾 · 共 ${formatBytes(report.totalBytes)} · ${report.totalFiles} 个文件`
            : "尚未扫描"}
        </span>
        <button
          type="button"
          className="pane-btn"
          onClick={() => void scan(drive)}
          disabled={busy}
          title="重新扫描"
        >
          {scanning ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
          {scanning ? "扫描中…" : "重新扫描"}
        </button>
      </div>

      {(scanning || cleaning) && (
        <div className="progress-wrap">
          <div className={`progress ${progress ? "is-determinate" : "is-indeterminate"}`}>
            <div className="progress-fill" style={progress ? { width: `${pct}%` } : undefined} />
          </div>
          <span className="progress-label">
            {progress
              ? `${cleaning ? "正在清理" : "正在扫描"}：${progress.title}（${progress.done}/${progress.total}）`
              : cleaning
                ? "正在清理…"
                : "正在扫描…"}
          </span>
        </div>
      )}

      {error && <p className="pane-error">{error}</p>}
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
        {groups.map(([group, groupItems]) => (
          <section className="clean-group" key={group}>
            <h4 className="clean-group-title">{group}</h4>
            {groupItems.map((item) => {
              const share = Math.min(100, (item.bytes / totalBytes) * 100);
              return (
                <label
                  key={item.id}
                  className={`clean-item${item.available && item.bytes > 0 ? "" : " is-disabled"}${
                    selected.has(item.id) ? " is-checked" : ""
                  }`}
                  title={item.roots.join("\n")}
                >
                  <input
                    type="checkbox"
                    checked={selected.has(item.id)}
                    disabled={!item.available || item.bytes === 0 || busy}
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
                    <span className="clean-bar" aria-hidden="true">
                      <span className="clean-bar-fill" style={{ width: `${share}%` }} />
                    </span>
                  </span>
                  <span className="clean-size">
                    {formatBytes(item.bytes)}
                    <span className="dim"> · {item.files} 个</span>
                  </span>
                </label>
              );
            })}
          </section>
        ))}
        {!report && !error && !scanning && <p className="pane-empty">尚未扫描。</p>}
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
    </div>
  );
}

// ── 关于 ──────────────────────────────────────────────────────────────

const ABOUT_FEATURES = [
  "资源管理器：多标签、目录树、拖放、右键菜单",
  "多格式预览：文本 / 图片 / PDF / Office / 视频",
  "AI 右栏：选中内容直接提问",
  "系统资源管理：进程查看与结束、规则驱动磁盘清理",
];

const ABOUT_SHORTCUTS = [
  ["F5", "刷新当前目录"],
  ["Ctrl+Shift+Esc", "打开系统资源管理"],
  ["Esc", "关闭弹窗 / 菜单"],
];

export function AboutDialog({ onClose }: { onClose: () => void }) {
  const [version, setVersion] = useState("");

  useEffect(() => {
    void getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

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
      <div className="about-card" role="dialog" aria-modal="true" aria-label="关于">
        <header className="about-head">
          <span className="about-mark">
            <Activity size={18} aria-hidden="true" />
          </span>
          <div>
            <h3>Hermix 文件查看器</h3>
            <p className="dim">{version ? `版本 ${version}` : "独立文件浏览器"}</p>
          </div>
          <button type="button" className="syspanel-close" onClick={onClose} aria-label="关闭">
            <X size={16} />
          </button>
        </header>

        <section className="about-section">
          <h4>
            <Package size={13} aria-hidden="true" />
            能力
          </h4>
          <ul>
            {ABOUT_FEATURES.map((feature) => (
              <li key={feature}>{feature}</li>
            ))}
          </ul>
        </section>

        <section className="about-section">
          <h4>
            <Info size={13} aria-hidden="true" />
            技术
          </h4>
          <p className="dim">
            Rust + Tauri 2 内核，React 19 + Vite 前端；预览引擎与桌面端共用
            <code>@hermix/preview</code>。系统资源管理用内核快照枚举进程，磁盘清理为规则驱动
            （白名单 + 禁区校验 + 二次确认）。
          </p>
        </section>

        <section className="about-section">
          <h4>
            <Keyboard size={13} aria-hidden="true" />
            快捷键
          </h4>
          <dl className="about-keys">
            {ABOUT_SHORTCUTS.map(([key, text]) => (
              <div key={key}>
                <dt>
                  <kbd>{key}</kbd>
                </dt>
                <dd>{text}</dd>
              </div>
            ))}
          </dl>
        </section>

        <footer className="about-foot">
          <span className="dim">MIT License · 本地运行，不上传任何数据</span>
          <button type="button" className="pane-btn" onClick={onClose}>
            关闭
          </button>
        </footer>
      </div>
    </div>
  );
}
