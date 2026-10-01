// 工具执行结果的**唯一**判定入口。
//
// 为什么单独一个零依赖模块（不 import React / markdown / 任何组件）：
// `messageParser.ts`、`ToolEventCard.tsx`、`protocolAdapter.ts` 三条渲染
// 路径都需要同一套判定，但前两者曾各自抄了一份 `detectToolError`，
// 靠注释「any change here MUST be mirrored」维持一致 —— 那是一条邀请
// 走散的约定，不是保证。实测后果：agent runtime 产生的
// `Tool 'x' does not exist. Available tools: ...` 三份实现都不认，
// 一条明确失败被渲染成「✓已完成」。
//
// 教训（同 H2）：契约类逻辑抽到唯一入口，不要靠注释同步两份实现。

/** 一个工具调用结束后的四种结局。`skipped` 是新增的第四态。 */
export type ToolOutcomeKind = "error" | "skipped";

/** 一次调用的终态判定结果。`kind === null` 表示「正常完成」。 */
export interface ToolOutcome {
  kind: ToolOutcomeKind | null;
  /** 一行可读原因，供 UI 直接展示（不做二次截断）。 */
  reason: string;
}

/**
 * 审批超时 / 用户拒绝 / 被收敛检测掐断 —— 工具**没有真正跑**。
 *
 * 这类不是「失败」：审批超时是无人应答，用户拒绝是有人做了决定。
 * 塞进 `error` 会让「失败数」虚高，塞进 `done` 则是撒谎（本次要修的
 * 正是后者：`The 600-second approval window expired ... so the tool
 * was not executed` 曾打「✓已完成」）。
 *
 * **不受 `is_error` 门控。** 即使 server 报 `is_error: false`，文本
 * 明说「没执行」时也必须落到 `skipped` —— 宁可显示「跳过」也不显示
 * 「完成」。反向的误报风险不存在：这些措辞不会出现在正常输出里。
 */
const SKIP_PATTERNS: RegExp[] = [
  // "The 600-second approval window expired before a decision was made"
  /\bapproval\s+(window\s+)?(expired|timed\s*out)\b/i,
  /\bapproval\b[^.]{0,60}\b(expired|timed\s*out|timeout)\b/i,
  // 用户明确拒绝 / 主动取消
  /\b(denied|rejected|cancell?ed)\s+by\s+(the\s+)?user\b/i,
  /\buser\s+(denied|rejected|cancell?ed)\b/i,
  // "so the tool was not executed" —— 必须同段出现审批语境，
  // 否则正常输出里一句 "not executed" 就会误判
  /\bnot\s+(executed|run)\b/i,
];

function looksSkipped(trimmed: string): boolean {
  if (/\bnot\s+(executed|run)\b/i.test(trimmed)) {
    // 需要审批语境佐证
    return /\b(approval|window|decision|timeout|expired|expire)\b/i.test(trimmed);
  }
  return SKIP_PATTERNS.slice(1).some((re) => re.test(trimmed));
}

/**
 * agent runtime / 参数校验层的失败措辞。三份旧实现都只认
 * `^(Failed|Error|ERR|Tool execution failed)`，覆盖不到这些 ——
 * 它们不是 `ToolError` 抛出的，是 runtime 自己拼的字符串。
 */
const RUNTIME_ERROR_PATTERNS: RegExp[] = [
  /\bdoes\s+not\s+exist\b/i, // "Tool 'tool_search' does not exist. Available tools: ..."
  /\binvalid\s+tool\s+parameters\b/i, // "Invalid tool parameters: Missing 'command' parameter"
  /\bmissing\b[^.]{0,24}\bparameters?\b/i, // "Missing 'command' parameter"
  /\btool\b[^.]{0,24}\bnot\s+found\b/i,
];

const LEADING_ERROR_RE = /^(Failed|Error|ERR|Tool execution failed)/i;

function clip(s: string, max = 120): string {
  return s.length > max ? `${s.slice(0, max - 3)}…` : s;
}

/**
 * 判定一次工具调用的结局。**运行态与恢复态都必须走这里。**
 *
 * @param content  `tool_complete` 的结果文本（或 `role:"tool"` 行的 content）
 * @param isError  server 的 `ToolResult::is_error`。`false` 短路「失败」判定
 *                 （正常输出恰好以 "Error" 开头不算失败），但不短路 `skipped`。
 */
export function classifyToolOutcome(
  content?: string | null,
  isError?: boolean,
): ToolOutcome {
  if (!content) return { kind: null, reason: "" };
  const trimmed = content.trim();
  if (!trimmed) return { kind: null, reason: "" };

  // 顺序要紧：审批超时的文案常常也带 "not executed"，先判 skip。
  if (looksSkipped(trimmed)) {
    return { kind: "skipped", reason: clip(trimmed) };
  }

  // `is_error === false` 是 server 的权威成功声明，跳过文本检查。
  if (isError === false) return { kind: null, reason: "" };

  if (isError === true) {
    return { kind: "error", reason: clip(trimmed) };
  }

  if (LEADING_ERROR_RE.test(trimmed)) {
    return { kind: "error", reason: clip(trimmed) };
  }
  if (RUNTIME_ERROR_PATTERNS.some((re) => re.test(trimmed))) {
    return { kind: "error", reason: clip(trimmed) };
  }

  // JSON 包装：exit_code ≠ 0 / 顶层 error 字段
  if (trimmed.startsWith("{")) {
    try {
      const parsed = JSON.parse(trimmed) as Record<string, unknown>;
      if (typeof parsed.error === "string" && parsed.error.trim().length > 0) {
        return { kind: "error", reason: clip(parsed.error.trim()) };
      }
      if (
        typeof parsed.exit_code === "number" &&
        parsed.exit_code !== 0 &&
        typeof parsed.stderr === "string" &&
        parsed.stderr.trim().length > 0
      ) {
        return { kind: "error", reason: clip(parsed.stderr.trim()) };
      }
      if (typeof parsed.exit_code === "number" && parsed.exit_code !== 0) {
        return { kind: "error", reason: `exit code ${parsed.exit_code}` };
      }
    } catch {
      /* 不是 JSON 就按普通文本处理 */
    }
  }

  return { kind: null, reason: "" };
}

/**
 * 兼容旧签名 `(content, isError) => string | null`。
 * `protocolAdapter` 与 `api/rest` 把它当函数值往下传，签名不能动。
 * 注意 `skipped` 在这里**也返回 reason** —— 调用方据此置 `errorReason`，
 * 再配合 `classifyToolOutcome` 决定状态。旧调用方拿不到 `skipped` 状态
 * 是可接受的降级：至少 reason 还在，不会被吞掉。
 */
export function detectToolError(content?: string, isError?: boolean): string | null {
  const { kind, reason } = classifyToolOutcome(content, isError);
  return kind ? reason : null;
}
