/**
 * FormView 渲染验收（M1.7）— 手写 schema → 表单控件 + 必填校验。
 *
 * 验证字段类型到控件的映射：text→input / enum→select / boolean→checkbox，
 * 以及必填校验拦截空提交。
 */

import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { FormView } from "./FormView";
import type { WorkspaceModule } from "../../types";

function makeTableModule(): Extract<WorkspaceModule, { type: "table" }> {
  return {
    type: "table",
    id: "vehicles",
    name: "车辆台账",
    fields: [
      { id: "plate", name: "车牌号", type: "text", required: true },
      { id: "status", name: "状态", type: "enum", options: ["在用", "维修"] },
      { id: "active", name: "在用", type: "boolean" },
    ],
    views: [],
  };
}

describe("FormView（手写 schema 渲染）", () => {
  it("按字段类型渲染控件", () => {
    const module = makeTableModule();
    render(<FormView module={module} onSubmit={vi.fn()} />);

    // text → input[type=text]
    expect(screen.getByLabelText("车牌号 *")).toBeTruthy();
    // enum → select（含选项）
    const select = screen.getByLabelText("状态") as HTMLSelectElement;
    expect(select).toBeTruthy();
    expect(select.querySelectorAll("option").length).toBe(2);
    // boolean → checkbox
    expect(screen.getByLabelText("在用")).toBeTruthy();
  });

  it("必填字段为空时拦截提交并显示错误", () => {
    const module = makeTableModule();
    const onSubmit = vi.fn();
    render(<FormView module={module} onSubmit={onSubmit} />);

    fireEvent.click(screen.getByText("保存"));
    expect(onSubmit).not.toHaveBeenCalled();
    expect(screen.getByText("车牌号 必填")).toBeTruthy();
  });

  it("填写后提交转换类型（枚举/布尔）", () => {
    const module = makeTableModule();
    const onSubmit = vi.fn();
    render(<FormView module={module} onSubmit={onSubmit} />);

    fireEvent.change(screen.getByLabelText("车牌号 *"), {
      target: { value: "京A12345" },
    });
    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "维修" } });
    fireEvent.click(screen.getByLabelText("在用")); // 勾选布尔
    fireEvent.click(screen.getByText("保存"));

    expect(onSubmit).toHaveBeenCalledWith({
      plate: "京A12345",
      status: "维修",
      active: true,
    });
  });
});
