/**
 * DenialBanner 测试——在从 App.tsx 抽出前后锁死全部行为：
 * 四种 kind 的副文案、工具名展示、整条点击关闭、× 按钮点击只关一次。
 * 照搬 CompactionStatusBanner.test.tsx 的 RTL render+expect 范式（仓库无快照先例）。
 */
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { DenialBanner, denialSubtitle } from "./DenialBanner";

const DENIED = { kind: "user_denied", tool: "run_command" };

describe("denialSubtitle", () => {
  it("user_denied 文案", () => {
    expect(denialSubtitle("user_denied")).toBe("你明确拒绝了该操作，未执行。");
  });
  it("timed_out 文案", () => {
    expect(denialSubtitle("timed_out")).toBe("确认超时（你在限定时间内没有做出决定），未执行。");
  });
  it("channel_closed 文案", () => {
    expect(denialSubtitle("channel_closed")).toBe("连接中断（WS 断开 / 服务端重启 / 会话重载），请求丢失。");
  });
  it("未知 kind 回退文案", () => {
    expect(denialSubtitle("something_else")).toBe("未执行。");
  });
});

describe("DenialBanner", () => {
  it("空 denial 不渲染任何节点", () => {
    const { container } = render(<DenialBanner denial={null} onDismiss={() => {}} />);
    expect(container.firstChild).toBeNull();
  });

  it("展示工具名与标题，且带 status 语义", () => {
    render(<DenialBanner denial={DENIED} onDismiss={() => {}} />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
    expect(screen.getByText("run_command").tagName).toBe("CODE");
    expect(screen.getByText(/的调用被拒绝/)).toBeInTheDocument();
    expect(screen.getByText(/你明确拒绝了该操作/)).toBeInTheDocument();
  });

  it("点击整条横幅触发 onDismiss", () => {
    const onDismiss = vi.fn();
    render(<DenialBanner denial={DENIED} onDismiss={onDismiss} />);
    fireEvent.click(screen.getByRole("status"));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("点击 × 只关一次（stopPropagation 不冒泡到整条）", () => {
    const onDismiss = vi.fn();
    render(<DenialBanner denial={DENIED} onDismiss={onDismiss} />);
    fireEvent.click(screen.getByRole("button"));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("每种 kind 的子文案都渲染进 DOM", () => {
    for (const kind of ["timed_out", "channel_closed"]) {
      const { unmount } = render(<DenialBanner denial={{ kind, tool: "write_file" }} onDismiss={() => {}} />);
      expect(screen.getByText(denialSubtitle(kind))).toBeInTheDocument();
      unmount();
    }
  });
});
