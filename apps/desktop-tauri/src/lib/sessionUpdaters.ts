/**
 * 会话列表的**纯 updater**：`(Session|Session[], payload) => Session|Session[]`。
 *
 * 从 App.tsx 的 29 个内联 `setSessions(prev => updateSession(...))` 里逐步
 * 抽取（2026-10-31 起，见 2026-10-30 审查笔记第十六/十七轮）。抽出后：
 * - 可以用 vitest 直接单测（不依赖 React / WebSocket）；
 * - App.tsx 里的回调只剩"解析 payload → 调 updater"。
 *
 * 约定：**不改写输入**（纯函数）、不产生副作用（时间戳由调用方注入
 * `now` 以便断言）、不在这里做任何 I/O。
 */
import type {
  ApprovalDecision,
  ChatMessage,
  ExecutionTimelineEvent,
  Session,
  TimelineStep,
} from "../types";
import { patchMessageById, updateSession } from "./sessionUpdates";
import { appendTextStep, appendToolStep, updateToolStepStdout } from "../app/streamEvents";
import { cleanSessionTitle } from "./sessionTitle";

/** 「新对话」占位标题的判定与 App.tsx 旧实现一致（含英文形态）。 */
const NEW_CHAT_TITLE = "新建对话";

/**
 * 首条消息自动起标题：仅当当前标题仍是占位（无标题 / 新对话）时才生成。
 * 首条消息有时是 LLM 产物（自动化/值守流程把推理原文带 JSON 外壳发进来），
 * 不洗就会把 `<think>…` 原文当标题存库（2026-10-28 删除确认卡实报）。
 */
export function computeAutoTitle(s: Session, fallbackContent: string): string {
  const isPlaceholder =
    !s.title || s.title === NEW_CHAT_TITLE || s.title === "新对话" || s.title === "New chat";
  if (!isPlaceholder) return s.title;
  const firstUserMsg = s.messages.find((m) => m.role === "user");
  const raw = firstUserMsg?.content ?? fallbackContent;
  const text = cleanSessionTitle(raw) || raw.replace(/\s+/g, " ").trim();
  return text.slice(0, 28) + (text.length > 28 ? "…" : "");
}

/**
 * 发送瞬间的占位提交（flushSync 同步 commit 的那个 updater）：
 * 追加用户消息 + 空 assistant 占位 + 自动标题。纯函数便于断言
 * "占位在 approval step 写入之前已进 state" 的竞态契约。
 */
export function beginAssistantTurn(
  s: Session,
  userMsg: ChatMessage,
  assistantId: string,
  fallbackContent: string,
  now: string,
): Session {
  return {
    ...s,
    messages: [
      ...s.messages,
      userMsg,
      { id: assistantId, role: "assistant", content: "", timestamp: now },
    ],
    updated_at: now,
    title: computeAutoTitle(s, fallbackContent),
  };
}

/**
 * 流式文本落盘：把攒批的 chunk 拼进目标 assistant 消息（正文 + 时间线
 * 文本步）。`flushPendingTextChunks` 每 rAF 调一次——这里是全应用最热
 * 的写入路径，保持为纯函数以便将来对攒批语义做精确单测。
 */
export function appendAssistantChunk(
  s: Session,
  targetId: string,
  batch: string,
  now: string,
): Session {
  return {
    ...s,
    messages: patchMessageById(s.messages, targetId, (m) => ({
      ...m,
      content: m.content + batch,
      unified_steps: appendTextStep(m.unified_steps, batch),
    })),
    updated_at: now,
  };
}

/** 内部：把一个时间线步骤去重后追加到目标 assistant 消息。 */
function appendAssistantStep(
  s: Session,
  assistantId: string,
  step: TimelineStep,
  isDuplicate: (existing: TimelineStep) => boolean,
  now: string,
): Session {
  const messages = s.messages.map((m) => {
    if (m.id !== assistantId) return m;
    const existing = m.unified_steps ?? [];
    if (existing.some(isDuplicate)) return m;
    return { ...m, unified_steps: [...existing, step] };
  });
  return { ...s, messages, updated_at: now };
}

