/**
 * UpdatePanel — 更新面板（portal 弹层）。
 *
 * 三态：
 *   forced  → 全屏阻断层，无关闭按钮（min_version 命中，决策笔记 §强制/可选）
 *   optional/手动打开 → 普通弹窗：版本 + Markdown 更新日志 + 立即更新
 *   进行中 → 进度条（下载字节流 → 校验/解压不确定态 → 重启提示）
 *
 * 更新日志用 MarkdownBody 渲染（manifest.notes 是 Markdown，发布脚本从
 * CHANGELOG.md 顶部提取）。
 */

import { useEffect } from "react";
import { Download, RefreshCw, AlertTriangle, CheckCircle2 } from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";
import { Button, Modal } from "./ui";
import type { Updater } from "../hooks/useUpdater";

function fmtBytes(n: number): string {
  if (!n) return "0 MB";
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function UpdatePanel({ updater }: { updater: Updater }) {
  const { status, panelOpen, setPanelOpen, progress, phase, error, checkFailed, startUpdate } = updater;

  // forced：ESC 也关不掉；optional：ESC 关闭。
  useEffect(() => {
    if (!panelOpen) return;
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape" && status?.kind !== "forced") setPanelOpen(false);
    };
    window.addEventListener("keydown", onEsc);
    return () => window.removeEventListener("keydown", onEsc);
  }, [panelOpen, status?.kind, setPanelOpen]);

  if (!panelOpen) return null;

  const forced = status?.kind === "forced";
  const isLatest = status?.kind === "none";
  const busy = phase !== "idle";
  const pct = progress && progress.total > 0 ? Math.min(100, Math.round((progress.received / progress.total) * 100)) : null;

  let phaseLabel = "";
  if (phase === "downloading") {
    const parts: string[] = ["正在下载"];
    if (pct !== null) parts.push(`${pct}%`);
    if (progress) {
      const bytes = progress.total > 0
        ? `${fmtBytes(progress.received)} / ${fmtBytes(progress.total)}`
        : fmtBytes(progress.received);
      parts.push(`（${bytes}）`);
    }
    phaseLabel = parts.join(" ");
  } else if (phase === "installing") {
    phaseLabel = "正在校验并解压更新包…";
  } else if (phase === "restarting") {
    phaseLabel = "即将重启应用以完成更新…";
  }

  const title = forced
    ? "必须更新后才能继续使用"
    : isLatest
      ? "已是最新"
      : status
        ? `有新版本 ${status.latest_version}`
        : "软件更新";

  return (
    <Modal
      open
      onClose={() => setPanelOpen(false)}
      width={forced ? "wide" : "base"}
      zIndex={10000}
      scrim
      /* 强制更新不可关；可选更新在下载中也不该被遮罩点击打断。 */
      closeOnBackdrop={!forced && !busy}
      closeOnEscape={!forced && !busy}
      title={
        <span className="flex items-center gap-2">
          {forced ? (
            <AlertTriangle className="icon-md text-[var(--color-warning)] shrink-0" />
          ) : (
            <Download className="icon-md text-accent shrink-0" />
          )}
          {title}
        </span>
      }
      bodyClassName="px-5 pb-4 overflow-y-auto min-h-0 flex-1"
    >
      {status ? (
        isLatest ? (
          <div className="text-sm text-text-secondary">
            当前版本 {status.current_version} 已是最新发布版。
          </div>
        ) : (
          <>
            <div className="text-[length:var(--text-meta)] text-text-faint mb-3">
              当前版本 {status.current_version}
              {status.pub_date ? ` · 发布于 ${status.pub_date.slice(0, 10)}` : ""}
            </div>
            {status.notes ? (
              /* 无描边：更新说明是紧跟标题的第二段文字，用间距而不是线分界。 */
              <div className="mt-3 text-sm text-text-secondary leading-[1.75] [&_li]:marker:text-text-faint">
                <MarkdownBody content={status.notes} />
              </div>
            ) : null}
          </>
        )
      ) : checkFailed ? (
        <div className="text-sm text-text-secondary">
          检查更新失败，具体原因见下方提示。
        </div>
      ) : (
        <div className="text-sm text-text-secondary">正在检查更新…</div>
      )}

      {/* 进度区 */}
      {busy && (
        <div className="mt-4">
          {phase === "downloading" && pct !== null ? (
            <div className="h-1.5 rounded-full bg-bg-tertiary overflow-hidden">
              <div className="h-full bg-accent transition-all" style={{ width: `${pct}%` }} />
            </div>
          ) : (
            <RefreshCw className="icon-sm animate-spin text-accent" />
          )}
          <div className="text-[length:var(--text-meta)] text-text-muted mt-2">{phaseLabel}</div>
        </div>
      )}

      {error && (
        <div className="mt-3 flex items-start gap-1.5 text-sm text-text-error">
          <AlertTriangle className="icon-sm shrink-0 mt-0.5" />
          <span className="min-w-0 break-words">{error}</span>
        </div>
      )}
      footer={
        <>
          {!forced && !busy && !isLatest && (
            <Button variant="secondary" size="base" onClick={() => setPanelOpen(false)}>
              以后再说
            </Button>
          )}
          {isLatest && !busy ? (
            // 已是最新：不存在可执行的更新动作，给关闭而不是可点的下载钮。
            <Button variant="secondary" size="base" onClick={() => setPanelOpen(false)}>
              <CheckCircle2 className="icon-sm text-text-muted" /> 关闭
            </Button>
          ) : (
            <Button
              variant="accent"
              size="base"
              disabled={!status || busy || isLatest}
              onClick={() => void startUpdate()}
            >
              {busy ? (
                <>
                  <RefreshCw className="icon-sm animate-spin" /> 更新中…
                </>
              ) : (
                <>
                  <Download className="icon-sm" /> 立即更新
                </>
              )}
            </Button>
          )}
        </>
      }
    </Modal>
  );
}
