// 工作日志行的清洗。
//
// 真实缺陷（2026-10-28 值守页截图）：左翼「最近工作日志」每行都是
// `- 14:32 [automati] USER: 请查一下今天…` —— 落库格式被原样搬上屏，
// 行首日期列之外又出现时刻，`[automati]` 是 `automation:…` 截断 8 字符的
// 产物。这里钉住剥前缀的行为。
import { describe, expect, it } from "vitest";
import { dailyLine, pad2, relativeCompact } from "./readout";

describe("dailyLine", () => {
  it("剥掉项目符号、时刻与会话 id 前缀", () => {
    expect(dailyLine("- 14:32 [automati] USER: 把昨天那份周报转成 PDF 发我")).toBe(
      "把昨天那份周报转成 PDF 发我",
    );
  });

  it("剥掉 twin / member 会话前缀与中文角色词", () => {
    expect(dailyLine("- 09:02 [twin:ali] 继续超星学习通")).toBe("继续超星学习通");
    expect(dailyLine("- 09:02 [member::3] 用户：对齐依赖版本")).toBe("对齐依赖版本");
  });

  it("只取首行非空，且不带前导空行", () => {
    expect(dailyLine("\n\n- 10:59 [automati] USER: 第一行\n第二行")).toBe("第一行");
  });

  it("自由文本原样保留（LLM 用 daily_memory 工具写的句子）", () => {
    expect(dailyLine("今天把三个仓库的依赖对齐了")).toBe("今天把三个仓库的依赖对齐了");
  });

  it("剥完为空时退回原文，不出空行", () => {
    expect(dailyLine("USER: ")).toBe("USER:");
  });

  it("超长裁到 max 并加省略号", () => {
    const out = dailyLine("x".repeat(200), 20);
    expect(out).toHaveLength(20);
    expect(out.endsWith("…")).toBe(true);
  });

  it("空输入不报错", () => {
    expect(dailyLine(undefined)).toBe("");
    expect(dailyLine("   ")).toBe("");
  });
});

describe("读数格式化", () => {
  it("pad2 定宽两位", () => {
    expect(pad2(0)).toBe("00");
    expect(pad2(7)).toBe("07");
    expect(pad2(42)).toBe("42");
  });

  it("relativeCompact 覆盖刚/分/时/天四档", () => {
    const now = Date.now();
    const iso = (minAgo: number) => new Date(now - minAgo * 60_000).toISOString();
    expect(relativeCompact(iso(0))).toBe("刚");
    expect(relativeCompact(iso(5))).toBe("5m");
    expect(relativeCompact(iso(18 * 60))).toBe("18h");
    expect(relativeCompact(iso(3 * 24 * 60))).toBe("3d");
    expect(relativeCompact("not-a-date")).toBe("—");
  });
});
