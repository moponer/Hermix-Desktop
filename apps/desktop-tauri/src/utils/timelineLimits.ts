// 执行时间线 / 统一步骤的**体积上限**（前端侧）。
//
// 起因（2026-10-29 实测）：一个 30 多次工具调用的回合，落库后单条消息
// `unified_steps` 1.1MB + `execution_timeline` 1.1MB，单会话 5.7MB。构成
// 几乎全是 `tool_stdout`：每个 16KB 分片一个事件、一次 `terminal` 输出
// 50KB 就留 4 个事件，还和 `tool_complete` 的预览重复。
//
// 后果不是显示问题，是**切会话要 JSON.parse 几 MB**、parsed 对象常驻几十 MB，
// 而折叠态只看首行、展开时才需要正文。
//
// 策略与后端 `timeline_limits.rs` 完全一致：每次调用只留**一个** stdout
// 事件，超限时保留头尾、中间插一行省略说明（错误常在尾部，上下文在头部）。

import type { ExecutionTimelineEvent, TimelineStep } from "../types";

/** 单次工具调用保留的 stdout 上限（**字符**，不是字节）。 */
export const STDOUT_CAP_PER_CALL = 24 * 1024;

function clipToChars(s: string, max: number): string {
  // 按码点切，避免把 emoji / 代理对劈成半个
  const chars = Array.from(s);
  return chars.length <= max ? s : chars.slice(0, max).join("");
}

function tailChars(s: string, max: number): string {
  const chars = Array.from(s);
  return chars.length <= max ? s : chars.slice(chars.length - max).join("");
}

/**
 * 把 `old + next` 裁到 `cap` 个字符：超限保留头尾，中间留一行省略说明。
 *
 * 头尾都要的理由：错误信息通常在输出尾部（stderr 最后几行），而上下文、
 * 命令行回显在头部；只留头会把用户最想看的失败原因丢掉。
 */
export function capStdoutText(old: string, next: string, cap = STDOUT_CAP_PER_CALL): string {
  const joined = old + next;
  const total = Array.from(joined).length;
  if (total <= cap) return joined;
  const headBudget = Math.floor(cap / 2);
  const tailBudget = cap - headBudget;
  return (
    clipToChars(joined, headBudget) +
    `\n…（中间已省略 ${total - cap} 字符，保留头尾）\n` +
    tailChars(joined, tailBudget)
  );
}

/**
 * 持久化前的压缩（幂等）：每个 `tool_call_id` 只留一个 `tool_stdout` 事件，
 * 合并后的内容按上限留头尾；非 stdout 事件原样保留（顺序、类型都不动）。
 *
 * **幂等怎么做到的**：只有"同一调用出现多条 stdout"才合并，合并时先 join
 * 再裁一次；单条事件原样透传，不再重新裁。否则第二次压缩会把已裁过的
 * 内容再切一刀、再插一行省略说明（实测：`capStdoutText` 直接套两次不幂等）。
 * 同一条消息会被持久化两次（中途一轮结束 + 最终 Done），所以这条必须成立。
 *
 * 返回值在「没有任何改动」时是**原数组引用**，调用方可以据引用相等跳过重渲染。
 */
export function compactStepsForPersist(
  steps: TimelineStep[],
  cap = STDOUT_CAP_PER_CALL,
): TimelineStep[] {
  let anyChanged = false;

  const out = steps.map((step) => {
    if (step.kind !== "tools") return step;

    // 先数清每个调用有几条 stdout：只有 >1 条才需要合并
    const counts = new Map<string, number>();
    for (const ev of step.events) {
      if (ev.type !== "tool_stdout") continue;
      const key = ev.tool_call_id ?? "";
      if (!key) continue;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    const needsMerge = [...counts.values()].some((n) => n > 1);
    if (!needsMerge) return step;

    const emitted = new Set<string>();
    const events: ExecutionTimelineEvent[] = [];
    for (const ev of step.events) {
      if (ev.type !== "tool_stdout") {
        events.push(ev);
        continue;
      }
      const key = ev.tool_call_id ?? "";
      if (!key || (counts.get(key) ?? 0) <= 1) {
        events.push(ev); // 单条：原样保留（也保证幂等）
        continue;
      }
      if (emitted.has(key)) continue; // 后面的分片并进第一条去了
      emitted.add(key);
      const body = step.events
        .filter((e) => e.type === "tool_stdout" && e.tool_call_id === key)
        .map((e) => e.content ?? "")
        .join("");
      events.push({ ...ev, content: capStdoutText("", body, cap) });
    }

    anyChanged = true;
    return { kind: "tools", events } as TimelineStep;
  });

  return anyChanged ? out : steps;
}
