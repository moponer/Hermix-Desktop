/**
 * TodoListCard 状态渲染测试。
 *
 * 锁死一件事：模型写进 `status` 的四个值必须在界面上各自对应一个可见的
 * 中文标签。此前只有图标（勾/叉/转圈），用户要凑近看形状才知道状态；
 * 而「执行中」和「待执行」的图标在 12px 下几乎分不出来。
 *
 * 注意：状态完全由模型写入，前端不推断。所以这里也不测任何"自动完成"
 * 行为 —— 那种行为不存在。
 */
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { TodoListCard } from "./TodoListCard";
import type { TodoItem } from "../api";

function item(overrides: Partial<TodoItem>): TodoItem {
  return {
    id: "t1",
    title: "task",
    description: null,
    status: "pending",
    priority: "medium",
    created_at: "2026-11-18T00:00:00Z",
    depends_on: [],
    last_result: null,
    execution_count: 0,
    group_id: "g1",
    group_title: null,
    session_id: "s1",
    origin_message_id: null,
    ...overrides,
  } as TodoItem;
}

function renderCard(items: TodoItem[]) {
  const byId: Record<string, TodoItem> = {};
  for (const i of items) byId[i.id] = i;
  return render(
    <TodoListCard
      groupId="g1"
      groupTitle="计划"
      itemsById={byId}
      onPatch={async () => {}}
      sessionId="s1"
    />,
  );
}

describe("TodoListCard 状态标签", () => {
  it("pending 显示「待执行」", () => {
    renderCard([item({ status: "pending" })]);
    expect(screen.getByText("待执行")).toBeTruthy();
  });

  it("in_progress 显示「执行中」", () => {
    renderCard([item({ status: "in_progress" })]);
    expect(screen.getByText("执行中")).toBeTruthy();
  });

  // 终态条目会让卡片默认折叠（全部结案 → 收成一行「✓ N/N」），
  // 这是组件的既有产品行为。所以测终态标签时混一条未完成项，
  // 让卡片保持展开 —— 折叠逻辑本身不在本文件被测范围内。
  it("completed 显示「已完成」", () => {
    renderCard([
      item({ id: "a", status: "completed" }),
      item({ id: "b", status: "pending" }),
    ]);
    expect(screen.getByText("已完成")).toBeTruthy();
  });

  it("failed 显示「已失败」", () => {
    renderCard([
      item({ id: "a", status: "failed" }),
      item({ id: "b", status: "pending" }),
    ]);
    expect(screen.getByText("已失败")).toBeTruthy();
  });

  it("未知状态不渲染标签，但行本身仍显示（前端不改写模型数据）", () => {
    renderCard([item({ status: "something_new" })]);
    expect(screen.getByText("task")).toBeTruthy();
    expect(screen.queryByText("待执行")).toBeNull();
    expect(screen.queryByText("执行中")).toBeNull();
  });

  it("同一卡片内多状态各自独立显示", () => {
    renderCard([
      item({ id: "a", title: "one", status: "completed" }),
      item({ id: "b", title: "two", status: "in_progress" }),
      item({ id: "c", title: "three", status: "pending" }),
    ]);
    expect(screen.getByText("已完成")).toBeTruthy();
    expect(screen.getByText("执行中")).toBeTruthy();
    expect(screen.getByText("待执行")).toBeTruthy();
  });
});
