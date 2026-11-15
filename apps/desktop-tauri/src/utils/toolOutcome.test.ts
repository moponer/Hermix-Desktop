// 工具结局判定的回归测试。
//
// 这套判定原先有**三份实现**（`messageParser.detectToolError`、
// `ToolEventCard.detectToolError`、`protocolAdapter` 串的函数值），
// 靠一句注释「any change here MUST be mirrored」维持一致。三份都漏判
// 了 agent runtime 的错误文案，于是 `Tool 'x' does not exist` 和
// 审批超时被渲染成「✓已完成」。现在收敛到 `utils/toolOutcome.ts`，
// 本文件同时钉住**运行态与恢复态给出同一个答案**——这是本次重构
// 的硬要求：同一个 tool_complete 事件，在 `groupTimeline`（运行态）
// 和 `expandTimelineToMessages`（恢复态）里必须落进同一个状态桶。
import { describe, it, expect } from "vitest";
import { classifyToolOutcome, detectToolError } from "./toolOutcome";
import { groupTimeline, expandTimelineToMessages } from "../components/messageParser";
import { groupEvents } from "../components/ToolEventCard";
import { wireArrayToChatMessages } from "../protocolAdapter";
import type { ChatMessage, ExecutionTimelineEvent } from "../types";

function ev(over: Partial<ExecutionTimelineEvent>): ExecutionTimelineEvent {
  return {
    type: "tool_start",
    tool: "tool_search",
    created_at: "2026-10-01T00:00:00.000Z",
    ...over,
  } as ExecutionTimelineEvent;
}

const START = ev({ type: "tool_start", tool: "tool_search", arguments: '{"q":"cua"}' });

/**
 * 一次「开始 → 完成」的两事件时间线。
 *
 * 两条事件必须带**同一个 tool 名**：`groupTimeline` 用
 * `tool_call_id ?? tool` 做配对键，名字不同会被当成两次调用，
 * start 那一组永远等不到 complete，被断流逻辑（stranded pass）
 * 判成 error —— 那是正确行为，不是我们要测的东西。
 */
function call(content: string, isError?: boolean): ExecutionTimelineEvent[] {
  return [
    START,
    ev({
      type: "tool_complete",
      tool: "tool_search",
      content,
      is_error: isError,
      created_at: "2026-10-01T00:00:01.000Z",
    }),
  ];
}

describe("classifyToolOutcome — 正常完成", () => {
  it("plain output is a completion, not a failure", () => {
    expect(classifyToolOutcome("3 processes found").kind).toBeNull();
  });

  it("is_error:false short-circuits text inspection", () => {
    // 输出恰好以 "Error" 开头，但 server 说了成功 —— 以 server 为准，
    // 否则正常的错误列表会被误报成失败。
    expect(classifyToolOutcome("Error: none", false).kind).toBeNull();
  });

  it("empty / missing content is a completion", () => {
    expect(classifyToolOutcome(undefined).kind).toBeNull();
    expect(classifyToolOutcome("   ").kind).toBeNull();
  });
});

describe("classifyToolOutcome — 失败（本次修的真实缺陷）", () => {
  it("agent runtime 的「工具不存在」判为失败", () => {
    // 这条文案三种旧实现全都不认，UI 打「✓已完成」。
    const c = classifyToolOutcome(
      "Tool 'tool_search' does not exist. Available tools: browser_snapshot, patch, clarify",
    );
    expect(c.kind).toBe("error");
    expect(c.reason).toContain("does not exist");
  });

  it("参数非法判为失败", () => {
    const c = classifyToolOutcome("Invalid tool parameters: Missing 'command' parameter");
    expect(c.kind).toBe("error");
    expect(c.reason).toContain("Missing 'command'");
  });

  it("既有形态仍然有效：Failed / Error / ERR 前缀", () => {
    expect(classifyToolOutcome("Failed to read file").kind).toBe("error");
    expect(classifyToolOutcome("ERR no such host").kind).toBe("error");
  });

  it("JSON 包装的 exit_code ≠ 0", () => {
    const c = classifyToolOutcome('{"exit_code":1,"stderr":"boom"}');
    expect(c.kind).toBe("error");
    expect(c.reason).toBe("boom");
    expect(classifyToolOutcome('{"exit_code":3}').kind).toBe("error");
  });

  it("is_error:true 直接判失败", () => {
    expect(classifyToolOutcome("anything at all", true).kind).toBe("error");
  });
});

