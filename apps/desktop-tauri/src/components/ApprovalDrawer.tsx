// components/ApprovalDrawer.tsx
//
// 审批抽屉 —— 从输入框上方弹出的"待决定"面板（ZCode 式权限面板，2026-09-30 重做）。
//
// 设计要点（对照 ZCode「需要权限」面板）：
// - **命令原样展示**：等宽框里逐字给出将要执行的内容——"它到底要跑什么"
//   是信任的核心。此前只给路径/摘要，用户从没看过命令本身。
// - **5 选项 + 一句话作用域说明**：允许这一次 / 始终允许此命令（进允许列表）/
//   完全访问（切到 full 权限档）/ 拒绝 / 告诉模型接下来该怎么做（note 回喂
//   模型，让它换方案而不是在同一个被拒调用上打转）。
// - 数字键 1–5 / Esc / ⏎ 键盘导航，底部一行提示。⏎ 只在焦点**不在**
//   可激活元素上时才等同「允许这一次」——焦点在某个选项按钮上时交还给
//   原生激活，否则会出现「按一次 Enter 既拒绝又批准」。
//
// 职责边界：
// - 抽屉只处理**待决定**的审批；已决定的历史卡片仍由 ProcessTimeline
//   内联渲染（那是记录，属于流）。
// - 批量审批（approval_request_batch）与单个审批共用抽屉，一次显示
//   最老的一项（agent 正阻塞在哪一项，就显示哪一项），右上角计数。
// - 快捷键仅在焦点不在输入类元素时生效 —— 输入框里打字绝不能触发选项，
//   宁可让用户多点一下。

import { useEffect, useRef, useState } from "react";
import { AlertTriangle, ChevronDown, ChevronUp, Send, ShieldCheck } from "lucide-react";
import type { ApprovalBatchMessageMeta, ApprovalMessageMeta } from "../types";
import { describeToolArgs, toolVerb } from "./bubbleUtils";

export interface PendingApprovals {
  singles: ApprovalMessageMeta[];
  batches: ApprovalBatchMessageMeta[];
}

interface ApprovalDrawerProps {
  pending: PendingApprovals;
  onApprove: (requestId: string, remember?: boolean) => void;
  /** `note`：拒绝时附的说明，原样进 LLM 可见的拒绝理由（DeniedWithFeedback）。 */
  onDeny: (requestId: string, note?: string) => void;
  onApproveBatch: (batchId: string, remember?: boolean) => void;
  onDenyBatch: (batchId: string, note?: string) => void;
  /** 切到「完全访问」权限档（后续工具调用自动放行；由 ChatView 落到
   *  全局 permissionLevel，随下一条消息的请求带上 `permission: "full"`）。
   *  未提供时隐藏第 3 项。 */
  onFullAccess?: () => void;
}

type Risk = "dangerous" | "caution" | "safe";
const RISK_LABEL: Record<Risk, string> = {
  dangerous: "高风险", caution: "中风险", safe: "低风险",
};

function riskKey(level?: string): Risk {
  const k = (level ?? "caution").toLowerCase();
  return k === "dangerous" || k === "caution" || k === "safe" ? (k as Risk) : "caution";
}

function totalPending(p: PendingApprovals): number {
  return p.singles.length + p.batches.length;
}

export function hasDrawerContent(p: PendingApprovals | null): boolean {
  return !!p && totalPending(p) > 0;
}

/** 工具参数 → 等宽框里逐字展示的一段文本（可含换行）。
 *  取不出有信息量的内容时返回 null，调用方回退到紧凑 JSON。 */
function commandText(tool: string, args: unknown): string | null {
  const a = args as Record<string, unknown> | null;
  if (!a || typeof args !== "object") return null;
  const str = (v: unknown): string | null =>
    typeof v === "string" && v.trim() ? v : null;
  const shell = /terminal|shell|command|exec|run|system|powershell|bash|cmd/i.test(tool);
  for (const k of ["command", "cmd", "script", "shell", "code", "query"]) {
    const v = str(a[k]);
    if (v) return (shell ? "$ " : "") + v;
  }
  const url = str(a.url);
  if (url) return url;
  const path =
    str(a.path) ?? str(a.file) ?? str(a.filePath) ?? str(a.target);
  if (path) {
    const t =
      str(a.content) ?? str(a.new_string) ?? str(a.replacement) ?? str(a.text);
    const body = t
      ? "→ " + (t.length > 500 ? t.slice(0, 500) + " …" : t)
      : "";
    return path + (body ? "\n" + body : "");
  }
  const task = str(a.task) ?? str(a.question) ?? str(a.prompt) ?? str(a.text);
  if (task) return task.length > 500 ? task.slice(0, 500) + " …" : task;
  return null;
}

function compactArgs(args: unknown): string | null {
  const raw = JSON.stringify(args ?? {});
  const trimmed = raw.trim();
  if (trimmed === "{}" || trimmed === "null") return null;
  return trimmed;
}

