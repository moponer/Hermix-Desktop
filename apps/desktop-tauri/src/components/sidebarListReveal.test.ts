import { describe, it, expect } from "vitest";
import {
  resolveVisibleCount,
  resolveRevealAction,
  LIST_PREVIEW_COUNT,
  LIST_EXPAND_CAP,
  type ListReveal,
} from "./sidebarListReveal";

// 这些用例锁死一个易错点：**展开上限必须有第二个出口**。
// 只做"折叠/展开"两态时，总数超过上限会卡死——展开后按钮既不能是收起
// （一点就从上限跳回折叠态），也不能继续是"查看更多"（已在上限，点了没反应）。
describe("resolveVisibleCount — 显示条数", () => {
  it("总数在折叠阈值以内：全量显示，一条不藏", () => {
    expect(resolveVisibleCount(0, "preview")).toBe(0);
    expect(resolveVisibleCount(1, "preview")).toBe(1);
    // 边界：恰好等于阈值时全显示（"5 条以内全显示"）
    expect(resolveVisibleCount(LIST_PREVIEW_COUNT, "preview")).toBe(LIST_PREVIEW_COUNT);
  });

  it("超过阈值：折叠态只显示前 N 条", () => {
    expect(resolveVisibleCount(6, "preview")).toBe(5);
    expect(resolveVisibleCount(50, "preview")).toBe(5);
  });

  it("展开态显示到上限为止，不会超过上限", () => {
    expect(resolveVisibleCount(6, "expanded")).toBe(6);
    expect(resolveVisibleCount(LIST_EXPAND_CAP, "expanded")).toBe(LIST_EXPAND_CAP);
    expect(resolveVisibleCount(50, "expanded")).toBe(LIST_EXPAND_CAP);
  });

  it("查看全部态显示总量（弹窗场景）", () => {
    expect(resolveVisibleCount(50, "all")).toBe(50);
  });
});

describe("resolveRevealAction — 按钮三态", () => {
  it("阈值以内不需要按钮", () => {
    expect(resolveRevealAction(LIST_PREVIEW_COUNT, "preview")).toBeNull();
    expect(resolveRevealAction(1, "preview")).toBeNull();
    expect(resolveRevealAction(0, "preview")).toBeNull();
  });

  it("恰好第 6 条时出现'查看更多'，数字是**被隐藏的条数**而非总数", () => {
    // 这是修复前的老 bug：条件写 >= 5 且括号内外都印总数，
    // 导致 5 条时显示"查看更多(5)"——可 5 条本来就全在屏幕上，点了毫无意义。
    expect(resolveRevealAction(6, "preview")).toEqual({ kind: "expand", hidden: 1 });
    expect(resolveRevealAction(10, "preview")).toEqual({ kind: "expand", hidden: 5 });
  });

  it("总数不超过展开上限时，展开后给'收起'", () => {
    expect(resolveRevealAction(6, "expanded")).toEqual({ kind: "collapse" });
    expect(resolveRevealAction(LIST_EXPAND_CAP, "expanded")).toEqual({ kind: "collapse" });
  });

  it("总数超过展开上限时，展开后必须给'查看全部'出口而不是'收起'", () => {
    // 若这里返回 collapse，用户点一下会从 20 条直接跳回 5 条，
    // 中间的 15 条等于白展开；同时没有任何入口能触到剩余部分。
    expect(resolveRevealAction(LIST_EXPAND_CAP + 1, "expanded")).toEqual({
      kind: "openPicker",
      hidden: 1,
    });
    expect(resolveRevealAction(50, "expanded")).toEqual({
      kind: "openPicker",
      hidden: 50 - LIST_EXPAND_CAP,
    });
  });

  it("查看全部态下按钮回落为'收起'", () => {
    expect(resolveRevealAction(50, "all")).toEqual({ kind: "collapse" });
  });
});

// 展开状态是"记住"的，列表却会变短（删会话、项目被移除）。若不清理，
// 会留下"明明只剩 3 条却记着已展开"的隐性不一致——界面看着没问题，
// 但一旦再涨回 6 条就会直接显示展开态，跳过折叠这一步。
describe("归集状态与列表长度的联动", () => {
  it("列表缩到阈值以内后，展开态不再产生任何按钮", () => {
    // 这是清理 effect 的目标状态：回到 preview 语义
    expect(resolveRevealAction(3, "preview")).toBeNull();
    expect(resolveVisibleCount(3, "preview")).toBe(3);
  });

  it("清理后若列表重新涨过阈值，仍从折叠态开始", () => {
    const afterCleanup: ListReveal = "preview";
    expect(resolveVisibleCount(8, afterCleanup)).toBe(LIST_PREVIEW_COUNT);
    expect(resolveRevealAction(8, afterCleanup)).toEqual({
      kind: "expand",
      hidden: 3,
    });
  });
});
