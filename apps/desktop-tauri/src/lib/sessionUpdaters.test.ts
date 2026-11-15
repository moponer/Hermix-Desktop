/**
 * sessionUpdaters 纯函数单测 —— App.tsx 内联 updater 抽取后的行为契约。
 *
 * 这些函数此前内联在 `setSessions(prev => ...)` 里，唯一的"测试"是手动点
 * 卡片；抽成纯函数后，竞态语义（已决策的步骤不许被过期补点改写）第一次
 * 有了可断言的测试位。
 */
import { describe, expect, it } from "vitest";
import {
  acknowledgePlan,
  appendApprovalBatchStep,
  appendApprovalRequestStep,
  appendAssistantChunk,
  appendChatMessage,
  appendNewSessions,
  appendTimelineEvent,
  applyToolStdoutChunks,
  beginAssistantTurn,
  computeAutoTitle,
  evictSessionMessages,
  finalizeAssistantStopReason,
  markAssistantError,
  mergeSessionMeta,
  prependOlderMessages,
  resolveClarify,
  stampApprovalDecision,
  stampBatchApprovalDecision,
  stampTrailingAssistantStopReason,
} from "./sessionUpdaters";
import type { ChatMessage, Session, TimelineStep } from "../types";

const NOW = "2026-10-31T12:00:00.000Z";

function msg(partial: Partial<ChatMessage>): ChatMessage {
  return { id: "m", role: "assistant", content: "", ...partial } as ChatMessage;
}

function session(messages: ChatMessage[]): Session {
  return { id: "s1", title: "t", messages } as unknown as Session;
}

function approvalStep(requestId: string, decided?: "approved" | "denied"): TimelineStep {
  return {
    kind: "approval_request",
    meta: { requestId, ...(decided ? { decision: decided } : {}) },
  } as unknown as TimelineStep;
}

function batchStep(batchId: string): TimelineStep {
  return { kind: "approval_request_batch", meta: { batchId } } as unknown as TimelineStep;
}

describe("stampApprovalDecision", () => {
  it("给匹配 requestId 的步骤打决策戳（含时间与 remember）", () => {
    const s = session([msg({ unified_steps: [approvalStep("apr-1")] })]);
    const out = stampApprovalDecision(s, "apr-1", "approved", true, NOW);
    const step = out.messages[0].unified_steps![0] as Extract<
      TimelineStep,
      { kind: "approval_request" }
    >;
    expect(step.meta.decision).toBe("approved");
    expect(step.meta.decidedAt).toBe(NOW);
    expect((step.meta as { remembered?: boolean }).remembered).toBe(true);
  });

  it("不匹配的 requestId 与其它消息原样保留", () => {
    const s = session([
      msg({ id: "a", unified_steps: [approvalStep("apr-other")] }),
      msg({ id: "b", content: "纯文本" }),
    ]);
    const out = stampApprovalDecision(s, "apr-1", "denied", undefined, NOW);
    expect(out.messages[0]).toBe(s.messages[0]);
    expect(out.messages[1]).toBe(s.messages[1]);
  });

  it("已决策的步骤不被过期补点改写（stale click 契约）", () => {
    const s = session([msg({ unified_steps: [approvalStep("apr-1", "denied")] })]);
    const out = stampApprovalDecision(s, "apr-1", "approved", undefined, NOW);
    const step = out.messages[0].unified_steps![0] as Extract<
      TimelineStep,
      { kind: "approval_request" }
    >;
    expect(step.meta.decision).toBe("denied");
  });
});

describe("stampBatchApprovalDecision", () => {
  it("按 batchId 整批打戳", () => {
    const s = session([
      msg({ unified_steps: [batchStep("b-1"), approvalStep("apr-1")] }),
    ]);
    const out = stampBatchApprovalDecision(s, "b-1", "approved", undefined, NOW);
    const first = out.messages[0].unified_steps![0] as Extract<
      TimelineStep,
      { kind: "approval_request_batch" }
    >;
    expect(first.meta.decision).toBe("approved");
    // 单条卡不受批量结算影响
    const second = out.messages[0].unified_steps![1] as Extract<
      TimelineStep,
      { kind: "approval_request" }
    >;
    expect(second.meta.decision).toBeUndefined();
  });
});