/**
 * 审批卡步骤注入（`onApprovalRequest`）：把 `kind: "approval_request"`
 * push 进 assistant 的 `unified_steps`。**去重是契约的一部分**——live
 * 重发与意外双发都会走到这里，同 requestId 追加两张卡就是真 bug。
 */
export function appendApprovalRequestStep(
  s: Session,
  assistantId: string,
  step: TimelineStep,
  requestId: string,
  now: string,
): Session {
  return appendAssistantStep(
    s,
    assistantId,
    step,
    (x) =>
      x.kind === "approval_request" &&
      (x.meta as { requestId?: string }).requestId === requestId,
    now,
  );
}

/** 批量审批卡步骤注入（`onApprovalBatchRequest`），同上按 batchId 去重。 */
export function appendApprovalBatchStep(
  s: Session,
  assistantId: string,
  step: TimelineStep,
  batchId: string,
  now: string,
): Session {
  return appendAssistantStep(
    s,
    assistantId,
    step,
    (x) =>
      x.kind === "approval_request_batch" &&
      (x.meta as { batchId?: string }).batchId === batchId,
    now,
  );
}

/**
 * 追加一条消息（澄清卡 / 计划确认卡等）并刷新 `updated_at`。
 * 消息的构造留在调用方——这里只管"进列表"。
 */
export function appendChatMessage(
  s: Session,
  message: ChatMessage,
  now: string,
): Session {
  return { ...s, messages: [...s.messages, message], updated_at: now };
}

/**
 * 工具 stdout 分片落库：把攒批的 `toolCallId → chunk` 合并进
 * `execution_timeline`（找不到该调用的分片则新建一条 `tool_stdout`）。
 * 这是「调用工具就卡死」修复后的合并热点路径。
 */
export function applyToolStdoutChunks(
  s: Session,
  targetId: string,
  pending: ReadonlyMap<string, string>,
  now: string,
): Session {
  return {
    ...s,
    messages: s.messages.map((m) => {
      if (m.id !== targetId || !m.execution_timeline) return m;
      let timeline = m.execution_timeline;
      let steps = m.unified_steps;
      for (const [chunkToolCallId, chunkContent] of pending) {
        const t = [...timeline];
        let found = false;
        for (let i = t.length - 1; i >= 0; i--) {
          if (t[i].type === "tool_stdout" && t[i].tool_call_id === chunkToolCallId) {
            t[i] = { ...t[i], content: (t[i].content ?? "") + chunkContent };
            found = true;
            break;
          }
        }
        if (!found) {
          t.push({
            type: "tool_stdout",
            tool_call_id: chunkToolCallId,
            content: chunkContent,
            created_at: now,
          });
        }
        timeline = t;
        steps = updateToolStepStdout(steps, chunkToolCallId, chunkContent);
      }
      return { ...m, execution_timeline: timeline, unified_steps: steps };
    }),
    updated_at: now,
  };
}

/**
 * 往目标 assistant 的 `execution_timeline` 追加一个事件并同步
 * `unified_steps`（tool_start / tool_complete / file_change 三处调用点
 * 的形状完全相同——此前是三份内联拷贝）。
 */
export function appendTimelineEvent(
  s: Session,
  targetId: string,
  event: ExecutionTimelineEvent,
  now: string,
): Session {
  return {
    ...s,
    messages: s.messages.map((m) =>
      m.id === targetId
        ? {
            ...m,
            execution_timeline: [...(m.execution_timeline ?? []), event],
            unified_steps: appendToolStep(m.unified_steps, event),
          }
        : m,
    ),
    updated_at: now,
  };
}

/**
 * 流式错误收尾：目标 assistant 标记 `stopReason: "error"`；正文为空时
 * 用错误文案兜底（否则卡片只剩一个没有正文的错误气泡）。
 */
export function markAssistantError(
  s: Session,
  assistantId: string,
  errorText: string | null,
  now: string,
): Session {
  return {
    ...s,
    messages: s.messages.map((m) =>
      m.id === assistantId
        ? { ...m, content: m.content || errorText || "", stopReason: "error" as const }
        : m,
    ),
    updated_at: now,
  };
}

