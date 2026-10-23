import { describe, expect, it } from "vitest";

import { mergeTranscript, toTranscriptRows, TRANSCRIPT_CAP } from "./twinTranscript";

const row = (role: string, text: string) => ({ role, text });

describe("toTranscriptRows", () => {
  it("只留 user / assistant 的非空文本", () => {
    const rows = toTranscriptRows([
      { role: "user", content: "在吗" },
      { role: "tool", content: "shell output" },
      { role: "assistant", content: "   " },
      { role: "clarify", content: "选哪个" },
      { role: "assistant", content: "在。" },
    ]);
    expect(rows).toEqual([row("user", "在吗"), row("assistant", "在。")]);
  });

  it("超长文本截断到 300 字并加省略号", () => {
    const rows = toTranscriptRows([{ role: "assistant", content: "字".repeat(500) }]);
    expect(rows[0].text).toHaveLength(300);
    expect(rows[0].text.endsWith("…")).toBe(true);
  });

  it("content 缺失不抛错", () => {
    expect(toTranscriptRows([{ role: "assistant" }])).toEqual([]);
  });
});

describe("mergeTranscript", () => {
  it("没有本轮消息时只显示落库历史的尾部", () => {
    const history = [row("user", "a"), row("assistant", "b"), row("user", "c")];
    expect(mergeTranscript(history, [])).toEqual(history);
  });

  it("本轮落库后不重复显示同一条 user", () => {
    const history = [
      row("user", "早"),
      row("assistant", "早，今天要做什么"),
      row("user", "整理本周笔记"),
      row("assistant", "整理好了"),
    ];
    // 本轮在内存里（App 写入），history 也已落库同一轮
    const live = [row("user", "整理本周笔记"), row("assistant", "整理好了")];
    const merged = mergeTranscript(history, live);
    expect(merged.map((r) => r.text)).toEqual(["早", "早，今天要做什么", "整理本周笔记", "整理好了"]);
  });

  it("本轮尚未落库时接在历史后面", () => {
    const history = [row("user", "早"), row("assistant", "早")];
    const live = [row("user", "新问题"), row("assistant", "在跑")];
    expect(mergeTranscript(history, live).map((r) => r.text)).toEqual([
      "早",
      "早",
      "新问题",
      "在跑",
    ]);
  });

  it("超出上限时只留尾部", () => {
    const history = Array.from({ length: 10 }, (_, i) => row("user", `m${i}`));
    const live = [row("user", "m9"), row("assistant", "reply")];
    const merged = mergeTranscript(history, live);
    expect(merged).toHaveLength(TRANSCRIPT_CAP);
    expect(merged[merged.length - 1]).toEqual(row("assistant", "reply"));
  });

  it("live 里没有 user 行（比如只有一条错误提示）时全部接在历史后", () => {
    const history = [row("user", "早")];
    const live = [row("assistant", "出错了")];
    expect(mergeTranscript(history, live).map((r) => r.text)).toEqual(["早", "出错了"]);
  });
});
