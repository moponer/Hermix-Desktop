// 会话级遥测条的回归测试。
//
// 这个条上每个数字都必须**有真实来源**。它取代了原先塞在折叠头里的
// 会话级统计，代价是它出现在每一屏的顶部 —— 一个编出来的数字会被看
// 很多次，比不显示糟得多。所以这里钉的是「什么时候**不**显示」。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { SessionTelemetry, formatElapsed } from "./SessionTelemetry";

afterEach(() => vi.useRealTimers());

const START = new Date("2026-10-01T00:00:00.000Z").getTime();
const iso = (ms: number) => new Date(ms).toISOString();

describe("formatElapsed", () => {
  it("按参考界面的粒度输出：小时 / 分+秒 / 秒", () => {
    expect(formatElapsed(3_888_000)).toBe("1 小时 4 分 48 秒");
    expect(formatElapsed(717_000)).toBe("11 分 57 秒");
    expect(formatElapsed(42_000)).toBe("42 秒");
  });

  it("0 与非法输入不崩", () => {
    expect(formatElapsed(0)).toBe("0 秒");
    expect(formatElapsed(-5)).toBe("0 秒");
    expect(formatElapsed(NaN)).toBe("0 秒");
  });
});

describe("SessionTelemetry — 耗时有真实来源", () => {
  it("按会话创建时间算已执行时长", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 717_000);
    render(<SessionTelemetry sessionStartedAt={iso(START)} />);
    expect(screen.getByText(/已执行 11 分 57 秒/)).toBeInTheDocument();
  });

  it("没有 sessionStartedAt 就不渲染整条（不能拿现在当起点编一个 0 秒）", () => {
    const { container } = render(<SessionTelemetry sessionStartedAt={undefined} />);
    expect(container.firstChild).toBeNull();
  });
});

describe("SessionTelemetry — token/s 宁可不显示", () => {
  const streamProps = {
    sessionStartedAt: iso(START),
    isStreaming: true,
    turnStartedAt: START,
  };

  it("有分子有分母且跑满 1 秒 → 显示", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 10_000);
    render(<SessionTelemetry {...streamProps} completionTokens={1660} />);
    expect(screen.getByText("166 token/s")).toBeInTheDocument();
  });

  it("空闲时不显示速率 —— 那是陈旧数字，不是当前速率", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 10_000);
    render(
      <SessionTelemetry
        sessionStartedAt={iso(START)}
        isStreaming={false}
        completionTokens={1660}
        turnStartedAt={START}
      />,
    );
    expect(screen.queryByText(/token\/s/)).toBeNull();
  });

  it("没有分母（turnStartedAt 缺）→ 不显示", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 10_000);
    render(
      <SessionTelemetry
        sessionStartedAt={iso(START)}
        isStreaming
        completionTokens={1660}
        turnStartedAt={null}
      />,
    );
    expect(screen.queryByText(/token\/s/)).toBeNull();
  });

  it("分子为 0（还没吐字）→ 不显示 0 token/s", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 10_000);
    render(<SessionTelemetry {...streamProps} completionTokens={0} />);
    expect(screen.queryByText(/token\/s/)).toBeNull();
  });

  it("开跑不到 1 秒 → 不显示（0.2 秒分母算出的速率是噪声）", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 200);
    render(<SessionTelemetry {...streamProps} completionTokens={200} />);
    expect(screen.queryByText(/token\/s/)).toBeNull();
  });

  it("没有 usage 事件 → 不显示", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 30_000);
    render(<SessionTelemetry {...streamProps} completionTokens={undefined} />);
    expect(screen.queryByText(/token\/s/)).toBeNull();
  });
});

describe("SessionTelemetry — 流式期间自己走", () => {
  it("每秒重算一次，秒数会自己涨", () => {
    vi.useFakeTimers();
    vi.setSystemTime(START + 60_000);
    const { rerender } = render(<SessionTelemetry sessionStartedAt={iso(START)} isStreaming />);
    expect(screen.getByText(/已执行 1 分 0 秒/)).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1000);
    });
    rerender(<SessionTelemetry sessionStartedAt={iso(START)} isStreaming />);
    expect(screen.getByText(/已执行 1 分 1 秒/)).toBeInTheDocument();
  });
});
