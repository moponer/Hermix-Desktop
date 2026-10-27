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
    const history = [row("user", "a"), row("assistant", "b")];
    expect(mergeTranscript(history, [])).toEqual(history);
  });

  it("落库历史尾部的孤儿提问不显示（她没有回答过它）", () => {
    const history = [row("user", "a"), row("assistant", "b"), row("user", "被打断的提问")];
    expect(mergeTranscript(history, []).map((r) => r.text)).toEqual(["a", "b"]);
  });

  /// 2026-10-27 实报：上一轮被打断后历史是 `[user(旧), user(新)]`，新问题
  /// 的回复开头却在讲旧问题 —— 界面上就是两条用户气泡 + 一段文不对题的回复。
  it("历史尾部两条连续 user（旧孤儿 + 本轮）只显示本轮", () => {
    const history = [row("user", "一页纸速览 美观一点的"), row("user", "超星学习的能继续吗")];
    const live = [row("user", "超星学习的能继续吗"), row("assistant", "查一下超星那边")];
    expect(mergeTranscript(history, live).map((r) => r.text)).toEqual([
      "超星学习的能继续吗",
      "查一下超星那边",
    ]);
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

  it("纯工具轮次降级为活动行（🔧 工具名），不再渲染成空白块", () => {
    const rows = toTranscriptRows([
      { role: "user", content: "继续去超星学习" },
      { role: "assistant", content: "", tool_calls: [{ name: "browser_open" }, { function: { name: "browser_text" } }] },
      { role: "assistant", content: "查看器已经启动了。" },
      { role: "assistant", content: "" },
    ]);
    const acts = rows.filter((r) => r.kind === "activity");
    // 只有一条带 tool_calls 的空轮生成活动行；无 tool_calls 的空轮被丢弃。
    expect(acts.length).toBe(1);
    expect(acts[0].text).toContain("🔧");
    expect(acts[0].text).toContain("browser_open");
    expect(acts[0].text).toContain("browser_text");
    // 无可见文本的行不出现在转录里
    expect(rows.some((r) => r.text.trim() === "")).toBe(false);
  });
});
