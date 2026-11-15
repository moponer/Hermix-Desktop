// 心跳巡查的判据。
//
// 2026-11-06 定：心跳**零 LLM**，只做机械判定 —— 卡没卡、有没有待补充、
// 上一批是不是失败、工单出事没、队列是不是根本没被拉走。
// 这里钉住四件最容易写错的事：
//   ① 卡住按"最后活动"算，不按开始时间（长任务不误报）；
//   ② 有审批待办时不算卡住（她在等你，不是挂了）；
//   ③ 工单查不到 ≠ 没有工单；
//   ④ 一切正常时 findings 必须为空（空转不产生噪音）。
import { describe, expect, it } from "vitest";
import {
  QUEUE_WAIT_MS,
  STALL_MS,
  patrol,
  patrolBrief,
  patrolLevel,
  type PatrolInput,
} from "./twinPatrol";
import type { TwinTask } from "./twinQueue";

const NOW = 1_800_000_000_000;

const task = (id: string, text: string, extra: Partial<TwinTask> = {}): TwinTask => ({
  id,
  text,
  createdAt: new Date(NOW - 5_000).toISOString(),
  ...extra,
});

const base = (over: Partial<PatrolInput> = {}): PatrolInput => ({
  now: NOW,
  queue: [],
  batch: null,
  streaming: false,
  lastActivityAt: null,
  pendingCount: 0,
  tickets: null,
  oldestQueuedAt: null,
  ...over,
});

describe("patrol", () => {
  it("一切正常：没有 findings（空转不刷噪音）", () => {
    const r = patrol(base({ queue: [], tickets: { running: 0, done: 0, failed: 0 } }));
    expect(r.findings).toEqual([]);
    expect(patrolLevel(r)).toBe("ok");
  });

  it("卡住按最后活动时间判定，并给 alert", () => {
    const r = patrol(
      base({
        streaming: true,
        lastActivityAt: NOW - STALL_MS - 60_000,
        batch: { id: "b1", tasks: [], startedAt: NOW - STALL_MS, status: "running" },
        // 工单查得到，避免 tickets_unknown 混进来混淆这条用例的意图。
        tickets: { running: 1, done: 0, failed: 0 },
      }),
    );
    expect(r.findings.map((f) => f.code)).toEqual(["stalled"]);
    expect(r.findings[0].level).toBe("alert");
    expect(patrolLevel(r)).toBe("alert");
  });

  it("刚刚有事件的任务不算卡住（长任务靠这个不误报）", () => {
    const r = patrol(
      base({
        streaming: true,
        lastActivityAt: NOW - 30_000,
        tickets: { running: 1, done: 0, failed: 0 },
      }),
    );
    expect(r.findings).toEqual([]);
  });

  it("有审批待办时，长时间无动作不报卡住（她在等你）", () => {
    const r = patrol(
      base({
        streaming: true,
        pendingCount: 1,
        lastActivityAt: NOW - STALL_MS * 2,
      }),
    );
    expect(r.findings.map((f) => f.code)).not.toContain("stalled");
  });

  it("待补充的需求单独计数，不混进待拉取", () => {
    const r = patrol(
      base({
        queue: [task("t1", "刷课程"), task("t2", "看看", { blocked: true, blockReason: "缺目标" })],
      }),
    );
    expect(r.queue).toBe(1);
    expect(r.blocked).toBe(1);
    expect(r.findings.map((f) => f.code)).toContain("blocked");
  });

  it("上一批失败：把原始原因带出来", () => {
    const r = patrol(
      base({
        batch: {
          id: "b1",
          tasks: [],
          startedAt: NOW,
          status: "failed",
          error: "后端 500",
        },
      }),
    );
    const f = r.findings.find((x) => x.code === "batch_failed");
    expect(f?.text).toContain("后端 500");
  });

  it("工单失败要报，查不到也要报（不等于没有工单）", () => {
    const failed = patrol(
      base({ streaming: true, tickets: { running: 1, done: 0, failed: 2 } }),
    );
    expect(failed.findings.map((f) => f.code)).toContain("tickets_failed");

    const unknown = patrol(base({ streaming: true, tickets: null }));
    expect(unknown.findings.map((f) => f.code)).toContain("tickets_unknown");
  });

  it("空闲时队列等了超过一分钟还没被拉走 = 拉取没生效", () => {
    const r = patrol(
      base({
        queue: [task("t1", "刷课程")],
        oldestQueuedAt: NOW - QUEUE_WAIT_MS - 1_000,
      }),
    );
    expect(r.findings.map((f) => f.code)).toContain("queue_waiting");
  });

  it("队列刚入队不到一分钟不报（4s 快循环还会再试）", () => {
    const r = patrol(base({ queue: [task("t1", "刷课程")], oldestQueuedAt: NOW - 20_000 }));
    expect(r.findings.map((f) => f.code)).not.toContain("queue_waiting");
  });

  it("她正在跑时不报队列等待（排队的本来就要等这一轮）", () => {
    const r = patrol(
      base({
        streaming: true,
        queue: [task("t1", "刷课程")],
        oldestQueuedAt: NOW - QUEUE_WAIT_MS * 5,
      }),
    );
    expect(r.findings.map((f) => f.code)).not.toContain("queue_waiting");
  });
});

describe("patrolBrief（巡查发现注入下一批）", () => {
  it("没有发现时返回 null —— 调用方不加空壳段落", () => {
    expect(patrolBrief([])).toBeNull();
  });

  it("把发现压成一段「上一批情况」，并要求她自己权衡、不要反问", () => {
    const brief = patrolBrief([
      { code: "tickets_failed", level: "warn", text: "本批派出 1 个工单失败 / 被拒，需要看一眼" },
      { code: "stalled", level: "alert", text: "本批已经 15 分钟没有新动作" },
    ]);
    expect(brief).toContain("【上一批情况】");
    expect(brief).toContain("工单失败");
    expect(brief).toContain("15 分钟没有新动作");
    // 不允许她为此额外开一轮对话（那正是"要么没事要么烧钱"的老问题）。
    expect(brief).toContain("不要为此额外问我");
  });
});
