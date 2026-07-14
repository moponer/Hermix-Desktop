import { describe, it, expect } from "vitest";
import { extractMemoryUpdates } from "./messageParser";
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

function recallEvent(over: Partial<ExecutionTimelineEvent>): ExecutionTimelineEvent {
  return {
    type: "memory_recall",
    tool: "memory_recall",
    created_at: new Date().toISOString(),
    ...over,
  } as ExecutionTimelineEvent;
}

import { extractMemoryRecalls } from "./messageParser";

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