/**
 * 正常收尾：给目标 assistant 打 `stopReason`；`text` 仅作兜底
 * （流式 chunk 没产出可见正文、正文全在 done 事件里的边缘情况）；
 * `durationMs` 只在有值时写入（重载历史没有，渲染层显示"已完成"）。
 * 无变化时返回原 `s`（引用相等，调用方据此跳过）。
 */
export function finalizeAssistantStopReason(
  s: Session,
  assistantId: string,
  stopReason: string | undefined,
  fallbackText: string | undefined,
  durationMs: number | undefined,
  now: string,
): Session {
  const target = s.messages.find((m) => m.id === assistantId);
  if (!target) return s;
  const needsFallbackText =
    (!target.content || target.content.trim() === "") &&
    fallbackText !== undefined &&
    fallbackText.trim().length > 0;
  if (target.stopReason === stopReason && !needsFallbackText) return s;
  return {
    ...s,
    messages: s.messages.map((m) =>
      m.id === assistantId
        ? {
            ...m,
            content: needsFallbackText ? fallbackText : m.content,
            stopReason: stopReason as ChatMessage["stopReason"],
            ...(durationMs !== undefined ? { durationMs } : {}),
          }
        : m,
    ),
    updated_at: now,
  };
}

/**
 * 兜底：找不到目标 id 时，把**最后一条** assistant 行打上 `stopReason`
 * （保留旧行为——不知道 id 时的最佳猜测）。
 */
export function stampTrailingAssistantStopReason(
  s: Session,
  stopReason: string | undefined,
): Session {
  const msgs = [...s.messages];
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].role === "assistant") {
      msgs[i] = { ...msgs[i], stopReason: stopReason as ChatMessage["stopReason"] };
      break;
    }
  }
  return { ...s, messages: msgs };
}

function decisionMeta(
  decision: ApprovalDecision,
  remember: boolean | undefined,
  now: string,
) {
  return {
    decision,
    decidedAt: now,
    ...(remember ? { remembered: true } : {}),
  };
}

/**
 * 单条审批结算：把时间线里 `requestId` 匹配的 `approval_request` 步骤
 * 打上决策戳。已决策的步骤（过期后的补点）保持原样。
 *
 * 对应 App.tsx `settleApproval` 的本地 `stampDecision`（2026-10-31 抽出）。
 * `now` 注入当前时间，测试可以精确断言 `decidedAt`。
 */
export function stampApprovalDecision(
  s: Session,
  requestId: string,
  decision: ApprovalDecision,
  remember?: boolean,
  now: string = new Date().toISOString(),
): Session {
  return {
    ...s,
    messages: s.messages.map((m) => {
      const steps = m.unified_steps;
      if (!steps || steps.length === 0) return m;
      let touched = false;
      const next = steps.map((step): TimelineStep => {
        if (step.kind !== "approval_request") return step;
        if (step.meta.requestId !== requestId) return step;
        if (step.meta.decision) {
          // Already decided (stale click path); leave as-is.
          return step;
        }
        touched = true;
        return {
          ...step,
          meta: { ...step.meta, ...decisionMeta(decision, remember, now) },
        };
      });
      return touched ? { ...m, unified_steps: next } : m;
    }),
  };
}

/**
 * 批量审批结算：`batchId` 匹配的 `approval_request_batch` 步骤整批打戳。
 * 与单条版的差异只有匹配的 step.kind 与键——实现刻意保持同构。
 */
export function stampBatchApprovalDecision(
  s: Session,
  batchId: string,
  decision: ApprovalDecision,
  remember?: boolean,
  now: string = new Date().toISOString(),
): Session {
  return {
    ...s,
    messages: s.messages.map((m) => {
      const steps = m.unified_steps;
      if (!steps || steps.length === 0) return m;
      let touched = false;
      const next = steps.map((step): TimelineStep => {
        if (step.kind !== "approval_request_batch") return step;
        if (step.meta.batchId !== batchId) return step;
        if (step.meta.decision) return step;
        touched = true;
        return {
          ...step,
          meta: { ...step.meta, ...decisionMeta(decision, remember, now) },
        };
      });
      return touched ? { ...m, unified_steps: next } : m;
    }),
  };
}

