/**
 * ToolRunNote tests — 工具轮的正文占位。
 *
 * 起因：一轮只调工具、没写正文时正文区整段空白（实测一次 30 轮请求里
 * 23 轮如此），读起来像卡住。占位把 execution_timeline 已有的事实搬到
 * 正文位置。断言四件事：
 *   1. 计数与工具名来自 `tool_start` 事件，重复工具只列一次；
 *   2. 流式态说"本轮已调用"，结束态说"本轮调用了"并标注"无文字结论"；
 *   3. 没有工具事件时不渲染任何东西（不能凭空造一行）；
 *   4. 旧数据只有 `tool_calls` 时退回按它计数。
 */
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { ToolRunNote } from "./ToolCallBubble";
import type { TimelineStep } from "../types";

function toolStep(tools: string[]): TimelineStep {
  return {
    kind: "tools",
    events: tools.map((tool, i) => ({
      type: "tool_start" as const,
      tool,
      created_at: new Date(1_700_000_000_000 + i * 1000).toISOString(),
    })),
  };
}

describe("ToolRunNote", () => {
  it("counts calls and lists distinct tool names", () => {
    render(
      <ToolRunNote
        steps={[toolStep(["read_file", "read_file", "grep"])]}
        streaming={false}
      />,
    );
    expect(screen.getByText(/本轮调用了 3 个工具/)).toBeInTheDocument();
    // Distinct names only — "read_file" appears once even though it ran twice.
    expect(screen.getByText("read_file、grep")).toBeInTheDocument();
    expect(screen.getByText("无文字结论")).toBeInTheDocument();
  });

  it("uses the live wording while the turn is still streaming", () => {
    render(<ToolRunNote steps={[toolStep(["terminal"])]} streaming />);
    expect(screen.getByText(/本轮已调用 1 个工具/)).toBeInTheDocument();
    // A live turn is not "done with no conclusion" — that verdict belongs
    // to the settled message only.
    expect(screen.queryByText("无文字结论")).toBeNull();
  });

  it("renders nothing when the turn called no tools", () => {
    const { container } = render(<ToolRunNote steps={[]} streaming={false} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("falls back to legacy tool_calls when no timeline events exist", () => {
    render(
      <ToolRunNote
        steps={[]}
        toolCalls={[{ name: "list_files" }, { name: "list_files" }]}
        streaming={false}
      />,
    );
    expect(screen.getByText(/本轮调用了 2 个工具/)).toBeInTheDocument();
    expect(screen.getByText("list_files")).toBeInTheDocument();
  });
});
