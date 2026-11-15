// 任务面板的队列语义。
//
// 2026-11-06 重构：输入口从"聊天"变成"下达需求"——需求入队，她按面板顺序
// 拉取开工。**归类不在这里做**：两条需求是不是同一件事由阿黎自己判断
// （她写 `【归档】… → 并入/新开 …`），前端只负责排队、限量、送达。
// 这里钉住三件容易写错的事：
//   ① 取哪几条只由用户排的顺序决定（不跳过、不重排）；
//   ② 被拉取的条目必须离开队列（面板只显示待拉取）；
//   ③ 归档行只摘原文，不反推、不伪造。
import { describe, expect, it } from "vitest";
import {
  ARCHIVE_MARK,
  BATCH_MAX,
  batchPrompt,
  enqueue,
  makeTask,
  matchReturned,
  moveTask,
  returnLines,
  takeBatch,
  verdictLines,
} from "./twinQueue";

const mk = (text: string, now = 1_700_000_000_000) => makeTask(text, now);

describe("enqueue", () => {
  it("追加到队尾，保持入队顺序", () => {
    const list = enqueue([], mk("整理笔记"));
    const next = enqueue(list, mk("写周报"));
    expect(next.map((t) => t.text)).toEqual(["整理笔记", "写周报"]);
  });

  it("空文本与完全重复的需求不入队（连点两下不该出现两条）", () => {
    const list = enqueue([], mk("整理笔记"));
    expect(enqueue(list, mk("   "))).toBe(list);
    expect(enqueue(list, mk("整理笔记"))).toBe(list);
  });
});

describe("moveTask", () => {
  it("上移提升优先级，越界不动", () => {
    const list = [mk("A"), mk("B"), mk("C")];
    expect(moveTask(list, list[1].id, -1).map((t) => t.text)).toEqual(["B", "A", "C"]);
    expect(moveTask(list, list[0].id, -1)).toBe(list);
    expect(moveTask(list, list[2].id, 1)).toBe(list);
    expect(moveTask(list, "nope", 1)).toBe(list);
  });
});

describe("takeBatch", () => {
  it("按面板顺序取队首若干条，取走的不再留在队列", () => {
    const list = [mk("A"), mk("B"), mk("C")];
    const { batch, rest } = takeBatch(list, 2);
    expect(batch.map((t) => t.text)).toEqual(["A", "B"]);
    expect(rest.map((t) => t.text)).toEqual(["C"]);
  });

  it("不按「类型」合并：类型完全不同也照面板顺序取（归她判断）", () => {
    // 上一版在这里按关键词分类再合批，用户当场指出归类是错的。
    const list = [mk("把超星课程刷完"), mk("写一份周报"), mk("整理桌面")];
    expect(takeBatch(list).batch.map((t) => t.text)).toEqual([
      "把超星课程刷完",
      "写一份周报",
      "整理桌面",
    ]);
  });

  it("有上限，不会把整个队列一次拉空", () => {
    const list = Array.from({ length: 10 }, (_, i) => mk(`任务${i}`));
    expect(takeBatch(list).batch).toHaveLength(BATCH_MAX);
    expect(takeBatch(list).rest).toHaveLength(10 - BATCH_MAX);
  });

  it("空队列不产出空批次", () => {
    const { batch, rest } = takeBatch([]);
    expect(batch).toHaveLength(0);
    expect(rest).toHaveLength(0);
  });
});

describe("blocked（她退回的需求）", () => {
  it("blocked 不参与拉取，但仍留在面板顺序里", () => {
    const list = [mk("A"), { ...mk("B"), blocked: true }, mk("C")];
    const { batch, rest } = takeBatch(list, 2);
    expect(batch.map((t) => t.text)).toEqual(["A", "C"]);
    expect(rest.map((t) => t.text)).toEqual(["B"]);
  });

  it("全被退回时不产出批次（不会反复拉走再退回）", () => {
    const list = [{ ...mk("A"), blocked: true }, { ...mk("B"), blocked: true }];
    expect(takeBatch(list).batch).toHaveLength(0);
  });
});

