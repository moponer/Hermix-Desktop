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

  it("prefers maxText denominator so banner matches the progress bar", () => {
    renderBanner("pressure", { maxText: "205k", maxTokens: 205_000 });
    expect(screen.getByText(/当前用量:120k \/ 205k tokens/)).toBeInTheDocument();
  });

  // phase-specific copy overrides the breaker-state copy
  // so the user sees "正在准备压缩" / "正在压缩中" / "压缩完成" /
  // "压缩失败" transitions instead of a single static warning card
  // jumping straight to the post-attempt result.
  describe("phase overrides (2026-07-22)", () => {
    function renderWithPhase(
      phase: import("./CompactionStatusBanner").CompactionPhase,
      overrides: Partial<CompactionStatus> = {},
    ) {
      return render(
        <CompactionStatusBanner
          status={{
            state: "closed",
            usedText: "120k",
            thresholdText: "160k",
            unit: "tokens",
            percent: 75,
            phase,
            ...overrides,
          }}
        />,
      );
    }

    it("starting phase shows '正在准备' with spinner", () => {
      const { container } = renderWithPhase("starting");
      expect(screen.getByText(/上下文压缩:正在准备/)).toBeInTheDocument();
      // Loader2 icon should carry the `animate-spin` class so the
      // mid-attempt phases read as "actively working".
      expect(container.querySelector(".animate-spin")).toBeTruthy();
    });

    it("in_progress_fast phase shows the fast-path copy", () => {
      const { container } = renderWithPhase("in_progress_fast");
      expect(screen.getByText(/上下文压缩:正在压缩中\(快速通道\)/)).toBeInTheDocument();
      expect(container.querySelector(".animate-spin")).toBeTruthy();
    });

    it("in_progress_llm phase shows the LLM-summary copy", () => {
      const { container } = renderWithPhase("in_progress_llm");
      expect(screen.getByText(/上下文压缩:正在压缩中\(LLM 摘要\)/)).toBeInTheDocument();
      expect(container.querySelector(".animate-spin")).toBeTruthy();
    });

    it("complete phase shows the success copy with no spinner", () => {
      const { container } = renderWithPhase("complete");
      expect(screen.getByText(/上下文压缩:已完成/)).toBeInTheDocument();
      expect(container.querySelector(".animate-spin")).toBeFalsy();
    });

    it("failed phase shows the failure copy", () => {
      renderWithPhase("failed");
      expect(screen.getByText(/上下文压缩:本轮失败/)).toBeInTheDocument();
    });

    it("idle phase falls back to the breaker-state copy (back-compat)", () => {
      // Explicit `phase: "idle"` should behave exactly like the
      // pre-phase banner — driven purely by `state`.
      renderWithPhase("idle");
      expect(screen.getByText(/上下文自动压缩:正常/)).toBeInTheDocument();
    });

    it("undefined phase falls back to the breaker-state copy (older snapshots)", () => {
      // Older snapshots from before the phase field was added
      // don't carry `phase` at all; the banner must still render.
      const { container } = render(
        <CompactionStatusBanner
          status={{
            state: "pressure",
            usedText: "120k",
            thresholdText: "160k",
            unit: "tokens",
            percent: 75,
          }}
        />,
      );
      expect(screen.getByText(/上下文使用率偏高/)).toBeInTheDocument();
      expect(container.querySelector(".animate-spin")).toBeFalsy();
    });
  });
});