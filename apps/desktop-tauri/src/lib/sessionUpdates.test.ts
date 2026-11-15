import { describe, expect, it } from "vitest";
import { patchMessageById, updateSession } from "./sessionUpdates";
import type { Session } from "../types";

const s = (id: string): Session => ({ id, title: id, messages: [] }) as unknown as Session;

describe("updateSession", () => {
  it("只替换目标会话，其余会话保持同一引用", () => {
    const a = s("a");
    const b = s("b");
    const next = updateSession([a, b], "b", (x) => ({ ...x, title: "B!" }));

    expect(next).toHaveLength(2);
    expect(next[0]).toBe(a); // 未命中项必须引用不变，否则整片列表都会重渲染
    expect(next[1]).not.toBe(b); // 命中项是新对象
    expect(next[1].title).toBe("B!");
    expect(next[1].id).toBe("b"); // 顺序保持
  });

  it("未命中时返回原数组，让 React 可以跳过这次提交", () => {
    const list = [s("a"), s("b")];
    expect(updateSession(list, "missing", (x) => x)).toBe(list);
  });
});

describe("patchMessageById", () => {
  const m = (id: string, content = ""): { id: string; content: string } => ({ id, content });

  it("命中末尾消息（流式热路径）：只换最后一条，其余引用不变", () => {
    const a = m("a");
    const b = m("b");
    const next = patchMessageById([a, b], "b", (x) => ({ ...x, content: x.content + "!" }));
    expect(next[0]).toBe(a);
    expect(next[1]).not.toBe(b);
    expect(next[1].content).toBe("!");
  });

  it("命中中间消息（历史重放）：线性查找后替换，其余引用不变", () => {
    const a = m("a");
    const b = m("b");
    const c = m("c");
    const next = patchMessageById([a, b, c], "a", (x) => ({ ...x, content: "A" }));
    expect(next[0].content).toBe("A");
    expect(next[1]).toBe(b);
    expect(next[2]).toBe(c);
  });

  it("未命中返回原数组", () => {
    const list = [m("a"), m("b")];
    expect(patchMessageById(list, "missing", (x) => x)).toBe(list);
  });

  it("空数组不 panic", () => {
    expect(patchMessageById([], "x", (x) => x)).toEqual([]);
  });
});
