import { describe, it, expect } from "vitest";
import { hardenSoftLineBreaks } from "./softBreaks";

// @vitest-environment node

describe("hardenSoftLineBreaks", () => {
  it("插入双空格让单 \\n 变成硬换行", () => {
    const input = "第一行\n第二行";
    const got = hardenSoftLineBreaks(input);
    expect(got).toBe("第一行  \n第二行");
  });

  it("跨多段保留段间空行", () => {
    const input = "段落一。\n\n段落二。";
    const got = hardenSoftLineBreaks(input);
    expect(got).toBe("段落一。\n\n段落二。");
  });

  it("fenced code block 内的 \\n 不被破坏", () => {
    const input = "第一段。\n\n```ts\nconst x = 1;\nconst y = 2;\n```\n\n尾段。";
    const got = hardenSoftLineBreaks(input);
    // 段末行为段边界，不追加双空格
    expect(got).toBe("第一段。\n\n```ts\nconst x = 1;\nconst y = 2;\n```\n\n尾段。");
  });

  it("段内换行 + 段边界", () => {
    // "前置段\n留段" → 中间是软换行（同一段）
    // 与 fenced 块的边界 fence 不相混
    const input = "前置\n留段\n\n```ts\nconst x = 1;\n```\n\n末段";
    const got = hardenSoftLineBreaks(input);
    expect(got).toContain("前置  \n留段");
    // fence 内不动
    expect(got).toContain("```ts\nconst x = 1;\n```");
    // 末段不增空格
    expect(got.endsWith("末段")).toBe(true);
  });

  it("多个 fenced block 混合", () => {
    const input = "段一\n段二\n\n```js\na\nb\n```\n\n段三\n段四";
    const got = hardenSoftLineBreaks(input);
    // 段内换行硬化
    expect(got).toContain("段一  \n段二");
    expect(got).toContain("段三  \n段四");
    // code 内原文
    expect(got).toContain("```js\na\nb\n```");
  });
});