/**
 * SidebarRevealSlot 测试——H7 从 App.tsx 抽条件渲染前的保护网：
 * 锁死"对话页不渲染 / 侧栏可见不渲染 / 其余页收起才渲染"三条分支，
 * 以及展开与搜索两个点击回调。
 */
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { SidebarRevealSlot } from "./SidebarRevealRow";

function renderSlot(sidebarVisible: boolean, page: string) {
  const onExpand = vi.fn();
  const onOpenSearch = vi.fn();
  const view = render(
    <SidebarRevealSlot
      sidebarVisible={sidebarVisible}
      page={page}
      onExpand={onExpand}
      onOpenSearch={onOpenSearch}
    />,
  );
  return { ...view, onExpand, onOpenSearch };
}

describe("SidebarRevealSlot", () => {
  it("对话页始终不渲染（入口在 ChatView 的 ModelStatusBar 里）", () => {
    const { container } = renderSlot(false, "chat");
    expect(container.firstChild).toBeNull();
  });

  it("侧栏可见时不渲染", () => {
    const { container } = renderSlot(true, "settings");
    expect(container.firstChild).toBeNull();
  });

  it("非对话页且侧栏收起时渲染展开+搜索两个按钮", () => {
    renderSlot(false, "settings");
    expect(screen.getByRole("button", { name: "展开侧边栏" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "搜索对话" })).toBeInTheDocument();
  });

  it("点击展开触发 onExpand", () => {
    const { onExpand } = renderSlot(false, "analytics");
    fireEvent.click(screen.getByRole("button", { name: "展开侧边栏" }));
    expect(onExpand).toHaveBeenCalledTimes(1);
  });

  it("点击搜索触发 onOpenSearch", () => {
    const { onOpenSearch } = renderSlot(false, "analytics");
    fireEvent.click(screen.getByRole("button", { name: "搜索对话" }));
    expect(onOpenSearch).toHaveBeenCalledTimes(1);
  });
});
