/**
 * TableView 渲染验收（M1.7）— 手写 schema → 渲染表头与行数据。
 *
 * 镜像后端 `WorkspaceSchema` 的手写 JSON，验证 schema 驱动渲染链路：
 * 字段定义 → 列头（展示名）→ 行数据（类型格式化）。
 */

import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { TableView } from "./TableView";
import type { WorkspaceModule } from "../../types";

function makeTableModule(): Extract<WorkspaceModule, { type: "table" }> {
  return {
    type: "table",
    id: "vehicles",
    name: "车辆台账",
    fields: [
      { id: "plate", name: "车牌号", type: "text", required: true, unique: true },
      { id: "status", name: "状态", type: "enum", options: ["在用", "维修", "停用"] },
      { id: "mileage", name: "里程", type: "number" },
      { id: "active", name: "启用", type: "boolean" },
    ],
    views: [{ id: "list", name: "列表", type: "grid", fields: ["plate", "status", "mileage", "active"] }],
  };
}

describe("TableView（手写 schema 渲染）", () => {
  it("按 schema 字段渲染列头与行数据", () => {
    const module = makeTableModule();
    const rows = [
      { plate: "京A12345", status: "在用", mileage: 12000, active: true },
      { plate: "京B67890", status: "维修", mileage: 8000, active: false },
    ];
    render(<TableView module={module} rows={rows} />);

    // 列头 = 字段展示名
    expect(screen.getByText("车牌号")).toBeTruthy();
    expect(screen.getByText("状态")).toBeTruthy();
    expect(screen.getByText("里程")).toBeTruthy();
    expect(screen.getByText("启用")).toBeTruthy();

    // 行数据
    expect(screen.getByText("京A12345")).toBeTruthy();
    expect(screen.getByText("京B67890")).toBeTruthy();
    // 状态列值「在用」只出现一次（列头是「启用」，不冲突）
    expect(screen.getAllByText("在用").length).toBe(1);
  });

  it("行数统计展示在工具条", () => {
    const module = makeTableModule();
    render(<TableView module={module} rows={[{ plate: "京A1", status: "在用" }]} />);
    expect(screen.getByText("1 行")).toBeTruthy();
  });

  it("空字段表显示提示", () => {
    const module: Extract<WorkspaceModule, { type: "table" }> = {
      type: "table",
      id: "empty",
      name: "空表",
      fields: [],
      views: [],
    };
    render(<TableView module={module} rows={[]} />);
    expect(screen.getByText("该表还没有字段")).toBeTruthy();
  });

  it("空行显示占位符 —", () => {
    const module = makeTableModule();
    render(
      <TableView
        module={module}
        rows={[{ plate: "京A1", status: null, mileage: null, active: null }]}
      />,
    );
    // 每个 null 单元格显示 "—"
    expect(screen.getAllByText("—").length).toBeGreaterThanOrEqual(3);
  });
});
