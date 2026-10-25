// 「助手阿黎」页内联对话的转录合并（纯函数，无 React / 无网络）。
//
// 两路数据源：
//   - history：`GET /v1/sessions/twin:ali/messages` 拉到的**落库**轮次，
//     手机投递进来的、App 重启前的旧历史都只在这里。
//   - live：她自己的发送器持有的本轮（user 行 + 流式增量），不经过宿主
//     会话表 —— 她那页与任何对话并行，互不占用彼此的状态。
//
// 合并的难点是**同一轮两边都有**：本轮落库后，history 的尾部与本轮的
// live 是同一批消息，直接拼接会把同一句话显示两遍。这里用「最后一条 user
// 文本」在 history 里定位切点，切点之后交给 live（更完整，可能是流式中的
// 半截文本）。

/** 内联面板最多显示几条（她的页面是信息板，不是完整对话页）。 */
export const TRANSCRIPT_CAP = 6;

export interface TranscriptRow {
  role: string;
  text: string;
  /** 工具活动行（该轮只有 tool_calls、没有可见文本）——渲染为小字活动，
   *  与正式回复区分。没有它，工具轮次在阿黎页就是一段空洞
   *  （2026-10-25 用户实测截图的三个空块）。 */
  kind?: "activity";
}

/** 单行最大字符数：面板最多两行，超长文本截断后仍是同一句。 */
const ROW_MAX_CHARS = 300;

/** 只取能直接读的对话行：user / assistant 的非空文本；纯工具轮次降级为
 *  活动行（🔧 工具名），不再渲染成空白块。 */
export function toTranscriptRows(
  messages: {
    role: string;
    content?: string | null;
    tool_calls?: unknown[] | null;
  }[],
): TranscriptRow[] {
  const rows: TranscriptRow[] = [];
  for (const m of messages) {
    if (m.role !== "user" && m.role !== "assistant") continue;
    const text = (m.content ?? "").trim();
    if (!text) {
      // 纯工具轮次：提取本轮工具名，保留过程可见性。
      // ChatMessage.ToolCall 是平铺 {name}；wire 形状是 {function:{name}}——两者兼容。
      const names = (m.tool_calls ?? [])
        .map((tc) => {
          const t = tc as { name?: string; function?: { name?: string } } | null;
          return t?.name ?? t?.function?.name;
        })
        .filter((n): n is string => Boolean(n));
      if (names.length > 0) {
        rows.push({
          role: "assistant",
          text: `🔧 ${names.join("、")}`,
          kind: "activity",
        });
      }
      continue;
    }
    rows.push({
      role: m.role,
      text: text.length > ROW_MAX_CHARS ? `${text.slice(0, ROW_MAX_CHARS - 1)}…` : text,
    });
  }
  return rows;
}

/**
 * 合并落库历史与内存中的本轮。
 *
 * - live 为空：纯用 history 的尾部。
 * - live 非空：在 history 里找最后一条与 live 最后一条 user 相同的行，
 *   该行之前的部分接上 live，再统一裁到 `TRANSCRIPT_CAP`。
 */
export function mergeTranscript(
  history: TranscriptRow[],
  live: TranscriptRow[],
  cap: number = TRANSCRIPT_CAP,
): TranscriptRow[] {
  if (live.length === 0) return history.slice(-cap);
  const lastUser = [...live].reverse().find((r) => r.role === "user")?.text;
  let cut = history.length;
  if (lastUser) {
    for (let i = history.length - 1; i >= 0; i--) {
      if (history[i].role === "user" && history[i].text === lastUser) {
        cut = i;
        break;
      }
    }
  }
  return [...history.slice(0, cut), ...live].slice(-cap);
}