/**
 * 澄清卡作答：`requestId` 匹配的 `clarify` 消息写入 `answer`。
 * `clarify` 缺失（历史会话恢复后字段被裁剪）时用消息正文补齐问题字段，
 * 与原内联实现的兜底一致。
 */
export function resolveClarify(
  sessions: Session[],
  requestId: string,
  answer: string,
): Session[] {
  return sessions.map((s) => ({
    ...s,
    messages: s.messages.map((m) => {
      if (m.role !== "clarify" || m.clarify?.requestId !== requestId) return m;
      return {
        ...m,
        clarify: {
          ...(m.clarify ?? {
            requestId,
            question: m.content,
            choices: [],
          }),
          answer,
        },
      };
    }),
  }));
}

/**
 * 计划确认卡「知道了」：本地置 `acknowledged`，卡片随即消失。
 * 后端没有计划确认回传通道（无 WS 变体、无 agent loop 等待方），纯本地。
 */
export function acknowledgePlan(sessions: Session[], messageId: string): Session[] {
  return sessions.map((s) => ({
    ...s,
    messages: s.messages.map((m) => {
      if (m.id !== messageId || m.role !== "plan" || !m.planConfirm) return m;
      return {
        ...m,
        planConfirm: { ...m.planConfirm, acknowledged: true },
      };
    }),
  }));
}

/**
 * 切走会话时的内存驱逐：清空 `messages`（大列表是内存大头），但保留
 * `message_count` 徽标数。幂等——重复驱逐同一会话是无操作。
 *
 * App.tsx 的两个 evict 效果（切换时 + 流结束后补刀）此前各自内联一份
 * 相同的 `evict` 闭包；这里收敛为一份。
 */
export function evictSessionMessages(
  sessions: Session[],
  sessionId: string,
): Session[] {
  return sessions.map((s) =>
    s.id === sessionId && s.messages.length > 0
      ? { ...s, messages: [], message_count: s.message_count ?? s.messages.length }
      : s,
  );
}

/**
 * 「加载更早的消息」：把上一页 `older` 去重后拼到时间线头部。
 * 去重在**函数内**做（以目标会话现有 `messages` 的 id 为准）——
 * 幂等：同一页重复传入不会产生重复消息。
 */
export function prependOlderMessages(
  sessions: Session[],
  sessionId: string,
  older: ChatMessage[],
): Session[] {
  return updateSession(sessions, sessionId, (s) => {
    const known = new Set(s.messages.map((m) => m.id));
    const fresh = older.filter((m) => !known.has(m.id));
    if (fresh.length === 0) return s;
    return { ...s, messages: [...fresh, ...s.messages] };
  });
}

/**
 * 刷新会话元数据（`refreshSessionMeta`）：服务端权威字段（title /
 * message_count / updated_at）覆盖本地，**已载入的 messages 一律不碰**
 * （清空会触发主对话重新拉取，流式期间更是灾难）；服务端新出现的会话
 * 追加到列表尾部。
 */
export function mergeSessionMeta(
  sessions: Session[],
  incoming: Session[],
): Session[] {
  const byId = new Map(incoming.map((s) => [s.id, s]));
  const merged = sessions.map((s) => {
    const fresh = byId.get(s.id);
    if (!fresh) return s;
    return {
      ...s,
      title: fresh.title,
      message_count: fresh.message_count,
      updated_at: fresh.updated_at,
    };
  });
  const known = new Set(sessions.map((s) => s.id));
  return [...merged, ...incoming.filter((s) => !known.has(s.id))];
}

/**
 * 只追加**新出现**的会话（WS 推送 / 多窗口同步路径）：已有 id 一律忽略，
 * 不覆盖本地（本地可能持有更细的 `messages`/审批状态）。
 */
export function appendNewSessions(
  sessions: Session[],
  incoming: Session[],
): Session[] {
  const known = new Set(sessions.map((s) => s.id));
  const added = incoming.filter((s) => !known.has(s.id));
  return added.length > 0 ? [...sessions, ...added] : sessions;
}
