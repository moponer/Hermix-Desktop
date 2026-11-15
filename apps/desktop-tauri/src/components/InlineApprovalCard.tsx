import { ShieldAlert, Check, X, Terminal, Globe, Wrench, Clock } from "lucide-react";
import type { ApprovalDecision } from "../types";
import type { ApprovalMessageMeta } from "../types";
import { describeShellCommand } from "../lib/commandDescribe";
import { APPROVAL_RISK_LABEL } from "./approvalRisk";

export type ApprovalRequestShape = ApprovalMessageMeta;

export interface InlineApprovalCardProps {
  request: ApprovalRequestShape;
  onApprove: (requestId: string) => void;
  onDeny: (requestId: string) => void;
  resolved?: ApprovalDecision | null;
}

// 文案共用 `approvalRisk.ts`：此前这张卡写「高危」，另外三张卡写「高风险」——
// 同一个后端字段在同一个界面上有两种说法。样式类仍是本卡独有。
const RISK_BADGE: Record<string, { label: string; cls: string }> = {
  dangerous: { label: APPROVAL_RISK_LABEL.dangerous, cls: "approval-pill approval-pill-dangerous" },
  caution: { label: APPROVAL_RISK_LABEL.caution, cls: "approval-pill approval-pill-caution" },
  safe: { label: APPROVAL_RISK_LABEL.safe, cls: "approval-pill approval-pill-safe" },
};

/**
 * Generate a human-readable Chinese description of what the tool wants to do.
 * This replaces the raw JSON display with a clear action description.
 */
function describeToolAction(tool: string, args: unknown): string {
  if (args == null || typeof args !== "object") return "";
  const a = args as Record<string, unknown>;
  const t = tool.toLowerCase();

  // storage_clean: 清理临时文件
  if (t === "storage_clean") {
    const paths = Array.isArray(a.paths) ? a.paths : [];
    const mode = typeof a.mode === "string" ? a.mode : "trash";
    const modeText = mode === "trash" ? "移回收站" : "永久删除";
    if (paths.length === 0) return `${modeText}文件`;
    if (paths.length === 1) {
      const p = String(paths[0]);
      const name = p.split(/[/\\]/).pop() || p;
      return `${modeText} "${name}"`;
    }
    return `${modeText} ${paths.length} 个文件/文件夹`;
  }

  // terminal: 执行命令（Base64 编码命令会自动解码为中文语义）
  if (t === "terminal" || t.includes("exec")) {
    const cmd = typeof a.command === "string" ? a.command : "";
    if (cmd) return describeShellCommand(cmd).text;
    return "执行终端命令";
  }

  // write_file / patch: 写入文件
  if (t === "write_file" || t === "patch") {
    const path = typeof a.path === "string" ? a.path : "";
    if (path) {
      const name = path.split(/[/\\]/).pop() || path;
      return t === "patch" ? `修改文件 "${name}"` : `写入文件 "${name}"`;
    }
    return t === "patch" ? "修改文件" : "写入文件";
  }

  // read_file: 读取文件
  if (t === "read_file") {
    const path = typeof a.path === "string" ? a.path : "";
    if (path) {
      const name = path.split(/[/\\]/).pop() || path;
      return `读取文件 "${name}"`;
    }
    return "读取文件";
  }

  // web_search / web_extract: 网络请求
  if (t === "web_search") {
    const query = typeof a.query === "string" ? a.query : "";
    return query ? `搜索: ${query.slice(0, 40)}` : "网络搜索";
  }
  if (t === "web_extract") {
    const url = typeof a.url === "string" ? a.url : "";
    return url ? `提取网页内容` : "提取网页";
  }

  // browser: 浏览器操作
  if (t.includes("browser")) {
    const action = typeof a.action === "string" ? a.action : "";
    return action ? `浏览器操作: ${action}` : "浏览器操作";
  }

  // cua_*: 桌面操作
  if (t.startsWith("cua_")) {
    const action = t.replace("cua_", "").replace(/_/g, " ");
    return `桌面操作: ${action}`;
  }

  // memory: 记忆操作
  if (t === "memory") {
    const action = typeof a.action === "string" ? a.action : "";
    return action ? `记忆操作: ${action}` : "记忆操作";
  }

  // skills: 技能操作
  if (t === "skills" || t === "skill_manage") {
    return "技能管理操作";
  }

  // Default: show tool name
  return `执行 ${tool}`;
}

function toolIcon(tool: string) {
  const t = tool.toLowerCase();
  if (t.includes("terminal") || t.includes("shell") || t.includes("exec") || t.includes("bash")) return Terminal;
  if (t.includes("browser") || t.includes("web") || t.includes("http") || t.includes("fetch")) return Globe;
  return Wrench;
}