export function ApprovalDrawer({
  pending,
  onApprove,
  onDeny,
  onApproveBatch,
  onDenyBatch,
  onFullAccess,
}: ApprovalDrawerProps) {
  const total = totalPending(pending);
  const single = pending.singles[0] ?? null;
  const batch = !single ? (pending.batches[0] ?? null) : null;

  const [showDiff, setShowDiff] = useState(false);
  const [feedback, setFeedback] = useState("");
  const feedbackRef = useRef<HTMLInputElement>(null);

  // 换一项待审请求时清掉上一项的反馈草稿。
  useEffect(() => {
    setFeedback("");
    setShowDiff(false);
  }, [single?.requestId, batch?.batchId]);

  type Action = "once" | "always" | "full" | "deny" | "note";
  const act = (a: Action) => {
    if (single) {
      if (a === "once") onApprove(single.requestId, false);
      else if (a === "always") onApprove(single.requestId, true);
      else if (a === "full") {
        onFullAccess?.();
        onApprove(single.requestId, false);
      } else if (a === "deny") onDeny(single.requestId);
      else onDeny(single.requestId, feedback.trim() || undefined);
    } else if (batch) {
      if (a === "once") onApproveBatch(batch.batchId, false);
      else if (a === "always") onApproveBatch(batch.batchId, true);
      else if (a === "full") {
        onFullAccess?.();
        onApproveBatch(batch.batchId, false);
      } else if (a === "deny") onDenyBatch(batch.batchId);
      else onDenyBatch(batch.batchId, feedback.trim() || undefined);
    }
  };

  // 快捷键：1–5 选项 · Esc 拒绝 · ⏎ 允许这一次。焦点在输入类元素里时
  // 一律不生效（输入框打 "1" 绝不能变成"允许"）—— 反馈输入框内的
  // ⏎ 由 input 自己的 keydown 处理。
  useEffect(() => {
    if (total === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t?.isContentEditable) return;
      const k = e.key;
      if (k === "1") { e.preventDefault(); act("once"); }
      else if (k === "2") { e.preventDefault(); act("always"); }
      else if (k === "3" && onFullAccess) { e.preventDefault(); act("full"); }
      else if (k === "4" || k === "Escape") { e.preventDefault(); act("deny"); }
      else if (k === "5") { e.preventDefault(); feedbackRef.current?.focus(); }
      else if (k === "Enter") {
        // 焦点在可激活元素（按钮/链接/摘要）上时，把 ⏎ 交还给浏览器原生
        // 激活。原实现无条件 `act("once")`，于是焦点落在「拒绝」上按
        // Enter 会同时触发原生 onClick(deny) 和这里的 once —— 一次按键
        // 既拒绝又批准，agent 循环先收 deny 再收 approve。
        const onActivatable = t?.closest("button, a[href], summary, [role='button']");
        if (onActivatable) return;
        e.preventDefault();
        act("once");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [total, single, batch, onFullAccess, feedback]);

  if (total === 0) return null;

  // R3-5（2026-10-16）：批量此前恒显示「中风险」——批量项自带 risk 字段，
  // 按「最险者胜」推导（一个 dangerous 就整批按 dangerous 提示）。
  const batchRisk = batch
    ? batch.items.reduce<Risk>((worst, it) => {
        const rank: Record<Risk, number> = { safe: 0, caution: 1, dangerous: 2 };
        const k = riskKey(it.risk);
        return rank[k] > rank[worst] ? k : worst;
      }, "safe")
    : "safe";
  const risk = single ? riskKey(single.riskLevel) : batchRisk;
  const riskCls =
    risk === "dangerous" ? "text-text-error bg-error-soft"
    : risk === "safe" ? "text-text-success bg-success-soft"
    : "text-text-warning bg-warning-soft";

  const diff = single?.diff ?? null;
  // toolVerb 对陌生工具会回退成工具名本身 —— 头部就会出现
  // 「cua_screen_info · cua_screen_info」这种复读。去重。
  const verb = single ? toolVerb(single.tool, JSON.stringify(single.args ?? "")) : "";
  const title = single
    ? (verb === single.tool ? `调用工具 ${single.tool}` : `${verb} · ${single.tool}`)
    : `批量审批 · ${batch?.items.length ?? 0} 个操作`;
  const cmd = single ? (commandText(single.tool, single.args) ?? compactArgs(single.args)) : null;
  const rawArgs = single ? JSON.stringify(single.args ?? {}, null, 2) : "";
  const allowAllVerb = single ? "允许这一次" : "允许全部";

  const options: { key: string; label: string; desc: string; run: Action }[] = [
    { key: "1", label: single ? "允许这一次" : "允许全部", desc: "只放行本次调用，下次相同操作仍会确认", run: "once" },
    { key: "2", label: single ? "始终允许此命令" : "始终允许全部", desc: "记入允许列表，相同命令以后不再询问", run: "always" },
    ...(onFullAccess
      ? [{ key: "3", label: "放手模式", desc: "切到放手模式：后续工具调用不再申请权限", run: "full" as Action }]
      : []),
    { key: "4", label: single ? "拒绝" : "全部拒绝", desc: "否决本次调用（Esc）", run: "deny" as Action },
    { key: "5", label: "告诉模型接下来应该怎么做…", desc: "说明会回给模型，让它换方案而不是重试被拒的调用", run: "note" as Action },
  ];
  const kbdOf = (optLabel: string) => options.find((o) => o.label === optLabel)?.key ?? "";

  return (
    <div className="approval-drawer" role="alertdialog" aria-label="需要权限">
      {/* 头：标题 + 风险 + 工具身份 + 队列计数 */}
      <div className="approval-drawer__head">
        <span className="approval-drawer__title">需要权限</span>
        <span className={`approval-drawer__risk ${riskCls}`}>
          {risk === "safe" ? <ShieldCheck className="icon-sm" /> : <AlertTriangle className="icon-sm" />}
          {RISK_LABEL[risk]}
        </span>
        <span className="approval-drawer__tool">{title}</span>
        <span className="approval-drawer__count">
          {total > 1 ? `1 / ${total}` : ""}
        </span>
      </div>

      {/* 状态 + 原因（后端附带 reason 时展示） */}
      <div className="approval-drawer__statusline">
        <span className="approval-drawer__status">⏳ 等待确认</span>
        {single?.reason && <span className="approval-drawer__desc">{single.reason}</span>}
      </div>

      {/* 体：命令原样 / 批量清单 / 参数详情 / diff */}
      <div className="approval-drawer__body">
        {single && cmd && (
          <div className="approval-drawer__cmd">
            <span className="approval-drawer__cmd-label">即将执行</span>
            <div className="approval-drawer__cmd-text">{cmd}</div>
          </div>
        )}
        {single && rawArgs.trim() !== "{}" && (
          <details className="approval-drawer__args">
            <summary className="approval-drawer__args-summary">参数详情</summary>
            <pre className="approval-drawer__args-pre">{rawArgs}</pre>
          </details>
        )}
        {diff && (
          <div className="approval-drawer__diff">
            <button
              type="button"
              className="approval-drawer__diff-toggle"
              onClick={() => setShowDiff((v) => !v)}
              aria-expanded={showDiff}
            >
              {showDiff ? <ChevronUp className="icon-sm" /> : <ChevronDown className="icon-sm" />}
              查看改动
            </button>
            {showDiff && <pre className="approval-drawer__diff-pre">{diff}</pre>}
          </div>
        )}
        {batch && (
          <ul className="approval-drawer__items">
            {batch.items.slice(0, 3).map((it) => (
              <li key={it.requestId ?? it.tool}>
                <span className="approval-drawer__item-tool">{it.tool}</span>
                <span className="approval-drawer__item-desc">
                  {describeToolArgs(it.tool, it.args)}
                </span>
              </li>
            ))}
            {batch.items.length > 3 && (
              <li className="approval-drawer__more">…等 {batch.items.length} 个操作</li>
            )}
          </ul>
        )}

        {/* 5 选项：每个带作用域说明 */}
        <div className="approval-drawer__options">
          {options.map((o) =>
            o.run === "note" ? null : (
              <button
                key={o.key}
                type="button"
                className={
                  o.run === "once"
                    ? "approval-drawer__option approval-drawer__option--primary"
                    : o.run === "deny"
                      ? "approval-drawer__option approval-drawer__option--danger"
                      : "approval-drawer__option"
                }
                onClick={() => act(o.run)}
              >
                <kbd className="approval-drawer__option-key">{o.key}</kbd>
                <span className="approval-drawer__option-label">{o.label}</span>
                <span className="approval-drawer__option-desc">{o.desc}</span>
              </button>
            ),
          )}
          {/* 第 5 项：反馈输入行（"5" 聚焦；输入框内 ⏎ 提交 → 带 note 的拒绝） */}
          <div className="approval-drawer__option approval-drawer__option--feedback" onClick={() => feedbackRef.current?.focus()}>
            <kbd className="approval-drawer__option-key">{kbdOf("告诉模型接下来应该怎么做…")}</kbd>
            <input
              ref={feedbackRef}
              className="approval-drawer__feedback-input"
              value={feedback}
              placeholder="告诉模型接下来应该怎么做…（Enter 提交）"
              onChange={(e) => setFeedback(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && feedback.trim()) {
                  e.preventDefault();
                  act("note");
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
                  act("deny");
                }
              }}
            />
            <button
              type="button"
              className="approval-drawer__feedback-send"
              disabled={!feedback.trim()}
              onClick={(e) => { e.stopPropagation(); act("note"); }}
              aria-label="提交说明并拒绝"
            >
              <Send className="icon-sm" />
            </button>
          </div>
        </div>
      </div>

      {/* 脚：键盘导航提示 */}
      <div className="approval-drawer__hint">
        数字键 {onFullAccess ? "1–5" : "1–2、4–5"} 选择 · Esc 拒绝 · ⏎ {allowAllVerb}（焦点在按钮上时 ⏎ 激活该按钮）
      </div>
    </div>
  );
}
