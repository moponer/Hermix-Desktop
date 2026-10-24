// 应用右键菜单（ui/ContextMenu）的行为契约。
//
// 这条链路的价值全在"替代浏览器的网页菜单"上：菜单项按右键目标给出、
// 键盘能走、Esc 能退。测试守住这三点，避免以后有人把菜单改成浏览器的
// 原生菜单（或者把它降级成只有鼠标能用）。
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ContextMenu } from "./ContextMenu";

const items = [
  { id: "copy", label: "复制", onSelect: vi.fn() },
  { id: "paste", label: "粘贴", disabled: true, onSelect: vi.fn() },
  { id: "select-all", label: "全选", separator: true, onSelect: vi.fn() },
];

describe("ContextMenu", () => {
  it("渲染菜单项，禁用项不可点", () => {
    render(<ContextMenu x={10} y={20} items={items} onClose={() => {}} />);

    expect(screen.getByText("复制")).toBeTruthy();
    expect(screen.getByText("粘贴")).toBeTruthy();
    const paste = screen.getByText("粘贴").closest("button");
    expect(paste?.disabled).toBe(true);
  });

  it("点菜单项 → 执行该项并关闭菜单", () => {
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(
      <ContextMenu
        x={0}
        y={0}
        items={[{ id: "copy", label: "复制", onSelect }]}
        onClose={onClose}
      />,
    );

    fireEvent.click(screen.getByText("复制"));
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("Esc 关闭菜单（不执行任何菜单项）", () => {
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(
      <ContextMenu
        x={0}
        y={0}
        items={[{ id: "copy", label: "复制", onSelect }]}
        onClose={onClose}
      />,
    );

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("箭头键在可用项之间移动焦点（跳过禁用项）", () => {
    render(<ContextMenu x={0} y={0} items={items} onClose={() => {}} />);

    fireEvent.keyDown(document, { key: "ArrowDown" });
    const buttons = screen.getAllByRole("menuitem");
    expect(document.activeElement).toBe(buttons[0]);

    // 下一项是禁用的"粘贴" → 焦点应落到"全选"
    fireEvent.keyDown(document, { key: "ArrowDown" });
    expect(document.activeElement).toBe(buttons[2]);
  });
});
