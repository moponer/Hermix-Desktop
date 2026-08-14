import { describe, it, expect } from "vitest";
import {
  extractMemoryUpdates,
  extractMemoryRecalls,
  groupTimeline,
  parseContent,
} from "./messageParser";
import { groupEvents } from "./ToolEventCard";
import type { ExecutionTimelineEvent } from "../types";

function memEvent(over: Partial<ExecutionTimelineEvent>): ExecutionTimelineEvent {
  return {
    type: "memory_update",
    tool: "memory_update",
    created_at: new Date().toISOString(),
    ...over,
  } as ExecutionTimelineEvent;
}

describe("extractMemoryUpdates", () => {
  it("returns [] when there are no events", () => {
    expect(extractMemoryUpdates(undefined)).toEqual([]);
    expect(extractMemoryUpdates([])).toEqual([]);
  });

  it("collects memory_update events and maps fields", () => {
    const events = [
      memEvent({
        memory_action: "write",
        memory_type: "fact",
        memory_key: "user.style:direct",
        memory_summary: "记住 1 条事实/偏好：user.style:direct",
        memory_count: 1,
      }),
      memEvent({
        memory_action: "delete",
        memory_type: "tool",
        memory_key: "old_note",
        memory_summary: "记忆 delete「old_note」：过时结论",
      }),
    ];
    const out = extractMemoryUpdates(events);
    expect(out).toHaveLength(2);
    expect(out[0].action).toBe("write");
    expect(out[0].memoryType).toBe("fact");
    expect(out[0].count).toBe(1);
    expect(out[1].action).toBe("delete");
    expect(out[1].key).toBe("old_note");
  });

  it("ignores non-memory_update events", () => {
    const events = [
      memEvent({ type: "tool_complete" }),
      { type: "file_change", tool: "file_change", created_at: new Date().toISOString() } as ExecutionTimelineEvent,
    ];
    expect(extractMemoryUpdates(events)).toEqual([]);
  });

  it("de-dupes by (action, key)", () => {
    const events = [
      memEvent({ memory_action: "write", memory_key: "k1", memory_summary: "a" }),
      memEvent({ memory_action: "write", memory_key: "k1", memory_summary: "b" }),
      memEvent({ memory_action: "write", memory_key: "k2", memory_summary: "c" }),
    ];
    const out = extractMemoryUpdates(events);
    expect(out).toHaveLength(2);
    expect(out.map((u) => u.key)).toEqual(["k1", "k2"]);
  });

  it("skips entries with no summary and no key", () => {
    const events = [memEvent({ memory_summary: "", memory_key: "" })];
    expect(extractMemoryUpdates(events)).toEqual([]);
  });
});

describe("parseContent", () => {
  it("keeps ordinary markdown checklists in the answer body", () => {
    const parsed = parseContent(
      "分析结果：\n\n- [ ] 检查数据库\n- [ ] 核对统计数据\n\n这些只是说明中的检查项。",
    );
    expect(parsed.tasks).toEqual([]);
    expect(parsed.body).toContain("- [ ] 检查数据库");
    expect(parsed.body).toContain("- [ ] 核对统计数据");
  });

  it("does not infer a task plan from numbered or bulleted prose", () => {
    const parsed = parseContent("1. 先定位问题\n2. 再核对数据\n3. 最后给出结论");
    expect(parsed.tasks).toEqual([]);
    expect(parsed.body).toContain("1. 先定位问题");
  });
});

function recallEvent(over: Partial<ExecutionTimelineEvent>): ExecutionTimelineEvent {
  return {
    type: "memory_recall",
    tool: "memory_recall",
    created_at: new Date().toISOString(),
    ...over,
  } as ExecutionTimelineEvent;
}

describe("extractMemoryRecalls", () => {
  it("returns [] when there are no events", () => {
    expect(extractMemoryRecalls(undefined)).toEqual([]);
    expect(extractMemoryRecalls([])).toEqual([]);
  });

  it("skips zero-count recalls (server-side guard)", () => {
    const events = [
      recallEvent({
        memory_recall_trigger: "interval",
        memory_recall_count: 0,
        memory_recall_turn_number: 5,
      }),
    ];
    expect(extractMemoryRecalls(events)).toEqual([]);
  });

  it("collects recall events and maps fields", () => {
    const events = [
      recallEvent({
        memory_recall_trigger: "interval",
        memory_recall_count: 3,
        memory_recall_total_chars: 1200,
        memory_recall_preview_keys: ["fact:owner_rust", "fact:user_style", "fact:no_blue"],
        memory_recall_turn_number: 5,
      }),
    ];
    const out = extractMemoryRecalls(events);
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({
      trigger: "interval",
      count: 3,
      totalChars: 1200,
      turnNumber: 5,
    });
    expect(out[0].previewKeys).toEqual([
      "fact:owner_rust",
      "fact:user_style",
      "fact:no_blue",
    ]);
  });

  it("dedupes by (trigger, turnNumber)", () => {
    const events = [
      recallEvent({
        memory_recall_trigger: "drift",
        memory_recall_count: 2,
        memory_recall_turn_number: 4,
      }),
      recallEvent({
        memory_recall_trigger: "drift",
        memory_recall_count: 2,
        memory_recall_turn_number: 4,
      }),
    ];
    expect(extractMemoryRecalls(events)).toHaveLength(1);
  });

  it("keeps recalls across different turns", () => {
    const events = [
      recallEvent({
        memory_recall_trigger: "first_turn",
        memory_recall_count: 5,
        memory_recall_turn_number: 1,
      }),
      recallEvent({
        memory_recall_trigger: "interval",
        memory_recall_count: 5,
        memory_recall_turn_number: 5,
      }),
    ];
    expect(extractMemoryRecalls(events)).toHaveLength(2);
  });

  it("ignores non-recall events", () => {
    const events = [
      memEvent({ memory_key: "x", memory_summary: "y" }),
      recallEvent({
        memory_recall_trigger: "interval",
        memory_recall_count: 1,
        memory_recall_turn_number: 1,
      }),
    ];
    expect(extractMemoryRecalls(events)).toHaveLength(1);
  });
});

