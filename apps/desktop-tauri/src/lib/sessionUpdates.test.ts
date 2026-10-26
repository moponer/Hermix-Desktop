import { describe, expect, it } from "vitest";
import { updateSession } from "./sessionUpdates";
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
