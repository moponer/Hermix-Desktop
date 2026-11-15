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

/**
 * 单条消息的最大字符数。
 *
 * 2026-11-06 从 300 提到 1200：阿黎这一页要交付的是「部署说明」和「完成
 * 汇报」，300 字刚好把结论砍在半句上（`…` 之后才是"还差什么"）。仍保留
 * 上限 —— 她偶尔会整段贴命令输出，那属于工具结果，不该占据信息板。
 */
export const ROW_MAX_CHARS = 1200;

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
 * - live 为空：裁掉 history 尾部「没人回复的提问」再取尾。
 *   上一轮被打断（停止 / 崩溃 / 断连）时 early-persist 已经把提问落库，
 *   回复却没写回，库里就留下孤儿提问 —— 摆出来是两条连续的用户气泡，
 *   看起来像「她答错了题」（2026-10-27 实报 `[user(旧), user(新)]`）。
 * - live 非空且带 user 行：在 history 里找最后一条与它同文的提问，连同
 *   它前面紧挨着的连续用户行（被打断的孤儿）一起交给 live 顶替。
 * - live 非空但没有 user 行：历史原样保留。
 *
 * 最后一条是 2026-10-28 修的：此前无论 live 有没有 user 行都先裁历史尾部，
 * 于是本轮只剩一条错误提示时（`live = [assistant("出错了")]`），连刚才问
 * 的那句话都被裁掉了 —— 面板上一条无头无尾的「出错了」。
 */
export function mergeTranscript(
  history: TranscriptRow[],
  live: TranscriptRow[],
  cap: number = TRANSCRIPT_CAP,
): TranscriptRow[] {
  if (live.length === 0) return trimOrphanTail(history).slice(-cap);

  const lastUser = [...live].reverse().find((r) => r.role === "user")?.text;
  if (!lastUser) return [...history, ...live].slice(-cap);

  let hit = -1;
  for (let i = history.length - 1; i >= 0; i--) {
    if (history[i].role === "user" && history[i].text === lastUser) {
      hit = i;
      break;
    }
  }
  // 本轮还没落库：整段历史接在本轮前面。
  if (hit < 0) return [...history, ...live].slice(-cap);

  let start = hit;
  while (start > 0 && history[start - 1].role === "user") start -= 1;
  return [...history.slice(0, start), ...live].slice(-cap);
}

/** 去掉尾部连续的用户行（无人回复的提问）。 */
function trimOrphanTail(history: TranscriptRow[]): TranscriptRow[] {
  let end = history.length;
  while (end > 0 && history[end - 1].role === "user") end -= 1;
  return history.slice(0, end);
}
