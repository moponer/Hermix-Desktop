/**
 * 阿黎的心跳巡查 —— **纯函数、零 LLM**。
 *
 * 设计边界（2026-11-06 定）：心跳只做"机械上可判定"的事，绝不每两分钟叫醒
 * 模型问一句"要不要干预"。2 分钟一次 × 一天 = 720 次；把它做成 LLM 调用
 * 既烧钱又没信息量，多数时候答案是"没事"。所以这里只回答四个问题：
 *   她卡住了吗？ / 有需求等你补充吗？ / 上一批失败了吗？ / 工单出事了吗？
 * 真需要她拿主意（改计划、重派），走的是"发现异常 → 你点一下"，
 * 而不是让心跳自己发号施令。
 *
 * 判据全部按**最后活动时间**，不按开始时间 —— 长编译、大文件下载十几分钟
 * 没有事件是正常的，按开始时间算会天天误报"卡死"。
 */
import type { TwinBatch, TwinTask } from "./twinQueue";

/** 心跳周期：2 分钟（用户指定）。 */
export const PATROL_INTERVAL_MS = 2 * 60_000;
/** 距最后一次活动超过这么久 → 疑似卡住（只提示，不自动停止）。 */
export const STALL_MS = 15 * 60_000;
/** 空闲状态下队列等了这么久还没被拉走 = 拉取链路没生效（正常 4s 内就该走）。 */
export const QUEUE_WAIT_MS = 60_000;

/** 建议的类型 —— 界面按 code 决定给哪个一键动作。 */
export type PatrolCode =
  | "stalled"
  | "blocked"
  | "batch_failed"
  | "tickets_failed"
  | "queue_waiting"
  | "tickets_unknown";

export interface PatrolFinding {
  code: PatrolCode;
  level: "warn" | "alert";
  text: string;
}

export interface PatrolTickets {
  running: number;
  done: number;
  failed: number;
}

export interface PatrolInput {
  now: number;
  /** 面板队列（含 blocked 的）。 */
  queue: TwinTask[];
  batch: TwinBatch | null;
  streaming: boolean;
  /** 本轮最后一次事件到达的时刻（ms）；没有在跑时为 null。 */
  lastActivityAt: number | null;
  /** 未决的审批 + 澄清数（它们已经在视线里，心跳不重复提示）。 */
  pendingCount: number;
  /** `null` = 工单状态没查到（不是"没有工单"）。 */
  tickets: PatrolTickets | null;
  /** 面板里最早一条**可拉取**需求的入队时刻（ms）。 */
  oldestQueuedAt: number | null;
}

export interface PatrolResult {
  at: number;
  /** 待拉取（不含 blocked）。 */
  queue: number;
  /** 等你补充 / 取消的。 */
  blocked: number;
  tickets: PatrolTickets | null;
  findings: PatrolFinding[];
}

/** 分钟数（向上取整，避免"0 分钟"这种读数）。 */
function mins(ms: number): number {
  return Math.max(1, Math.round(ms / 60_000));
}

export function patrol(input: PatrolInput): PatrolResult {
  const {
    now,
    queue,
    batch,
    streaming,
    lastActivityAt,
    pendingCount,
    tickets,
    oldestQueuedAt,
  } = input;

  const openCount = queue.filter((t) => !t.blocked).length;
  const blockedCount = queue.length - openCount;
  const findings: PatrolFinding[] = [];

  // ① 卡住：在跑但长时间没有任何事件。只提示 —— 停不停由人定，
  //    自动杀进程的风险远大于多等十几分钟。
  if (streaming && lastActivityAt != null && now - lastActivityAt > STALL_MS) {
    findings.push({
      code: "stalled",
      level: "alert",
      text: `本批已经 ${mins(now - lastActivityAt)} 分钟没有新动作，可能卡住了（要停就点停止，别干等）`,
    });
  }

  // ② 上一批失败：错误写的是什么就说什么，不翻译。
  if (batch?.status === "failed" && batch.error) {
    findings.push({
      code: "batch_failed",
      level: "warn",
      text: `上一批发不出去：${batch.error}`,
    });
  }

  // ③ 等补充：她退回的、缺目标的需求。
  if (blockedCount > 0) {
    findings.push({
      code: "blocked",
      level: "warn",
      text: `${blockedCount} 条需求缺目标，等你补充或取消`,
    });
  }

  // ④ 工单失败：只在"这一批还在跑"或"刚跑完"时有意义。
  if (tickets && tickets.failed > 0) {
    findings.push({
      code: "tickets_failed",
      level: "warn",
      text: `本批派出 ${tickets.failed} 个工单失败 / 被拒，需要看一眼`,
    });
  }

  // ⑤ 队列一直没被拉走：正常 4s 内就该走，等满一分钟说明链路有问题。
  if (!streaming && openCount > 0 && oldestQueuedAt != null && now - oldestQueuedAt > QUEUE_WAIT_MS) {
    findings.push({
      code: "queue_waiting",
      level: "warn",
      text: `队列等了 ${mins(now - oldestQueuedAt)} 分钟还没被拉走（拉取可能没生效）`,
    });
  }

  // ⑥ 工单查不到：如实说查不到，不说"没有工单"。
  if (tickets === null && streaming) {
    findings.push({
      code: "tickets_unknown",
      level: "warn",
      text: "这一轮的工单状态没查到（后端没应答），团队进度暂时不可见",
    });
  }

  // 有审批/澄清待办时，"长时间没有新动作"是正常的（她停下来等你），
  // 报"卡住"就是假警报。等待本身已经在「等你决定」那块里了，不重复报。
  const effective =
    pendingCount > 0 ? findings.filter((f) => f.code !== "stalled") : findings;

  return {
    at: now,
    queue: openCount,
    blocked: blockedCount,
    tickets,
    findings: effective,
  };
}

/** 巡查结论里最严重的一档（界面据此决定用警告色还是普通色）。 */
export function patrolLevel(result: PatrolResult | null): "ok" | "warn" | "alert" {
  if (!result) return "ok";
  if (result.findings.some((f) => f.level === "alert")) return "alert";
  if (result.findings.length > 0) return "warn";
  return "ok";
}

/**
 * 把巡查结论压成一段「上一批情况」，注入**下一批**开工指令。
 *
 * 为什么不单独发一轮问她"要不要干预"：每出现一次异常就多烧一轮对话，
 * 而且模型很容易从"给建议"滑到"直接动手"（她现在有终端和团队）。
 * 把情况塞进下一批的正常指令里，她在同一轮里自己权衡 —— 零额外调用、
 * 零自主行为、也不会在对话里留下一段无问题的长回复。
 *
 * 没有发现时返回 `null`，调用方据此**原样使用**批次指令（不加空壳段落）。
 */
export function patrolBrief(findings: PatrolFinding[]): string | null {
  if (findings.length === 0) return null;
  const lines = findings.map((f) => `- ${f.text}`).join("\n");
  return [
    "【上一批情况】",
    lines,
    "开工前先判断：这些情况要不要调整（失败的工单重派还是放弃、长时间没动静的是继续等还是先收尾）。",
    "不需要调整就按下面的需求直接做，不要为此额外问我。",
  ].join("\n");
}
