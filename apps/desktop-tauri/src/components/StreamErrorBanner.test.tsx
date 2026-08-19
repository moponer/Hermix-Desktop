/**
 * StreamErrorCard tests — verify the Linear/Vercel-style surface:
 *   1. the title comes from the kind dictionary;
 *   2. the message and optional tip render in order;
 *   3. the Bootstrap-era "code" badge is gone (no chip element);
 *   4. the dismiss / retry callbacks fire on click;
 *   5. the tone class (amber / amber-strong / amber-soft) is applied
 *      so the rail width can express severity via CSS.
 */
import { describe, it, expect, vi } from "vitest";
import { render, fireEvent, screen } from "@testing-library/react";
import { StreamErrorCard } from "./StreamErrorBanner";

describe("StreamErrorCard", () => {
  it("renders the kind-driven title and message", () => {
    render(
      <StreamErrorCard
        kind="bad_request"
        message="模型不可用或参数错误。"
      />,
    );
    // Title comes from the ERROR_META dictionary for `bad_request`.
    expect(screen.getByText("请求无效")).toBeInTheDocument();
    // The user-supplied message body.
    expect(screen.getByText("模型不可用或参数错误。")).toBeInTheDocument();
  });

  it("renders the kind-driven tip line", () => {
    render(<StreamErrorCard kind="quota" message="..." />);
    // Quota tip mentions "设置中更换 key".
    expect(screen.getByText(/设置中更换 key/)).toBeInTheDocument();
  });

  it("does NOT render the Bootstrap-era 'code' chip", () => {
    const { container } = render(
      <StreamErrorCard kind="bad_request" message="..." />,
    );
    // The old `.stream-error-card__chip` element was the loud
    // monospace UPPERCASE badge — make sure it's gone.
    const chips = container.querySelectorAll(".stream-error-card__chip");
    expect(chips.length).toBe(0);
  });

  it("applies the kind-derived tone so CSS can express severity", () => {
    const { container: strong } = render(
      <StreamErrorCard kind="auth" message="..." />,
    );
    // auth / quota / bad_request are amber-strong (rail 4px).
    expect(
      strong.querySelector(".stream-error-card--amber-strong"),
    ).toBeInTheDocument();

    const { container: soft } = render(
      <StreamErrorCard kind="network" message="..." />,
    );
    // network / timeout are amber-soft (rail 2px).
    expect(
      soft.querySelector(".stream-error-card--amber-soft"),
    ).toBeInTheDocument();
  });

  it("fires onDismiss when the X button is clicked", () => {
    const onDismiss = vi.fn();
    render(
      <StreamErrorCard
        kind="bad_request"
        message="..."
        onDismiss={onDismiss}
      />,
    );
    fireEvent.click(screen.getByLabelText("关闭错误提示"));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("fires onRetry when the retry button is clicked", () => {
    const onRetry = vi.fn();
    render(
      <StreamErrorCard
        kind="bad_request"
        message="..."
        onRetry={onRetry}
      />,
    );
    fireEvent.click(screen.getByText("重试上一条"));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("does NOT render the retry button when onRetry is absent", () => {
    render(<StreamErrorCard kind="bad_request" message="..." />);
    expect(screen.queryByText("重试上一条")).toBeNull();
  });

  it("renders the collapsible 技术详情 block only when detail is set", () => {
    const { rerender, queryByText } = render(
      <StreamErrorCard kind="bad_request" message="..." />,
    );
    expect(queryByText("技术详情")).toBeNull();

    rerender(
      <StreamErrorCard
        kind="bad_request"
        message="..."
        detail="raw backend error"
      />,
    );
    expect(screen.getByText("技术详情")).toBeInTheDocument();
    expect(screen.getByText("raw backend error")).toBeInTheDocument();
  });

  it("falls back to the 'unknown' meta when kind is omitted", () => {
    render(<StreamErrorCard message="..." />);
    expect(screen.getByText("发送失败")).toBeInTheDocument();
  });
});