describe("returnLines / matchReturned", () => {
  it("摘退回行（含项目符号）", () => {
    const reply = "先判断了一下。\n【退回】那个事 → 缺 目标文件\n- 【退回】另一件 → 缺 账号\n开始做其它的。";
    expect(returnLines(reply)).toEqual(["那个事 → 缺 目标文件", "另一件 → 缺 账号"]);
  });

  it("按需求原文对齐，→ 后面是原因", () => {
    const tasks = [mk("把超星剩余课程刷完"), mk("整理桌面")];
    const { matched, unmatched } = matchReturned(tasks, ["把超星剩余课程刷完 → 缺 登录账号"]);
    expect(matched).toHaveLength(1);
    expect(matched[0].task.text).toBe("把超星剩余课程刷完");
    expect(matched[0].reason).toBe("缺 登录账号");
    expect(unmatched).toEqual([]);
  });

  it("对不上的行留在 unmatched，不张冠李戴", () => {
    const tasks = [mk("把超星剩余课程刷完")];
    const { matched, unmatched } = matchReturned(tasks, ["那个事 → 缺 路径", "别的 → 缺 X"]);
    expect(matched).toHaveLength(0);
    expect(unmatched).toHaveLength(2);
  });

  it("本批只有一条、只有一行退回时，按要点归给它", () => {
    const tasks = [mk("把超星剩余课程刷完")];
    const { matched } = matchReturned(tasks, ["刷课 → 缺 账号"]);
    expect(matched).toHaveLength(1);
    expect(matched[0].reason).toBe("缺 账号");
  });

  it("同一需求不会被两行退回重复占用", () => {
    const tasks = [mk("把超星剩余课程刷完")];
    const { matched, unmatched } = matchReturned(tasks, [
      "把超星剩余课程刷完 → 缺 账号",
      "把超星剩余课程刷完 → 缺 课程名",
    ]);
    expect(matched).toHaveLength(1);
    expect(unmatched).toHaveLength(1);
  });
});

describe("batchPrompt", () => {
  it("编号清单 + 先判断再动手 + 归档格式", () => {
    const p = batchPrompt([mk("刷完超星课程"), mk("把作业交了")]);
    expect(p).toContain("本批 2 条");
    expect(p).toContain("1. 刷完超星课程");
    expect(p).toContain("2. 把作业交了");
    // 判断依据是"当前正在做的任务"，不是关键词表。
    expect(p).toContain("当前正在做的任务");
    expect(p).toContain("并入同一任务线");
    expect(p).toContain(ARCHIVE_MARK);
    expect(p).toContain("每条单独回报结果");
  });

  it("空批次给空串（不产出半截指令）", () => {
    expect(batchPrompt([])).toBe("");
  });
});

describe("verdictLines", () => {
  it("摘出她自己写的归档行（原文，含项目符号也能认）", () => {
    const reply = [
      "先看了一下面板上这两条。",
      "【归档】刷完超星课程 → 并入 超星学习",
      "- 【归档】把作业交了 → 并入 超星学习",
      "【归档】整理本周笔记 → 新开 笔记归档",
      "接着我开始执行……",
    ].join("\n");
    expect(verdictLines(reply)).toEqual([
      "刷完超星课程 → 并入 超星学习",
      "把作业交了 → 并入 超星学习",
      "整理本周笔记 → 新开 笔记归档",
    ]);
  });

  it("没写归档就返回空数组，不拿关键词反推分类", () => {
    expect(verdictLines("好的，我先把课程刷完。")).toEqual([]);
    expect(verdictLines("")).toEqual([]);
  });

  it("标记必须出现在行首（正文里提到不算）", () => {
    expect(verdictLines("她说 【归档】A → 新开 B")).toEqual([]);
  });

  it("条数有上限，防止模型刷出一长串", () => {
    const many = Array.from({ length: 40 }, (_, i) => `${ARCHIVE_MARK}需求${i} → 新开 T`).join("\n");
    expect(verdictLines(many)).toHaveLength(12);
  });
});