describe("resolveClarify", () => {
  it("给匹配 requestId 的澄清卡写入 answer，跨会话生效", () => {
    const s1 = session([
      msg({ id: "c1", role: "clarify", content: "选哪个？", clarify: { requestId: "clr-1", question: "选哪个？", choices: ["a", "b"] } }),
    ] as unknown as ChatMessage[]);
    const s2 = session([
      msg({ id: "c2", role: "clarify", content: "其它会话", clarify: { requestId: "clr-2", question: "q", choices: [] } }),
    ] as unknown as ChatMessage[]);
    const out = resolveClarify([s1, s2], "clr-1", "a");
    expect(out[0].messages[0].clarify!.answer).toBe("a");
    expect(out[1].messages[0].clarify!.answer).toBeUndefined();
  });

  it("clarify 缺失的澄清卡不匹配（守卫先于兜底 spread）", () => {
    // 原内联实现即如此：`m.clarify?.requestId !== requestId` 的守卫在
    // `...(m.clarify ?? {...})` 兜底之前——兜底 spread 实际是死分支。
    // 这里固化真实契约；将来若要支持"裸 clarify 消息"，需连守卫一起改。
    const s = session([
      msg({ id: "c1", role: "clarify", content: "正文即问题" }),
    ]);
    const out = resolveClarify([s], "clr-1", "答案");
    expect(out[0].messages[0].clarify).toBeUndefined();
  });
});

describe("acknowledgePlan", () => {
  it("按 messageId 置 acknowledged", () => {
    const s = session([
      msg({ id: "p1", role: "plan", planConfirm: { acknowledged: false } as never }),
    ]);
    const out = acknowledgePlan([s], "p1");
    expect(out[0].messages[0].planConfirm!.acknowledged).toBe(true);
  });

  it("不匹配的消息原样返回（引用相等）", () => {
    const m = msg({ id: "p2", role: "plan", planConfirm: { acknowledged: false } as never });
    const s = session([m]);
    const out = acknowledgePlan([s], "p1");
    expect(out[0].messages[0]).toBe(m);
  });
});

describe("evictSessionMessages", () => {
  it("清空目标会话的 messages 但保留徽标数", () => {
    const a = { ...session([msg({ id: "m1" })]), message_count: 7 };
    const b = { ...session([msg({ id: "m2" })]), id: "s2" };
    const out = evictSessionMessages([a, b], "s1");
    expect(out[0].messages).toEqual([]);
    expect(out[0].message_count).toBe(7);
    expect(out[1]).toBe(b);
  });

  it("message_count 缺失时回退为被驱逐前的条数", () => {
    const a = session([msg({ id: "m1" }), msg({ id: "m2" })]);
    const out = evictSessionMessages([a], "s1");
    expect(out[0].message_count).toBe(2);
  });

  it("幂等：重复驱逐不再变化", () => {
    const a = session([msg({ id: "m1" })]);
    const once = evictSessionMessages([a], "s1");
    const twice = evictSessionMessages(once, "s1");
    expect(twice[0].messages).toEqual([]);
  });
});

describe("prependOlderMessages", () => {
  it("把上一页拼到头部，且按现有 id 去重", () => {
    const existing = [msg({ id: "m3" })];
    const older = [msg({ id: "m1" }), msg({ id: "m2" }), msg({ id: "m3" })];
    const out = prependOlderMessages([session(existing)], "s1", older);
    expect(out[0].messages.map((m) => m.id)).toEqual(["m1", "m2", "m3"]);
  });

  it("全部重复时内容不变（幂等）", () => {
    const s = session([msg({ id: "m1" })]);
    const out = prependOlderMessages([s], "s1", [msg({ id: "m1" })]);
    expect(out[0].messages.map((m) => m.id)).toEqual(["m1"]);
  });

  it("其它会话不受影响", () => {
    const other = { ...session([msg({ id: "x" })]), id: "s2" };
    const out = prependOlderMessages([other], "s1", [msg({ id: "m1" })]);
    expect(out[0]).toBe(other);
  });
});

describe("mergeSessionMeta", () => {
  it("覆盖权威元数据但保留本地 messages，新会话追加到尾部", () => {
    const local = {
      ...session([msg({ id: "m1" })]),
      title: "旧标题",
      message_count: 1,
      updated_at: "old",
    };
    const fresh = {
      ...session([]),
      id: "s1",
      title: "新标题",
      message_count: 9,
      updated_at: "new",
    } as unknown as Session;
    const brand = { ...session([]), id: "s2", title: "新会话" } as unknown as Session;
    const out = mergeSessionMeta([local], [fresh, brand]);
    expect(out).toHaveLength(2);
    expect(out[0].title).toBe("新标题");
    expect(out[0].message_count).toBe(9);
    expect(out[0].messages).toHaveLength(1);
    expect(out[1].id).toBe("s2");
  });

  it("本地多出的会话保留（服务端没说的不删）", () => {
    const local = session([msg({ id: "m1" })]);
    const out = mergeSessionMeta([local], []);
    expect(out).toHaveLength(1);
  });
});

