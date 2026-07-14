/**
 * CompactionStatusBanner tests — 2026-07-14.
 *
 * Locks the 5-state UI so future refactors of the banner can't silently
 * drop a state. Each test renders with a different `CompactionState` and
 * asserts (a) the banner actually shows, (b) the title contains the
 * expected substring, (c) the optional "used / threshold" footer line
 * appears when both `usedText` and `thresholdText` are populated.
 *
 * Why this matters: Phase 5 reconnects the `compaction_status` WS event
 * that has been a Day-1 placeholder since 2026-06-29. Once it actually
 * fires, the banner transitions through closed → pressure → fallback
 * → open during long conversations, and a silent state-drop would leave
 * the user staring at a stale banner.
 */
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import {
  CompactionStatusBanner,
  type CompactionState,
  type CompactionStatus,
} from "./CompactionStatusBanner";

function renderBanner(state: CompactionState, overrides: Partial<CompactionStatus> = {}) {
  return render(
    <CompactionStatusBanner
      status={{
        state,
        usedText: "120k",
        thresholdText: "160k",
        unit: "tokens",
        percent: 75,
        ...overrides,
      }}
    />,
  );
}

describe("CompactionStatusBanner", () => {
  it("renders nothing when status is null", () => {
    const { container } = render(<CompactionStatusBanner status={null} />);
    expect(container.firstChild).toBeNull();
  });

  it("closed state shows '正常' status", () => {
    renderBanner("closed");
    expect(screen.getByText(/上下文自动压缩:正常/)).toBeInTheDocument();
  });

  it("half_open state shows '恢复探测中' status", () => {
    renderBanner("half_open");
    expect(screen.getByText(/上下文自动压缩:恢复探测中/)).toBeInTheDocument();
  });

  it("open state shows '已熔断' status", () => {
    renderBanner("open");
    expect(screen.getByText(/上下文自动压缩:已熔断/)).toBeInTheDocument();
  });

  it("fallback state shows '轻量裁剪' status", () => {
    renderBanner("fallback");
    expect(screen.getByText(/上下文自动压缩:已切换轻量裁剪/)).toBeInTheDocument();
  });

  it("pressure state shows '使用率偏高' warning", () => {
    renderBanner("pressure");
    expect(screen.getByText(/上下文使用率偏高/)).toBeInTheDocument();
  });

  it("renders used/threshold footer when both are populated", () => {
    renderBanner("pressure");
    expect(screen.getByText(/当前用量:120k \/ 160k tokens/)).toBeInTheDocument();
  });

  it("hides footer when usedText or thresholdText is missing", () => {
    renderBanner("closed", { usedText: undefined, thresholdText: undefined });
    expect(screen.queryByText(/当前用量:/)).not.toBeInTheDocument();
  });

  it("surfaces consecutive failure count when state involves a breaker", () => {
    renderBanner("open", { consecutiveFailures: 3 });
    expect(screen.getByText(/连续失败 3 次/)).toBeInTheDocument();
  });

  it("surfaces last failure reason when provided", () => {
    renderBanner("open", { lastFailureReason: "LLM rate limit" });
    expect(screen.getByText(/原因:LLM rate limit/)).toBeInTheDocument();
  });

  it("defaults unit to 'tokens' when omitted (back-compat)", () => {
    renderBanner("closed", { unit: undefined });
    expect(screen.getByText(/当前用量:120k \/ 160k tokens/)).toBeInTheDocument();
  });
});