import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, FolderOpen, FileText, AlertTriangle, Clipboard } from "lucide-react";
import {
  listBackendLogFiles,
  readBackendLogTail,
  openLogDirectory,
  type LogFileInfo,
  type BackendLogTail,
} from "../../desktopBridge";
import { toastError, toastSuccess } from "../../hooks/useToast";

const TAIL_LINE_OPTIONS: number[] = [200, 500, 2000, 10000];

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

  useEffect(() => {
    void refresh();
    return () => abortRef.current?.abort();
  }, [refresh]);

  const onOpenDir = useCallback(async () => {
    try {
      const dir = await openLogDirectory();
      toastSuccess(`日志目录已打开：${dir}`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toastError(`无法打开日志目录：${msg}`);
    }
  }, []);

  const onCopyTail = useCallback(async () => {
    if (!tail) return;
    try {
      await navigator.clipboard.writeText(tail.lines.join("\n"));
      toastSuccess(`已复制日志（${tail.lines.length} 行）`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toastError(`复制失败：${msg}`);
    }
  }, [tail]);

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-base font-medium text-text-primary">诊断与日志</h2>
        <p className="text-sm text-text-muted mt-1">
          装版应用的后端日志默认写入 <code className="font-mono">hermix_home/logs/</code>，
          日志是后端排错的第一现场。流卡死时这里能看到比聊天 UI 更多的细节。
        </p>
      </div>

      {/* Path + actions */}
      <div className="rounded-lg border border-border-primary bg-bg-card p-4 space-y-3">
        <div className="flex items-center justify-between gap-3 flex-wrap">
          <div className="flex items-center gap-2 min-w-0">
            <FileText className="icon-md text-text-muted shrink-0" />
            <span className="text-[12px] text-text-muted font-mono truncate" title={info?.dir ?? ""}>
              {info?.dir ?? "（尚未生成日志目录）"}
            </span>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <label className="text-[11px] text-text-muted">显示尾部</label>
            <select
              value={tailLines}
              onChange={(e) => setTailLines(Number(e.target.value))}
              className="px-2 py-1 rounded-md bg-bg-tertiary text-[12px] text-text-primary border border-border-secondary"
            >
              {TAIL_LINE_OPTIONS.map((n) => (
                <option key={n} value={n}>
                  {n} 行
                </option>
              ))}
            </select>
            <button
              onClick={() => void refresh()}
              className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[12px] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
              title="重新读取"
            >
              <RefreshCw className="icon-sm" />
              刷新
            </button>
            <button
              onClick={onOpenDir}
              className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[12px] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
              title="在文件管理器中打开日志目录"
            >
              <FolderOpen className="icon-sm" />
              打开目录
            </button>
          </div>
        </div>

        {info && info.files.length > 0 && (
          <div className="text-[11px] text-text-muted">
            共 {info.files.length} 个文件，占用 {formatBytes(info.totalBytes)}
            {info.files[0] && (
              <>
                {" · "}
                最新：<span className="font-mono">{info.files[0].name}</span>（{formatBytes(info.files[0].sizeBytes)}）
              </>
            )}
          </div>
        )}
      </div>

      {error && (
        <div className="rounded-lg border border-border-secondary bg-bg-card p-3 flex items-start gap-2">
          <AlertTriangle className="icon-md text-text-muted mt-0.5 shrink-0" />
          <div className="text-[12px] text-text-secondary">
            读取日志失败：<span className="font-mono">{error}</span>
            <br />
            <span className="text-text-muted">
              装版首次启动可能尚未写日志（侧车还未初始化过），点「刷新」重试。
            </span>
          </div>
        </div>
      )}

      {tail && tail.lines.length > 0 && (
        <div className="rounded-lg border border-border-primary bg-bg-card">
          <div className="flex items-center justify-between px-3 py-2 border-b border-border-primary">
            <div className="text-[12px] text-text-secondary truncate font-mono">
              {tail.fileName}{" "}
              <span className="text-text-muted">（共 {tail.totalLines} 行，显示尾部 {tail.lines.length} 行）</span>
            </div>
            <button
              onClick={onCopyTail}
              className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[12px] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
            >
              <Clipboard className="icon-sm" />
              复制
            </button>
          </div>
          <pre
            className="p-3 text-[11px] leading-snug font-mono overflow-auto max-h-[60vh] whitespace-pre-wrap break-words text-text-secondary"
            style={{ background: "var(--color-bg-tertiary)" }}
          >
            {tail.lines.join("\n")}
          </pre>
        </div>
      )}

      {!loading && !error && tail && tail.lines.length === 0 && (
        <div className="text-[12px] text-text-muted">日志文件为空。</div>
      )}

      {!loading && !error && !tail && (
        <div className="text-[12px] text-text-muted">
          尚未发现日志文件。点击「刷新」重试，或执行一次完整对话后再查看。
        </div>
      )}
    </div>
  );
}
