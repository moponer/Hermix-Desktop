// 折叠粒度的回归测试。
//
// 2026-10-01 用户实机截图定调（推翻此前"一轮一个总开关"的做法）：
//
//   - **运行中**：每段思考是**各自独立**的折叠项，正在想的那段展开、
//     刚结束的那段自动收起。工具行平铺可见。
//   - **结束后**：整轮的多段思考 + 全部执行过程收成顶部一行
//     （`已工作 N 分 M 秒`），下面只剩正文。
//
// 此前的实现是 `expanded = isStreaming || keepOpen ? true : …` ——
// 运行中恒展开且不可收，多段思考糊成一整片，正是截图里那个问题。
import { describe, it, expect } from "vitest";
import {
  appendReasoningStep,
  appendTextStep,
  appendToolStep,
  closeOpenReasoning,
} from "../app/streamEvents";
import { mergeAdjacentReasoning, resolveSteps } from "../components/messageParser";
import type { ChatMessage, ExecutionTimelineEvent, TimelineStep } from "../types";

const t = (ms: number) => new Date(ms).toISOString();
const START = 1_700_000_000_000;

function toolEvent(over: Partial<ExecutionTimelineEvent> = {}): ExecutionTimelineEvent {
  return {
    type: "tool_start",
    tool: "terminal",
    arguments: '{"command":"ls"}',
    created_at: t(START + 1000),
    ...over,
  } as ExecutionTimelineEvent;
}

/** 复刻真实流式顺序：思考 → 工具 → 思考 → 工具 → 正文。 */
function buildStream(): TimelineStep[] {
  let steps: TimelineStep[] | undefined;
  steps = appendReasoningStep(steps, "第一段：先确认现状");
  steps = appendToolStep(steps, toolEvent());
  steps = appendToolStep(steps, toolEvent({ type: "tool_complete", created_at: t(START + 3000) }));
  steps = appendReasoningStep(steps, "第二段：发现漏洞");
  steps = appendToolStep(steps, toolEvent({ created_at: t(START + 5000) }));
  steps = appendTextStep(steps, "修好了。");
  return steps;
}

describe("每段思考独立成 step", () => {
  it("被工具隔开的思考不被合并成一段", () => {
    const steps = buildStream();
    const reasoning = steps.filter((s) => s.kind === "reasoning");
    expect(reasoning).toHaveLength(2);
    expect(mergeAdjacentReasoning(steps).filter((s) => s.kind === "reasoning")).toHaveLength(2);
  });

  it("相邻的思考仍会被合并（think-block 残留的既有行为不变）", () => {
    let steps: TimelineStep[] | undefined;
    steps = appendReasoningStep(steps, "上");
    steps = appendReasoningStep(steps, "下");
    expect(steps).toHaveLength(1);
    expect(steps![0]).toMatchObject({ kind: "reasoning", content: "上下" });
  });
});

describe("思考段的时间戳", () => {
  it("新段打 startedAt，同段追加不重置", () => {
    const a = appendReasoningStep(undefined, "x");
    const b = appendReasoningStep(a, "y");
    expect(b[0]).toMatchObject({ kind: "reasoning", startedAt: expect.any(String) });
    expect((b[0] as { startedAt?: string }).startedAt).toBe(
      (a[0] as { startedAt?: string }).startedAt,
    );
  });

  it("下一段工具/正文追加时闭合上一段思考（endedAt 被补上）", () => {
    let steps: TimelineStep[] | undefined;
    steps = appendReasoningStep(steps, "在想了");
    expect((steps![0] as { endedAt?: string }).endedAt).toBeUndefined();

    steps = appendToolStep(steps, toolEvent());
    const first = steps![0] as { kind: string; endedAt?: string };
    expect(first.kind).toBe("reasoning");
    expect(first.endedAt).toEqual(expect.any(String));
  });

  it("closeOpenReasoning 幂等：已闭合的不再改写", () => {
    let steps: TimelineStep[] | undefined;
    steps = appendReasoningStep(steps, "a");
    steps = appendToolStep(steps, toolEvent());
    const once = steps![0] as { endedAt?: string };
    const twice = closeOpenReasoning(steps!);
    expect((twice[0] as { endedAt?: string }).endedAt).toBe(once.endedAt);
  });

  it("合并相邻段时保住时间跨度（取最早开始 / 最晚结束）", () => {
    const merged = mergeAdjacentReasoning([
      { kind: "reasoning", content: "上", startedAt: t(START), endedAt: t(START + 1000) },
      { kind: "reasoning", content: "下", startedAt: t(START + 1000), endedAt: t(START + 4000) },
    ]);
    expect(merged[0]).toMatchObject({
      kind: "reasoning",
      content: "上\n\n下",
      startedAt: t(START),
      endedAt: t(START + 4000),
    });
  });
});

describe("恢复态不带编造的时长", () => {
  it("重载历史没有时间戳 → 标签退化为「思考」，不显示秒数", () => {
    // 恢复态走 buildFallbackSteps / chatUtils.rebuildUnifiedSteps，
    // 那些路径不产生 startedAt。这里钉住「没数据就不显示时长」——
    // 显示一个编出来的「持续了 0 秒」比不显示更糟。
    const msg = {
      id: "m1",
      role: "assistant",
      content: "",
      timestamp: t(START),
      reasoningContent: "恢复出来的思考",
    } as ChatMessage;
    const steps = resolveSteps(msg);
    const r = steps.find((s) => s.kind === "reasoning") as
      | { startedAt?: string; endedAt?: string }
      | undefined;
    expect(r).toBeDefined();
    expect(r?.startedAt).toBeUndefined();
  });
});
