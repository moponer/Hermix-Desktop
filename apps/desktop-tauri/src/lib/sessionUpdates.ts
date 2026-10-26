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