function toolEvent(over: Partial<ExecutionTimelineEvent>): ExecutionTimelineEvent {
  return {
    type: "tool_start",
    tool: "read_file",
    created_at: new Date().toISOString(),
    ...over,
  } as ExecutionTimelineEvent;
}

describe("groupTimeline stranded-pass (FIX)", () => {
  it("keeps stranded groups as `running` while the turn is still streaming", () => {
    // Tool `tool_start` arrives, no `tool_complete` yet. Live WS is
    // still feeding events. The renderer should keep showing the
    // "运行中" badge + spinner, not jump to "已中断".
    const events = [toolEvent({ type: "tool_start", tool: "read_file" })];
    const groups = groupTimeline(events, { streamComplete: false });
    expect(groups).toHaveLength(1);
    expect(groups[0].status).toBe("running");
    expect(groups[0].statusNote ?? "").not.toMatch(/执行已中断/);
  });

  it("promotes stranded groups to `error` once the turn has settled (default)", () => {
    // Same events, but the parent turn has finished streaming. This
    // is the legacy / reload path: stranded = genuinely interrupted.
    const events = [toolEvent({ type: "tool_start", tool: "read_file" })];
    const groups = groupTimeline(events);
    expect(groups).toHaveLength(1);
    expect(groups[0].status).toBe("error");
    expect(groups[0].statusNote).toBe("执行已中断（无完成事件）");
  });

  it("uses streamComplete=true when called without opts (backward compat)", () => {
    const events = [toolEvent({ type: "tool_start", tool: "read_file" })];
    const groups = groupTimeline(events);
    // Same as the explicit-true case above — defaults preserve
    // the historical behaviour for `expandTimelineToMessages`.
    expect(groups[0].status).toBe("error");
  });

  it("does not touch a group that already received tool_complete, even during streaming", () => {
    const ts = new Date().toISOString();
    const events = [
      toolEvent({ type: "tool_start", tool: "read_file", created_at: ts }),
      toolEvent({ type: "tool_complete", tool: "read_file", created_at: ts }),
    ];
    const groups = groupTimeline(events, { streamComplete: false });
    expect(groups).toHaveLength(1);
    expect(groups[0].status).toBe("done");
  });
});

describe("groupEvents stranded-pass (FIX, mirrors groupTimeline)", () => {
  // ACUI tool timeline renders via `groupEvents` (not `groupTimeline`).
  // The bug surface was: a `delegate_task` tool_start landed before its
  // real `tool_complete` arrived, and the ACUI row immediately flashed
  // "失败 / 执行已中断" — then flipped to "已完成" when the late
  // complete landed. Same fix as `groupTimeline`: skip the stranded
  // pass while the parent turn is still streaming.
  it("keeps stranded delegate_task groups as `running` while the turn is still streaming", () => {
    const events = [toolEvent({ type: "tool_start", tool: "delegate_task" })];
    const groups = groupEvents(events, { streamComplete: false });
    expect(groups).toHaveLength(1);
    expect(groups[0].status).toBe("running");
    expect(groups[0].statusNote ?? "").not.toMatch(/执行已中断/);
  });

  it("promotes stranded groups to `error` once the turn has settled (default)", () => {
    // Reload / settled view: the live WS is gone, so a stranded
    // group is genuinely interrupted and should surface as
    // "执行已中断" — keeps the historical reload behaviour intact.
    const events = [toolEvent({ type: "tool_start", tool: "read_file" })];
    const groups = groupEvents(events);
    expect(groups).toHaveLength(1);
    expect(groups[0].status).toBe("error");
    expect(groups[0].statusNote).toBe("执行已中断");
  });

  it("uses streamComplete=true when called without opts (backward compat)", () => {
    const events = [toolEvent({ type: "tool_start", tool: "read_file" })];
    const groups = groupEvents(events);
    // No opt → defaults to true → stranded pass runs → error.
    expect(groups[0].status).toBe("error");
  });

  it("does not touch a group that already received tool_complete, even during streaming", () => {
    const ts = new Date().toISOString();
    const events = [
      toolEvent({ type: "tool_start", tool: "read_file", created_at: ts }),
      toolEvent({ type: "tool_complete", tool: "read_file", created_at: ts }),
    ];
    const groups = groupEvents(events, { streamComplete: false });
    expect(groups).toHaveLength(1);
    expect(groups[0].status).toBe("done");
  });

  it("keeps a streaming delegate_task running even if it has produced no stdout yet", () => {
    // The pre-fix code only kept stranded groups running if they had
    // already streamed stdout. delegate_task in particular emits
    // nothing on stdout (it just spawns a sub-agent), so this
    // sub-case was the most common source of the false-failure
    // flash. The streamComplete=false path now skips the stranded
    // pass entirely — no stdout / no output is fine while the WS
    // is alive.
    const events = [toolEvent({ type: "tool_start", tool: "delegate_task" })];
    const groups = groupEvents(events, { streamComplete: false });
    expect(groups[0].status).toBe("running");
  });
});
