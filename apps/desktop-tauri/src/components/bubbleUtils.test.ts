// `formatDuration` 的回归测试。
//
// 2026-10-01 真实事故：这个函数原先**只进位到分钟**，于是 6 天被渲染成
// 「8719m 55s」—— 一眼看过去像个正常数字，实际是个 bug 的产物。耗时是
// 轮次尺度的量（自治任务几小时很常见），不进位到小时就没有量级。
import { describe, it, expect } from "vitest";
import { formatDuration } from "./bubbleUtils";

describe("formatDuration", () => {
  it("测不出来就不显示（空串，不是 0）", () => {
    expect(formatDuration(undefined)).toBe("");
    expect(formatDuration(0)).toBe("");
    expect(formatDuration(-5)).toBe("");
    expect(formatDuration(NaN)).toBe("");
  });

  it("秒档：四舍五入到 60s 时进位成 1m，不显示「60s」", () => {
    expect(formatDuration(4_000)).toBe("4s");
    expect(formatDuration(29_400)).toBe("29s");
    expect(formatDuration(59_400)).toBe("59s");
    expect(formatDuration(59_900)).toBe("1m");
  });

  it("分钟档：整分不带秒尾巴", () => {
    expect(formatDuration(120_000)).toBe("2m");
    expect(formatDuration(252_000)).toBe("4m 12s");
  });

  it("小时档：秒在小时档是噪声，整小时省掉分钟位", () => {
    expect(formatDuration(3_600_000)).toBe("1h");
    expect(formatDuration(11_880_000)).toBe("3h 18m");
  });

  it("天档：绝不再出现「8719m」这种量级", () => {
    expect(formatDuration(86_400_000)).toBe("1d");
    expect(formatDuration(523_195_000)).toBe("6d 1h");
  });

  it("事故回归：6 天的跨度不能渲染成分钟数", () => {
    // 事故输入（库里 id=151360 那条消息的真实数值）
    expect(formatDuration(523_195_000)).not.toMatch(/^\d+m/);
  });
});
