// MemoryConsolidationCard：对话流「记忆沉淀」卡的行为锚点。
import { describe, expect, it } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MemoryConsolidationCard } from "./ToolCallBubble";
import type { MemoryUpdateEntry } from "./messageParser";

const UPDATES: MemoryUpdateEntry[] = [
  {
    action: "summarize",
    memoryType: "summary",
    key: "session",
    summary: "保存会话总结：USER: 帮我看看记忆系统",
    count: 1,
    timestamp: "2026-10-07T00:00:00Z",
  },
  {
    action: "write",
    memoryType: "promotion",
    key: "memory",
    summary: "晋升 2 条会话记忆到长期记忆",
    count: 2,
    timestamp: "2026-10-07T00:00:01Z",
  },
  {
    action: "write",
    memoryType: "daily",
    key: "daily",
    summary: "已沉淀今日记忆",
    timestamp: "2026-10-07T00:00:02Z",
  },
];

describe("MemoryConsolidationCard", () => {
  it("折叠态显示总数（count 聚合）", () => {
    render(<MemoryConsolidationCard updates={UPDATES} />);
    // 1 + 2 + 1 = 4
    expect(screen.getByText(/4 条已写入记忆库/)).toBeInTheDocument();
    expect(screen.queryByText(/晋升 2 条会话记忆到长期记忆/)).not.toBeInTheDocument();
  });

  it("点击展开后逐条显示明细，再点击收起", () => {
    render(<MemoryConsolidationCard updates={UPDATES} />);
    fireEvent.click(screen.getByRole("button", { name: /记忆沉淀/ }));
    expect(screen.getByText("晋升 2 条会话记忆到长期记忆")).toBeInTheDocument();
    expect(screen.getByText("已沉淀今日记忆")).toBeInTheDocument();
    expect(screen.getByText("保存会话总结：USER: 帮我看看记忆系统")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /记忆沉淀/ }));
    expect(screen.queryByText(/晋升 2 条/)).not.toBeInTheDocument();
  });

  it("无更新时不渲染", () => {
    const { container } = render(<MemoryConsolidationCard updates={[]} />);
    expect(container).toBeEmptyDOMElement();
  });
});