/**
 * Redesigned approval card — max 2 lines, no markdown formatting,
 * clear visual hierarchy, color-coded action buttons.
 *
 * Line 1: Tool name pill + risk badge + status pill
 * Line 2: Short args text (truncated) + action buttons
 */

export function InlineApprovalCard({ request, onApprove, onDeny, resolved }: InlineApprovalCardProps) {
  const riskKey = (request.riskLevel ?? "dangerous").toLowerCase();
  const badge = RISK_BADGE[riskKey] ?? RISK_BADGE.dangerous;
  const Icon = toolIcon(request.tool);
  const isPending = !resolved;
  const argsText = describeToolAction(request.tool, request.args);

  return (
    // h-leaving 折叠到 0 高度 — 决策一做出整张卡直接收回，避免“1 秒空白”：
    // 旧版 is-resolved 后卡片只把 actions 区折叠，行 2 留空 + “已允许”小标，
    // 视觉上半空不空；新版 resolve 触发外层折叠动画，父层下一个消息到达时
    // 直接占位，不留空隙。
    <div
      className={`h-enter ${!isPending ? "h-leaving" : ""}`}
      // 审批是"必须被打断"的交互：读屏用户此前对"现在需要我决定"毫无感知
      // （本文件不在任何含 aria-live 的清单里）。assertive 让卡片一出现就被播报。
      role="group"
      aria-live="assertive"
      aria-label="需要你审批的工具调用"
    >
      <div className="flex-1 min-w-0">
        {/* Header — only while pending. Resolved cards let the status
            pill ("已允许 / 已拒绝 / 已超时") speak for itself; the
            explicit "Hermix 请求确认 · 已决定" line becomes redundant
            noise once the decision is recorded, and dropping it gives
            the command + footer more breathing room. */}
        {/* Card surface — rendered inline in the chat stream. */}
        <div
          className={`msg-card approval-card${!isPending ? " is-resolved" : ""}`}
          data-resolved={resolved ?? undefined}
          data-pending-risk={isPending ? riskKey : undefined}
          data-pending-approval-id={isPending ? request.requestId : undefined}
        >
          {/* Line 1: Tool pill + Risk badge + Status */}
          <div className="approval-pills">
            <span className="approval-tool-pill">
              <Icon className="icon-sm shrink-0" />
              <span className="font-mono">{request.tool}</span>
            </span>
            <span className={badge.cls}>
              <ShieldAlert className="icon-xs" />
              {badge.label}
            </span>
            {resolved === "approved" && (
              <span className="approval-pill approval-pill-approved ml-auto">
                <Check className="icon-xs" />
                已允许
              </span>
            )}
            {resolved === "denied" && (
              <span className="approval-pill approval-pill-denied ml-auto">
                <X className="icon-xs" />
                已拒绝
              </span>
            )}
            {resolved === "expired" && (
              <span className="approval-pill approval-pill-expired ml-auto">
                <Clock className="icon-xs" />
                已超时
              </span>
            )}
          </div>

          {/* Line 2: Args text + Action buttons */}
          <div className="approval-line2">
            {argsText && (
              <span className={`approval-args-text${!isPending ? " is-resolved" : ""}`}>{argsText}</span>
            )}
            <div className={`approval-actions${!isPending ? " is-hidden" : ""}`}>
              <button
                onClick={() => onDeny(request.requestId)}
                className="approval-btn approval-btn-danger"
              >
                <X className="icon-sm" />
                拒绝
              </button>
              <button
                onClick={() => onApprove(request.requestId)}
                className="approval-btn approval-btn-approve"
              >
                <Check className="icon-sm" />
                允许
              </button>
            </div>
            <div className={`approval-resolved-footer${isPending ? " is-hidden" : ""}`}>
              <span
                className={
                  resolved === "approved"
                    ? "approval-resolved-icon icon-ok"
                    : resolved === "expired"
                      ? "approval-resolved-icon icon-expired"
                      : "approval-resolved-icon icon-deny"
                }
                aria-hidden
              >
                {resolved === "approved" ? (
                  <Check className="icon-sm" />
                ) : resolved === "expired" ? (
                  <Clock className="icon-sm" />
                ) : (
                  <X className="icon-sm" />
                )}
              </span>
              <span>
                {resolved === "approved"
                  ? "操作已继续执行"
                  : resolved === "expired"
                    ? "操作已自动取消；如仍想执行，请重新发起"
                    : "操作已中止"}
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
