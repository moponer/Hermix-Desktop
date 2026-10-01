// 「先说正文 → 又继续思考」时正文丢失的回归测试。
//
// 2026-10-01 用户实机发现（严重）：LLM 先给出一段正文，随后又进入下一段
// 思考。那段正文**短暂显示一下就消失**，最终渲染里也没有它。
//
// 根因：`App.tsx` 的 `onReasoningChunk` 里有一段「把末尾 text step 回溯
// 转成 reasoning」的补救，条件是「只要末尾有 text step 就转」。它本来是
// 为后端 `looks_like_untagged_reasoning`（模型不打 `<think>` 标签）准备的
// —— 那种情况下流**开头**几段被误发成 Text，判定后要改回来。
//
// 但它在每个 reasoning 分片到达时都跑，于是「已说出口的正文」也被当成
// 无标签推理前缀 spliced 掉。内容丢失。
//
// 本文件把那条补救的判定抽成可测的纯函数，两种场景各钉一条：
//   1. 真·无标签推理前缀 → 仍然要转（不能把修复改坏）
//   2. 思考中途说出的正文 → **绝不转**
import { describe, it, expect } from "vitest";
import {
  appendReasoningStep,
  appendTextStep,
  appendToolStep,
  applyReasoningChunk,
  isUntaggedReasoningPrefix,
} from "./streamEvents";
import type { TimelineStep, ExecutionTimelineEvent } from "../types";

// 守卫与状态转移都在 `streamEvents` 里（生产与测试共用同一份实现）——
// 测试里另抄一份就等于没测：改生产代码时测试还会绿。
const onReasoningChunk = (
  steps: TimelineStep[],
  content: string,
  reasoningContent: string,
  piece: string,
) => {
  const r = applyReasoningChunk(steps, content, reasoningContent, piece);
  return {
    steps: r.steps,
    content: r.content,
    reasoningContent: r.reasoningContent,
  };
};

const toolEv = (): ExecutionTimelineEvent => ({
  type: "tool_start",
  tool: "terminal",
  arguments: '{"command":"ls"}',
  created_at: new Date().toISOString(),
} as ExecutionTimelineEvent);

const texts = (steps: TimelineStep[]) =>
  steps.filter((s): s is Extract<TimelineStep, { kind: "text" }> => s.kind === "text");

const reasonings = (steps: TimelineStep[]) =>
  steps.filter((s): s is Extract<TimelineStep, { kind: "reasoning" }> => s.kind === "reasoning");

describe("守卫：只有流开头的 text 才可能是无标签推理", () => {
  it("全是 text → 是前缀（无标签推理的真实形态）", () => {
    expect(isUntaggedReasoningPrefix([])).toBe(true);
    expect(
      isUntaggedReasoningPrefix([{ kind: "text", content: "a" }, { kind: "text", content: "b" }]),
    ).toBe(true);
  });

  it("出现过 reasoning 之后 → 不是前缀", () => {
    expect(
      isUntaggedReasoningPrefix([
        { kind: "reasoning", content: "r" },
        { kind: "text", content: "正文" },
      ]),
    ).toBe(false);
  });

  it("出现过 tools 之后 → 不是前缀（工具之后的输出一定是真输出）", () => {
    expect(
      isUntaggedReasoningPrefix([
        { kind: "tools", events: [toolEv()] },
        { kind: "text", content: "正文" },
      ]),
    ).toBe(false);
  });
});

describe("场景一：无标签推理前缀 —— 修复必须继续生效", () => {
  it("开头的 text 被转成 reasoning，content 清空", () => {
    // 后端先误发了 2 个 Text 分片，随后判定「这是无标签推理」发 Thinking。
    let steps = appendTextStep(undefined, "我先看看这个");
    steps = appendTextStep(steps, "有什么问题");

    const r = onReasoningChunk(steps, "", "", "发现根因了");

    expect(r.content).toBe("");
    expect(r.reasoningContent).toBe("我先看看这个有什么问题发现根因了");
    expect(texts(r.steps)).toHaveLength(0);
    expect(reasonings(r.steps)).toHaveLength(1);
  });
});

describe("场景二：思考中途说出的正文 —— 绝不能被吞（用户报的 bug）", () => {
  /** 复刻截图里的真实流：思考 → 正文 → 继续思考 → 收尾正文。 */
  function buggyStream() {
    let steps = appendReasoningStep(undefined, "用户在问一个哲学问题");
    steps = appendTextStep(steps, "所以：能帮你推理，但没有立场。"); // ← 正文①
    const mid = onReasoningChunk(steps, "所以：能帮你推理，但没有立场。", "用户在问一个哲学问题", "The user is asking...");
    steps = mid.steps;
    let content = mid.content;
    let reasoning = mid.reasoningContent;

    // 后续又来正文
    steps = appendTextStep(steps, "这条就是个聊天提问。");
    content += "这条就是个聊天提问。";
    reasoning += "这条就是个聊天提问。";
    return { steps, content, reasoning };
  }

  it("正文① 既留在 content 里，也不在 reasoning 里", () => {
    const { content, reasoning } = buggyStream();
    expect(content).toContain("所以：能帮你推理，但没有立场。");
    expect(reasoning).not.toContain("所以：能帮你推理，但没有立场。");
  });

  it("正文① 仍然是一个可渲染的 text step", () => {
    const { steps } = buggyStream();
    const t = texts(steps);
    // 两段正文是同一个 body 的流式分片，`appendTextStep` 会并进同一个
    // step —— 所以这里断言「正文① 还在」，不是「只有正文①」。
    expect(t).toHaveLength(1);
    expect(t[0].content).toContain("所以：能帮你推理，但没有立场。");
  });

  it("后续推理不会被并进正文那一段（顺序也不该乱）", () => {
    const { steps } = buggyStream();
    expect(reasonings(steps)).toHaveLength(1);
    // text 步在 reasoning 之后 —— 正文仍然排在它该在的位置
    expect(steps[steps.length - 1].kind).toBe("text");
  });

  it("工具之后说的正文同样不被吞", () => {
    let steps = appendToolStep(undefined, toolEv());
    steps = appendTextStep(steps, "命令跑完了，结果是这样");
    const r = onReasoningChunk(steps, "命令跑完了，结果是这样", "", "我得解释一下");
    expect(r.content).toBe("命令跑完了，结果是这样");
    expect(texts(r.steps)).toHaveLength(1);
  });
});

describe("场景三：连续推理不产生多余转换", () => {
  it("第二个推理分片到达时没有 text 可转，是纯追加", () => {
    let steps = appendReasoningStep(undefined, "第一段");
    const r = onReasoningChunk(steps, "", "", "第二段");
    expect(r.content).toBe("");
    expect(reasonings(r.steps)).toHaveLength(1);
    expect(reasonings(r.steps)[0].content).toBe("第一段第二段");
  });
});