describe("classifyToolOutcome — 跳过（新增第四态）", () => {
  it("审批超时判为跳过，不是完成", () => {
    // 2026-10-01 实机截图：这一条显示的是「✓已完成」。
    const c = classifyToolOutcome(
      "The 600-second approval window expired before a decision was made, so the tool was not executed",
    );
    expect(c.kind).toBe("skipped");
  });

  it("用户拒绝判为跳过", () => {
    expect(classifyToolOutcome("Permission denied by user").kind).toBe("skipped");
    expect(classifyToolOutcome("The user rejected this action").kind).toBe("skipped");
  });

  it("裸的 not executed 需要审批语境佐证，不误伤正常输出", () => {
    // 同样的短语，缺审批语境 → 不判跳过，也不判失败，就是普通输出。
    expect(
      classifyToolOutcome("2 steps were not executed because they were skipped in config").kind,
    ).toBeNull();
  });

  it("不受 is_error:false 门控 —— 文本说没执行就不能报完成", () => {
    // 宁可显示「跳过」也不显示「完成」。这是本次修复的核心不对称。
    const c = classifyToolOutcome(
      "approval window expired; the tool was not executed",
      false,
    );
    expect(c.kind).toBe("skipped");
  });
});

describe("运行态与恢复态一致（本次重构的硬要求）", () => {
  const cases: Array<[string, string, "error" | "skipped" | "done"]> = [
    ["工具不存在", "Tool 'tool_search' does not exist. Available tools: patch", "error"],
    ["审批超时", "The approval window expired, so the tool was not executed", "skipped"],
    ["参数非法", "Invalid tool parameters: Missing 'command' parameter", "error"],
    ["正常输出", "3 processes found", "done"],
  ];

  for (const [name, content, expected] of cases) {
    it(`groupTimeline / groupEvents / 恢复态对「${name}」判定一致`, () => {
      const timeline = call(content);

      // 1) 运行态：ProcessTimeline → groupTimeline
      const live = groupTimeline(timeline, { streamComplete: true });
      expect(live[0]?.status).toBe(expected);

      // 2) ACUI 时间线：ToolEventCard.groupEvents
      const acui = groupEvents(timeline);
      expect(acui[0]?.status).toBe(expected);

      // 3) 恢复态：expandTimelineToMessages 展开出的 role:"tool" 行
      const assistant: ChatMessage = {
        id: "m1",
        role: "assistant",
        content: "",
        timestamp: "2026-10-01T00:00:00.000Z",
        execution_timeline: timeline,
      } as ChatMessage;
      const expanded = expandTimelineToMessages([assistant]);
      const toolRow = expanded.find((m) => m.role === "tool");
      expect(toolRow?.tool?.status).toBe(expected);

      // 4) 恢复态另一入口：protocolAdapter 把 `role:"tool"` 行并回
      //    assistant 的 execution_timeline —— 注意它**不发**独立的
      //    tool 消息，所以要从时间线上重新 groupTimeline 一次。
      const projected = wireArrayToChatMessages(
        [
          { id: "a1", role: "assistant", content: "", created_at: "2026-10-01T00:00:00.000Z" },
          { id: "t1", role: "tool", content, tool_name: "tool_search", created_at: "2026-10-01T00:00:01.000Z" },
        ],
        "s1",
        detectToolError,
      );
      const assistantRow = projected.find((m) => m.role === "assistant");
      const reloaded = groupTimeline(assistantRow?.execution_timeline ?? [], {
        streamComplete: true,
      });
      expect(reloaded[0]?.status).toBe(expected);
    });
  }

  it("恢复态不把 skipped 降级成 error（两者都非「完成」，但语义不同）", () => {
    const assistant: ChatMessage = {
      id: "m2",
      role: "assistant",
      content: "",
      timestamp: "2026-10-01T00:00:00.000Z",
      execution_timeline: call("The approval window expired, so the tool was not executed"),
    } as ChatMessage;
    const toolRow = expandTimelineToMessages([assistant]).find((m) => m.role === "tool");
    expect(toolRow?.tool?.status).toBe("skipped");
    // 原因必须带过去，否则恢复后只看到一个没有解释的 ⊘
    expect(toolRow?.tool?.errorReason).toContain("approval window expired");
  });

  it("流式进行中的组不预告结局", () => {
    // 只有 tool_start、没有 complete：不能因为「还没定论」就报失败。
    const groups = groupTimeline([START], { streamComplete: false });
    expect(groups[0]?.status).toBe("running");
  });
});