describe("appendNewSessions", () => {
  it("只追加新 id，已有的一律忽略", () => {
    const known = { ...session([]), id: "s1", title: "本地版" } as unknown as Session;
    const incoming = [
      { ...session([]), id: "s1", title: "服务端版" } as unknown as Session,
      { ...session([]), id: "s2" } as unknown as Session,
    ];
    const out = appendNewSessions([known], incoming);
    expect(out).toHaveLength(2);
    expect(out[0].title).toBe("本地版");
    expect(out[1].id).toBe("s2");
  });

  it("没有新会话时返回原数组引用（零拷贝）", () => {
    const known = session([]);
    const sessions = [known];
    const incoming = [{ ...session([]), id: "s1" } as unknown as Session];
    const out = appendNewSessions(sessions, incoming);
    expect(out).toBe(sessions);
  });
});

describe("beginAssistantTurn", () => {
  it("追加用户消息 + 空 assistant 占位 + 自动标题", () => {
    const s = { ...session([]), title: "新对话" } as Session;
    const userMsg = msg({ id: "u1", role: "user", content: "帮我看看这个报错" });
    const out = beginAssistantTurn(s, userMsg, "a1", "帮我看看这个报错", NOW);
    expect(out.messages).toHaveLength(2);
    expect(out.messages[0].role).toBe("user");
    expect(out.messages[1]).toMatchObject({ id: "a1", role: "assistant", content: "" });
    expect(out.messages[1].timestamp).toBe(NOW);
    expect(out.updated_at).toBe(NOW);
    // 占位标题被首条用户消息替换，且截断到 28 字
    expect(out.title).toContain("帮我看看这个报错");
  });

  it("已有真实标题时不覆盖", () => {
    const s = { ...session([]), title: "用户的会话" } as Session;
    const out = beginAssistantTurn(
      s,
      msg({ id: "u1", role: "user", content: "新内容" }),
      "a1",
      "新内容",
      NOW,
    );
    expect(out.title).toBe("用户的会话");
  });
});

describe("appendAssistantChunk", () => {
  it("把批内 chunk 拼进正文与时间线文本步", () => {
    const s = session([
      msg({ id: "a1", role: "assistant", content: "Hello", unified_steps: [] as never }),
    ]);
    const out = appendAssistantChunk(s, "a1", " world", NOW);
    expect(out.messages[0].content).toBe("Hello world");
    expect(out.updated_at).toBe(NOW);
  });

  it("目标不存在时其它消息原样保留", () => {
    const m = msg({ id: "m1", content: "x" });
    const s = session([m]);
    const out = appendAssistantChunk(s, "a-missing", "y", NOW);
    expect(out.messages[0].content).toBe("x");
  });
});

describe("computeAutoTitle", () => {
  it("占位标题取首条用户消息并截断到 28 字", () => {
    const long = "这是一条特别长的首条消息内容，用来验证标题会在二十八个字符处被截断并附加省略号。";
    const s = { ...session([msg({ id: "u1", role: "user", content: long })]), title: "新对话" } as Session;
    const out = computeAutoTitle(s, "fallback");
    expect(out.endsWith("…")).toBe(true);
    expect(out.length).toBe(29); // 28 字 + …
  });

  it("真实标题原样返回，不重新生成", () => {
    const s = { ...session([msg({ id: "u1", role: "user", content: "x" })]), title: "已命名" } as Session;
    expect(computeAutoTitle(s, "fallback")).toBe("已命名");
  });

  it("没有用户消息时回退到 fallbackContent", () => {
    const s = { ...session([]), title: "新对话" } as Session;
    expect(computeAutoTitle(s, "兜底内容")).toBe("兜底内容");
  });
});

