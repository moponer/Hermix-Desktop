import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, FolderOpen, FileText, AlertTriangle, Clipboard, ShieldCheck } from "lucide-react";
import {
  listBackendLogFiles,
  readBackendLogTail,
  openLogDirectory,
  type LogFileInfo,
  type BackendLogTail,
} from "../../desktopBridge";
import { fetchJSON } from "../../api/rest";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { Card, EmptyState, Skeleton } from "../ui";
import { COPIED, COPY_FAILED, failure } from "../strings";

const TAIL_LINE_OPTIONS: number[] = [200, 500, 2000, 10000];

/** `GET /api/pairing/audit` 的行（pairing.rs::AuditRow）。 */
interface AuditRow {
  ts: number;
  event: string;
  device_id: string | null;
  source_ip: string | null;
  decision: string | null;
}

/** 审计事件中文标签（Pairing/Auth/Grant/Revoke 四类）。 */
const AUDIT_EVENT_LABELS: Record<string, string> = {
  pairing: "配对",
  auth: "认证",
  grant: "授权查询",
  revoke: "撤销",
};

function formatAuditTime(ts: number): string {
  const d = new Date(ts < 1e12 ? ts * 1000 : ts);
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

/**
 * Diagnostics tab — surface the rolling log file written by
 * `hermix-telemetry::init_telemetry` so users (and support) can see
 * what the backend did in the moments before a stream misbehaved.
 *
 * The packaged Tauri app's stderr is dropped by `windows_subsystem =
 * "windows"`, so this tab is the **only** user-visible observability
 * surface once installed. Two actions:
 *   1. Live tail the most recent log file (refreshes on demand).
 *   2. Open the log directory in the OS file explorer (for users who
 *      want to zip and attach to a bug report).
 */
export function DiagnosticsTab() {
  const [info, setInfo] = useState<LogFileInfo | null>(null);
  const [tailLines, setTailLines] = useState<number>(500);
  const [tail, setTail] = useState<BackendLogTail | null>(null);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [audit, setAudit] = useState<AuditRow[] | null>(null);
  const [auditError, setAuditError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const refresh = useCallback(async () => {
    abortRef.current?.abort();
    const ac = new AbortController();
    abortRef.current = ac;
    setLoading(true);
    setError(null);
    try {
      const [files, log] = await Promise.all([
        listBackendLogFiles(),
        readBackendLogTail(tailLines),
      ]);
      if (ac.signal.aborted) return;
      setInfo(files);
      setTail(log);
    } catch (e) {
      if (ac.signal.aborted) return;
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setInfo(null);
      setTail(null);
    } finally {
      if (!ac.signal.aborted) setLoading(false);
    }
  }, [tailLines]);

  // 审计事件（best-effort：设备注册库未装配 503 时按无数据显示）。
  const refreshAudit = useCallback(async () => {
    setAuditError(null);
    try {
      const res = await fetchJSON<{ events: AuditRow[]; count: number }>(
        "/api/pairing/audit?limit=200",
      );
      setAudit(res.events);
    } catch (e) {
      setAudit(null);
      setAuditError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    void refreshAudit();
    return () => abortRef.current?.abort();
  }, [refresh, refreshAudit]);

  const onOpenDir = useCallback(async () => {
    try {
      const dir = await openLogDirectory();
      toastSuccess(`日志目录已打开：${dir}`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toastError(failure("无法打开日志目录", "请检查目录权限后重试。", msg));
    }
  }, []);

  const onCopyTail = useCallback(async () => {
    if (!tail) return;
    try {
      await navigator.clipboard.writeText(tail.lines.join("\n"));
      toastSuccess(`${COPIED}日志（${tail.lines.length} 行）`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toastError(failure(COPY_FAILED, "请手动选中日志后复制。", msg));
    }
  }, [tail]);

  return (
    <div className="space-y-4">
      {/* Path + actions */}
      <Card className="space-y-3">
        <div className="flex items-center justify-between gap-3 flex-wrap">
          <div className="flex items-center gap-2 min-w-0">
            <FileText className="icon-md text-text-muted shrink-0" />
            <span className="text-[length:var(--text-meta)] text-text-muted font-mono truncate" title={info?.dir ?? ""}>
              {info?.dir ?? "（尚未生成日志目录）"}
            </span>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <label className="text-[length:var(--text-meta)] text-text-muted">显示尾部</label>
            <select
              value={tailLines}
              onChange={(e) => setTailLines(Number(e.target.value))}
              className="px-2 py-1 rounded-[var(--radius-md)] bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary"
            >
              {TAIL_LINE_OPTIONS.map((n) => (
                <option key={n} value={n}>
                  {n} 行
                </option>
              ))}
            </select>
            <button
              onClick={() => void refresh()}
              className="h-7 px-2.5 rounded-[var(--radius-md)] flex items-center gap-1 bg-bg-tertiary text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
              title="重新读取"
            >
              <RefreshCw className="icon-sm" />
              刷新
            </button>
            <button
              onClick={onOpenDir}
              className="h-7 px-2.5 rounded-[var(--radius-md)] flex items-center gap-1 bg-bg-tertiary text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
              title="在文件管理器中打开日志目录"
            >
              <FolderOpen className="icon-sm" />
              打开目录
            </button>
          </div>
        </div>

        {info && info.files.length > 0 && (
          <div className="text-[length:var(--text-meta)] text-text-muted">
            共 {info.files.length} 个文件，占用 {formatBytes(info.totalBytes)}
            {info.files[0] && (
              <>
                {" · "}
                最新：<span className="font-mono">{info.files[0].name}</span>（{formatBytes(info.files[0].sizeBytes)}）
              </>
            )}
          </div>
        )}
      </Card>

      {error && (
        <div className="rounded-[var(--radius-lg)] bg-bg-card p-3 flex items-start gap-2 shadow-[var(--shadow-paper)]">
          <AlertTriangle className="icon-md text-text-muted mt-0.5 shrink-0" />
          <div className="text-[length:var(--text-meta)] text-text-secondary">
            读取日志失败：<span className="font-mono">{error}</span>
            <br />
            <span className="text-text-muted">
              装版首次启动可能尚未写日志（侧车还未初始化过），点「刷新」重试。
            </span>
          </div>
        </div>
      )}

      {tail && tail.lines.length > 0 && (
        <div className="rounded-[var(--radius-lg)] bg-bg-card shadow-[var(--shadow-paper)]">
          <div className="flex items-center justify-between bg-bg-tertiary/40 px-3 py-2">
            <div className="text-[length:var(--text-meta)] text-text-secondary truncate font-mono">
              {tail.fileName}{" "}
              <span className="text-text-muted">（共 {tail.totalLines} 行，显示尾部 {tail.lines.length} 行）</span>
            </div>
            <button
              onClick={onCopyTail}
              className="h-7 px-2.5 rounded-[var(--radius-md)] flex items-center gap-1 bg-bg-tertiary text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
            >
              <Clipboard className="icon-sm" />
              复制
            </button>
          </div>
          <pre
            className="p-3 text-[length:var(--text-meta)] leading-snug font-mono overflow-auto max-h-[60vh] whitespace-pre-wrap break-words text-text-secondary"
            style={{ background: "var(--color-bg-tertiary)" }}
          >
            {tail.lines.join("\n")}
          </pre>
        </div>
      )}

      {!loading && !error && tail && tail.lines.length === 0 && (
        <div className="text-[length:var(--text-meta)] text-text-muted">日志文件为空。</div>
      )}

      {!loading && !error && !tail && (
        <div className="text-[length:var(--text-meta)] text-text-muted">
          尚未发现日志文件。点击「刷新」重试，或执行一次完整对话后再查看。
        </div>
      )}

      {/* 安全审计事件（GET /api/pairing/audit，时间倒序） */}
      <div className="rounded-[var(--radius-lg)] bg-bg-card shadow-[var(--shadow-paper)]">
        <div className="flex items-center justify-between bg-bg-tertiary/40 px-3 py-2">
          <div className="flex items-center gap-2">
            <ShieldCheck className="icon-md text-text-muted" />
            <span className="text-[length:var(--text-meta)] text-text-secondary">安全审计事件</span>
            {audit && (
              <span className="text-[length:var(--text-meta)] text-text-muted">（最近 {audit.length} 条）</span>
            )}
          </div>
          <button
            onClick={() => void refreshAudit()}
            className="h-7 px-2.5 rounded-[var(--radius-md)] flex items-center gap-1 bg-bg-tertiary text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
            title="重新读取审计事件"
          >
            <RefreshCw className="icon-sm" />
            刷新
          </button>
        </div>

        {auditError ? (
          <div className="px-3 py-3 text-[length:var(--text-meta)] text-text-muted">
            <p>
              审计不可用：{auditError}。设备注册库未装配时没有审计数据，请稍后重试。
            </p>
            <button
              onClick={() => void refreshAudit()}
              className="mt-2 h-7 px-2.5 rounded-[var(--radius-md)] flex items-center gap-1 bg-bg-tertiary text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
            >
              <RefreshCw className="icon-sm" />
              重试
            </button>
          </div>
        ) : audit === null ? (
          /* 加载中不能显示成空态：audit 为 null 时既没有数据也还没读完。 */
          <div className="space-y-2 px-3 py-3" aria-busy="true">
            <Skeleton className="h-4 w-full" />
            <Skeleton className="h-4 w-11/12" />
            <Skeleton className="h-4 w-10/12" />
          </div>
        ) : audit.length === 0 ? (
          <EmptyState
            variant="plain"
            size="sm"
            icon={<ShieldCheck className="size-7" />}
            title="暂无审计事件"
            description="设备配对、认证与授权记录会出现在这里。"
            action={
              <button
                onClick={() => void refreshAudit()}
                className="h-7 px-2.5 rounded-[var(--radius-md)] flex items-center gap-1 bg-bg-tertiary text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary"
              >
                <RefreshCw className="icon-sm" />
                刷新
              </button>
            }
          />
        ) : (
          <div className="max-h-[40vh] overflow-auto">
            <table className="w-full text-[length:var(--text-meta)]">
              {/* 表头吸顶：外层是 max-h-[40vh] overflow-auto，不吸顶滚动后表头立即消失。 */}
              <thead className="sticky top-0 z-10 bg-bg-card">
                <tr className="text-text-muted text-left bg-bg-card">
                  <th className="px-3 py-1.5 font-normal">时间</th>
                  <th className="px-3 py-1.5 font-normal">事件</th>
                  <th className="px-3 py-1.5 font-normal">设备</th>
                  <th className="px-3 py-1.5 font-normal">来源 IP</th>
                  <th className="px-3 py-1.5 font-normal">结果</th>
                </tr>
              </thead>
              <tbody>
                {audit.map((row, i) => (
                  <tr key={i} className={`text-text-secondary ${i % 2 === 1 ? "bg-bg-tertiary/40" : ""}`}>
                    <td className="px-3 py-1.5 font-mono whitespace-nowrap">{formatAuditTime(row.ts)}</td>
                    <td className="px-3 py-1.5">{AUDIT_EVENT_LABELS[row.event] ?? row.event}</td>
                    <td className="px-3 py-1.5 font-mono truncate max-w-[160px]" title={row.device_id ?? ""}>
                      {row.device_id ? `${row.device_id.slice(0, 8)}…` : "—"}
                    </td>
                    <td className="px-3 py-1.5 font-mono">{row.source_ip ?? "—"}</td>
                    <td className="px-3 py-1.5">{row.decision ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
