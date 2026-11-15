import type { Session } from "../types";

/**
 * 只更新 `id` 匹配的那个会话，其余会话**引用原样返回**。
 *
 * `App.tsx` 里 20+ 处重复这个样板：
 * ```ts
 * setSessions((prev) => prev.map((s) => {
 *   if (s.id !== sessionId) return s;
 *   return { ...s, messages: next };
 * }));
 * ```
 * 提取成函数后调用点只写「怎么改这一条」，不再各自重写「其余原样返回」——
 * 后者写错（比如漏了 `return s`）会让**所有**会话被替换成 `undefined`，
 * 是流式路径上最容易造成整片聊天区消失的一类错误。
 *
 * 返回值保持与输入同长度的数组顺序，React 的浅比较依赖未命中项引用不变。
 */
export function updateSession(
  sessions: Session[],
  id: string,
  patch: (session: Session) => Session,
): Session[] {
  let changed = false;
  const next = sessions.map((s) => {
    if (s.id !== id) return s;
    changed = true;
    return patch(s);
  });
  // 没命中时返回原数组：调用方常在 effect / 回调里无脑 set，保住引用可以让
  // React 跳过这次 re-render（很多 setSessions 的目标会话并不在当前列表里）。
  return changed ? next : sessions;
}

/**
 * 按 id 替换消息数组里的一条，其余元素引用不变；未命中返回原数组。
 *
 * 快路径专为流式文本设计：`onChunk` 命中的**几乎总是最后一条**（正在吐字的
 * assistant 气泡）。先看末尾能省掉对整个数组的逐项 `id` 比较——长会话里
 * 每次 chunk 都要扫一遍全部消息，是流式期间最稳定的固定开销之一。
 *
 * 慢路径（目标在中间：历史消息重放、多气泡交错）退化为线性查找，语义与
 * `messages.map(...)` 完全一致。
 */
export function patchMessageById<T extends { id: string }>(
  messages: T[],
  id: string,
  patch: (message: T) => T,
): T[] {
  const last = messages.length - 1;
  if (last >= 0 && messages[last].id === id) {
    const next = messages.slice();
    next[last] = patch(messages[last]);
    return next;
  }
  for (let i = 0; i < last; i++) {
    if (messages[i].id === id) {
      const next = messages.slice();
      next[i] = patch(messages[i]);
      return next;
    }
  }
  return messages;
}
