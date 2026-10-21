// 进程页：进程列表 + 结束进程 + 顶部实时仪表。
//
// 与磁盘清理、文件索引同属"系统资源管理"，但它**不是**这次重构的重点：
// 布局与交互沿用原样，只是从 SystemPanel.tsx 拆出来，让三个页面各占一个
// 文件（原来的单文件已经 1500+ 行，再加两块必然失控）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Copy,
  Cpu,
  Loader2,
  MemoryStick,
  RefreshCw,
  Search,
  ShieldAlert,
  Trash2,
} from "lucide-react";
import type { ProcessRow, ProcessSnapshot } from "./types";
import { copyText, errorText, formatTime, formatUptime, useAnimatedNumber, useInterval } from "./format";

const MAX_ROWS = 400;
type SortKey = "cpu" | "memory" | "name" | "pid";

export function ProcessPane() {
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
  const maxMemory = useMemo(() => Math.max(1, ...rows.map((row) => row.memoryMb)), [rows]);

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

  // 索引助手是提权进程：「结束进程」对它无效（Access denied），
  // 但它能自己退出——这里给一条正经的停法。
  const helper = useMemo(
    () => rows.find((row) => row.name.toLowerCase() === "hermix-filesearch.exe"),
    [rows],
  );

  const stopHelper = useCallback(async () => {
    try {
      setNotice(await invoke<string>("viewer_filesearch_stop"));
    } catch (cause) {
      setNotice(errorText(cause));
    }
    void load();
  }, [load]);

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
      {helper && (
        <div className="pane-banner">
          <ShieldAlert size={14} aria-hidden="true" />
          <span>
            本地索引服务在运行（提权进程，PID {helper.pid}，占{" "}
            {helper.memoryMb >= 1024
              ? `${(helper.memoryMb / 1024).toFixed(2)} GB`
              : `${Math.round(helper.memoryMb)} MB`}
            ）。它需要管理员权限读 MFT，普通「结束进程」对它无效。
          </span>
          <button type="button" className="pane-btn" onClick={() => void stopHelper()}>
            优雅停止索引服务
          </button>
        </div>
      )}

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
                    <span
                      className="meter-fill is-cpu"
                      style={{ transform: `scaleX(${cpuPct / 100})` }}
                    />
                  </span>
                  <span className="meter-value">{row.cpu.toFixed(1)}%</span>
                </span>
                <span role="cell" className="num meter-cell">
                  <span className="meter" aria-hidden="true">
                    <span
                      className="meter-fill is-mem"
                      style={{ transform: `scaleX(${memPct / 100})` }}
                    />
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
          <div
            className={`stat-bar-fill ${tone(memPct)}`}
            style={{ transform: `scaleX(${memPct / 100})` }}
          />
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
