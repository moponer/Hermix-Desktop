import { describe, it, expect } from "vitest";
import {
  STDOUT_CAP_PER_CALL,
  capStdoutText,
  compactStepsForPersist,
} from "./timelineLimits";
import type { ExecutionTimelineEvent, TimelineStep } from "../types";

function stdout(id: string, content: string): ExecutionTimelineEvent {
  return {
    type: "tool_stdout",
    tool: "terminal",
    tool_call_id: id,
    content,
    created_at: "2026-10-29T00:00:00Z",
  } as ExecutionTimelineEvent;
}

function start(id: string): ExecutionTimelineEvent {
  return {
    type: "tool_start",
    tool: "terminal",
    tool_call_id: id,
    arguments: "{}",
    created_at: "2026-10-29T00:00:00Z",
  } as ExecutionTimelineEvent;
}

describe("capStdoutText", () => {
  it("未超限时原样拼接", () => {
    expect(capStdoutText("aa", "bb", 1024)).toBe("aabb");
  });

  it("超限保留头尾并说明省略了多少", () => {
    const head = "H".repeat(30);
    const middle = "M".repeat(100);
    const tail = "T".repeat(30);
    const out = capStdoutText(head + middle, tail, 40);
    expect(out.startsWith("H")).toBe(true);
    expect(out.endsWith("T")).toBe(true);
    expect(out).not.toContain("M");
    expect(out).toMatch(/已省略 \d+ 字符/);
  });

  it("按字符而不是字节省略（中文不过度截断）", () => {
    const out = capStdoutText("", "中".repeat(100), 60);
    expect(Array.from(out).length).toBeLessThanOrEqual(60 + 40);
  });
});

describe("compactStepsForPersist", () => {
  it("同一次调用的多个 stdout 分片合并成一个事件", () => {
    const steps: TimelineStep[] = [
      {
        kind: "tools",
        events: [start("c1"), stdout("c1", "aa"), stdout("c1", "bb"), stdout("c1", "cc")],
      },
    ];
    const out = compactStepsForPersist(steps);
    const tools = out[0] as { kind: "tools"; events: ExecutionTimelineEvent[] };
    const stdouts = tools.events.filter((e) => e.type === "tool_stdout");
    expect(stdouts).toHaveLength(1);
    expect(stdouts[0].content).toBe("aabbcc");
    expect(tools.events[0].type).toBe("tool_start");
  });

  it("不同调用各自保留一条", () => {
    const steps: TimelineStep[] = [
      { kind: "tools", events: [stdout("c1", "one"), stdout("c2", "two")] },
    ];
    const tools = compactStepsForPersist(steps)[0] as {
      kind: "tools";
      events: ExecutionTimelineEvent[];
    };
    expect(tools.events).toHaveLength(2);
  });

  it("幂等：压缩两次结果不变（不会多出一行省略说明）", () => {
    const big = "x".repeat(STDOUT_CAP_PER_CALL);
    const steps: TimelineStep[] = [
      { kind: "tools", events: [stdout("c1", big), stdout("c1", "tail")] },
    ];
    const once = compactStepsForPersist(steps);
    const twice = compactStepsForPersist(once);
    const a = (once[0] as { events: ExecutionTimelineEvent[] }).events[0].content;
    const b = (twice[0] as { events: ExecutionTimelineEvent[] }).events[0].content;
    expect(b).toBe(a);
  });

  it("没有可压缩内容时返回原数组引用（调用方可据引用相等跳过重渲染）", () => {
    const steps: TimelineStep[] = [{ kind: "text", content: "hi" }];
    expect(compactStepsForPersist(steps)).toBe(steps);
  });

  it("非 tools step 原样保留", () => {
    const steps: TimelineStep[] = [
      { kind: "reasoning", content: "想一下" },
      { kind: "tools", events: [stdout("c1", "a")] },
    ];
    const out = compactStepsForPersist(steps);
    expect(out[0]).toBe(steps[0]);
  });
});