describe("审批卡步骤注入", () => {
  const assistant = msg({ id: "a1", role: "assistant", content: "" });

  it("单条：注入 + 同 requestId 去重", () => {
    const step = approvalStep("apr-1");
    const s1 = appendApprovalRequestStep(session([assistant]), "a1", step, "apr-1", NOW);
    expect(s1.messages[0].unified_steps).toHaveLength(1);
    const s2 = appendApprovalRequestStep(s1, "a1", step, "apr-1", NOW);
    expect(s2.messages[0].unified_steps).toHaveLength(1);
    // 不同 requestId 不去重
    const s3 = appendApprovalRequestStep(s1, "a1", approvalStep("apr-2"), "apr-2", NOW);
    expect(s3.messages[0].unified_steps).toHaveLength(2);
  });

  it("批量：按 batchId 去重，不干扰单条卡", () => {
    const s = session([assistant]);
    const s1 = appendApprovalBatchStep(s, "a1", batchStep("b-1"), "b-1", NOW);
    const s2 = appendApprovalBatchStep(s1, "a1", batchStep("b-1"), "b-1", NOW);
    expect(s2.messages[0].unified_steps).toHaveLength(1);
  });
});

describe("appendChatMessage", () => {
  it("追加消息并刷新 updated_at", () => {
    const m = msg({ id: "new", role: "clarify" });
    const out = appendChatMessage(session([msg({ id: "old" })]), m, NOW);
    expect(out.messages).toHaveLength(2);
    expect(out.messages[1]).toBe(m);
    expect(out.updated_at).toBe(NOW);
  });
});

describe("applyToolStdoutChunks", () => {
  function withTimeline(timeline: unknown[]): Session {
    return session([msg({ id: "a1", role: "assistant", execution_timeline: timeline as never })]);
  }

  it("合并进既有 tool_stdout", () => {
    const s = withTimeline([
      { type: "tool_stdout", tool_call_id: "t1", content: "abc", created_at: NOW },
    ]);
    const out = applyToolStdoutChunks(s, "a1", new Map([["t1", "def"]]), NOW);
    const t = (out.messages[0].execution_timeline as Array<{ content?: string }>)[0];
    expect(t.content).toBe("abcdef");
  });

  it("找不到调用时分片新建一条 tool_stdout", () => {
    const s = withTimeline([{ type: "tool_start", tool_call_id: "t1" }]);
    const out = applyToolStdoutChunks(s, "a1", new Map([["t9", "hi"]]), NOW);
    const tl = out.messages[0].execution_timeline as Array<{ type: string; content?: string }>;
    expect(tl.at(-1)?.type).toBe("tool_stdout");
    expect(tl.at(-1)?.content).toBe("hi");
  });
});

describe("appendTimelineEvent", () => {
  it("push 事件并同步 unified_steps", () => {
    const s = session([msg({ id: "a1", role: "assistant" })]);
    const event = { type: "tool_start", tool: "shell", tool_call_id: "t1", created_at: NOW } as never;
    const out = appendTimelineEvent(s, "a1", event, NOW);
    expect(out.messages[0].execution_timeline).toHaveLength(1);
    expect(out.messages[0].unified_steps).toHaveLength(1);
  });
});

describe("错误与收尾", () => {
  it("markAssistantError：正文为空时用错误文案兜底，非空保留", () => {
    const empty = session([msg({ id: "a1", role: "assistant", content: "" })]);
    expect(markAssistantError(empty, "a1", "出错了", NOW).messages[0].content).toBe("出错了");
    const hasText = session([msg({ id: "a1", role: "assistant", content: "已有正文" })]);
    expect(markAssistantError(hasText, "a1", null, NOW).messages[0].content).toBe("已有正文");
  });

  it("finalizeAssistantStopReason：无变化返回原引用（跳过渲染）", () => {
    const s = session([msg({ id: "a1", role: "assistant", content: "hi", stopReason: "done" as never })]);
    const out = finalizeAssistantStopReason(s, "a1", "done", undefined, undefined, NOW);
    expect(out).toBe(s);
  });

  it("finalizeAssistantStopReason：正文为空时用 done 事件的 text 兜底 + durationMs", () => {
    const s = session([msg({ id: "a1", role: "assistant", content: "" })]);
    const out = finalizeAssistantStopReason(s, "a1", "done", "全部文本", 1234, NOW);
    expect(out.messages[0].content).toBe("全部文本");
    expect(out.messages[0].stopReason).toBe("done");
    expect((out.messages[0] as { durationMs?: number }).durationMs).toBe(1234);
  });

  it("stampTrailingAssistantStopReason：标记最后一条 assistant", () => {
    const s = session([
      msg({ id: "u", role: "user" }),
      msg({ id: "a2", role: "assistant" }),
    ]);
    const out = stampTrailingAssistantStopReason(s, "interrupted");
    expect(out.messages[1].stopReason).toBe("interrupted");
  });
});
