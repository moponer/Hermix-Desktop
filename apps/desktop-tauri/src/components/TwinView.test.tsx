import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

// MarkdownBody 的 markdown→hast 在 jsdom 下缺 ArrayBuffer 依赖
//（satteri hast-reader），主对话测试同样 mock 掉——本文件只测接线。
vi.mock("./MarkdownBody", () => ({
  MarkdownBody: ({ content }: { content: string }) => <div data-testid="md">{content}</div>,
}));
import { TwinView } from "./TwinView";
import type { TwinInlineApi } from "../app/routes";
import * as api from "../api";

// 页面挂载时会拉三项真实数据；测试只关心对话部分，全部给空。
vi.mock("../api", () => ({
  getAutomationTasks: vi.fn(async () => []),
  getActiveAutomationRuns: vi.fn(async () => 0),
  getDailyMemoryList: vi.fn(async () => []),
  getSessionMessages: vi.fn(async () => ({ messages: [], hasMore: false })),
  // 落档时同步热更后端（放手立即生效）；这里只需要"有这么一个函数"。
  setSessionPermission: vi.fn(async () => {}),
}));

function makeTwin(overrides: Partial<TwinInlineApi> = {}): TwinInlineApi {
  return {
    stop: vi.fn(),
    streaming: false,
    turn: null,
    pending: [],
    resolve: vi.fn(),
    // 任务面板（2026-11-06 重构）：输入口只入队，拉取由 App 侧的调度驱动。
    queue: [],
    enqueueTask: vi.fn(),
    removeTask: vi.fn(),
    moveTask: vi.fn(),
    batch: null,
    requeueBatch: vi.fn(),
    reviseTask: vi.fn(() => null),
    patrol: null,
    autoIntervene: false,
    setAutoIntervene: vi.fn(),
    ...overrides,
  };
}

function renderTwin(twin: TwinInlineApi, onOpenChat = vi.fn()) {
  render(
    <TwinView
      sessions={[]}
      onOpenChat={onOpenChat}
      onOpenSession={vi.fn()}
      onDeleteSession={vi.fn()}
      twin={twin}
    />,
  );
  return { onOpenChat };
}

beforeEach(() => {
  vi.clearAllMocks();
  // 权限模式落在 storage 里：不清会把上一个用例的 full 带进下一个。
  window.localStorage.clear();
});

describe("TwinView 内联对话", () => {
  it("提交即入队（不是直接开跑），也不把人送去对话页", () => {
    const twin = makeTwin();
    const { onOpenChat } = renderTwin(twin);

    const input = screen.getByLabelText("下达一条需求给助手阿黎");
    fireEvent.change(input, { target: { value: "整理一下本周笔记" } });
    fireEvent.submit(input.closest("form")!);

    expect(twin.enqueueTask).toHaveBeenCalledWith("整理一下本周笔记");
    // 草稿清空：入队成功才有清空，否则用户的内容会凭空消失。
    expect((input as HTMLInputElement).value).toBe("");
    expect(onOpenChat).not.toHaveBeenCalled();
  });

  it("本轮回复就地显示", () => {
    renderTwin(
      makeTwin({
        turn: { user: "在吗", reply: "在，说吧。", activity: null, error: null },
      }),
    );
    expect(screen.getByText("在，说吧。")).toBeTruthy();
    expect(screen.getByText("你")).toBeTruthy();
  });

  it("未决审批就地给出允许/拒绝，点击即结算", () => {
    const twin = makeTwin({
      pending: [{ id: "req-1", kind: "approval", tool: "shell", reason: "删除已合并分支" }],
    });
    renderTwin(twin);

    // 出现待审批会自动切到数据态，审批块在右翼与中轴各有一份，点哪份都结算
    expect(screen.getAllByText(/shell/).length).toBeGreaterThan(0);
    fireEvent.click(screen.getAllByText("允许")[0]);
    expect(twin.resolve).toHaveBeenCalledWith("req-1", "approved");
  });

  it("澄清有选项时点选项作答", () => {
    const twin = makeTwin({
      pending: [
        { id: "c-1", kind: "clarify", question: "用哪个方案？", choices: ["稳妥", "激进"] },
      ],
    });
    renderTwin(twin);

    expect(screen.getAllByText("用哪个方案？").length).toBeGreaterThan(0);
    fireEvent.click(screen.getByText("稳妥"));
    expect(twin.resolve).toHaveBeenCalledWith("c-1", "approved", "稳妥");
  });

  it("她这一轮在跑时输入框仍可用：下达的需求先入队，停止是另一个按钮", () => {
    // 2026-11-06 实报：输出没结束时输入框被禁用，人只能干等。
    // 现在"下达"与"停止"并存，跑着也能排需求。
    const twin = makeTwin({ streaming: true });
    renderTwin(twin);
    const input = screen.getByLabelText("下达一条需求给助手阿黎");
    expect(input).not.toBeDisabled();
    fireEvent.change(input, { target: { value: "顺便把周报写了" } });
    fireEvent.submit(input.closest("form")!);
    expect(twin.enqueueTask).toHaveBeenCalledWith("顺便把周报写了");
    expect(screen.getByLabelText("停止这一轮")).toBeInTheDocument();
  });

  it("同一句需求已在面板里时如实提示，不假装收下", () => {
    const twin = makeTwin({
      queue: [{ id: "t1", text: "整理笔记", createdAt: "" }],
    });
    renderTwin(twin);
    const input = screen.getByLabelText("下达一条需求给助手阿黎");
    fireEvent.change(input, { target: { value: "整理笔记" } });
    fireEvent.submit(input.closest("form")!);
    expect(twin.enqueueTask).not.toHaveBeenCalled();
    expect(screen.getByText("这条已经在面板里了")).toBeInTheDocument();
  });

  it("本轮出错时把错误原文显示在输入口上方", () => {
    renderTwin(
      makeTwin({
        turn: { user: "跑一下", reply: "", activity: null, error: "API key 无效" },
      }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent("API key 无效");
  });

  it("没有错误就不印错误条（旧版「原因见下方错误」配空白就是这条）", () => {
    renderTwin(
      makeTwin({
        turn: { user: "跑一下", reply: "在跑", activity: null, error: null },
      }),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("TwinView 权限模式", () => {
  it("两档常驻可见，默认态选中「默认 · 需授权」，且不显示风险条", () => {
    renderTwin(makeTwin());
    expect(screen.getByRole("button", { name: "默认 · 需授权" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "放手 · 无审批" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    expect(screen.queryByText(/都会直接执行/)).toBeNull();
  });

  it("进放手模式要两下：第一下只说明代价，第二下才生效并挂风险条", async () => {
    const twin = makeTwin();
    renderTwin(twin);

    fireEvent.click(screen.getByRole("button", { name: "放手 · 无审批" }));
    // 第一下不生效：选项被换成"风险 + 确认"。
    expect(screen.getByText("放手后不再申请权限")).toBeInTheDocument();
    expect(screen.queryByText(/都会直接执行/)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "确认放手" }));
    expect(await screen.findByText(/都会直接执行/)).toBeInTheDocument();

    // 落档即通知后端：不等下一条消息重武装，正在跑的这一轮也不再弹卡
    // （2026-11-06 实报「点了放手还在问」）。
    expect(api.setSessionPermission).toHaveBeenCalledWith("twin:ali", "full");

    // 下达需求只入队；真正下发时 App 侧现读 localStorage 里的档位
    // （`pullTwinTasks`），所以这里只需确认入队成功。
    const input = screen.getByLabelText("下达一条需求给助手阿黎");
    fireEvent.change(input, { target: { value: "跑一下" } });
    fireEvent.submit(input.closest("form")!);
    expect(twin.enqueueTask).toHaveBeenCalledWith("跑一下");
  });

  it("确认态可以取消，取消后仍是默认模式", () => {
    renderTwin(makeTwin());
    fireEvent.click(screen.getByRole("button", { name: "放手 · 无审批" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.getByRole("button", { name: "默认 · 需授权" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.queryByText(/都会直接执行/)).toBeNull();
  });

  it("风险条上的「切回默认」一键回安全侧", async () => {
    renderTwin(makeTwin());
    fireEvent.click(screen.getByRole("button", { name: "放手 · 无审批" }));
    fireEvent.click(screen.getByRole("button", { name: "确认放手" }));
    await screen.findByText(/都会直接执行/);

    fireEvent.click(screen.getByRole("button", { name: "切回默认" }));
    await waitFor(() => expect(screen.queryByText(/都会直接执行/)).toBeNull());
    expect(screen.getByRole("button", { name: "默认 · 需授权" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });
});

describe("TwinView 数据态的环", () => {
  // 环上有四道弧：外框 / 轨道 / 进度 / 内圈虚线。没有进度时只该有三道 ——
  // `strokeDasharray="0 C"` 配 `strokeLinecap="round"` 会在起点留下一个圆帽，
  // 于是读数写着 00、环上 12 点方向却亮着一点，看起来像"已经在跑"。
  const circles = () => document.querySelectorAll(".twin-ring svg circle").length;

  afterEach(() => {
    vi.mocked(api.getAutomationTasks).mockResolvedValue([]);
  });

  it("没有在跑的任务：不画进度弧（0% 不留圆点）", async () => {
    renderTwin(makeTwin());
    fireEvent.click(screen.getByText("状态"));
    await waitFor(() => expect(circles()).toBe(3));
  });

  it("有在跑的任务：进度弧按 run_count / max_runs 出现", async () => {
    vi.mocked(api.getAutomationTasks).mockResolvedValue([
      {
        id: "t1",
        name: "盘前简报",
        prompt: "跑一遍",
        schedule: "0 8 * * *",
        enabled: true,
        run_count: 1,
        max_runs: 4,
        last_status: "running",
        history: [],
        created_at: "2026-10-01T00:00:00Z",
        updated_at: "2026-10-01T00:00:00Z",
      },
    ]);
    renderTwin(makeTwin());
    fireEvent.click(screen.getByText("状态"));
    await waitFor(() => expect(circles()).toBe(4));
    expect(document.querySelector(".twin-ring__big")?.textContent).toBe("25%"); // 1/4
    expect(screen.getByText("本轮进度")).toBeInTheDocument();
  });
});

describe("TwinView 任务面板", () => {
  // 需求没有"类型"字段：是不是同一件事由阿黎自己判断（见 twinQueue 注释）。
  const task = (id: string, text: string) => ({ id, text, createdAt: "" });

  it("待拉取按顺序列出：可调优先级、可删除", () => {
    const twin = makeTwin({
      queue: [task("t1", "整理笔记"), task("t2", "写周报")],
    });
    renderTwin(twin);
    fireEvent.click(screen.getByText("任务面板"));

    expect(screen.getByText("整理笔记")).toBeInTheDocument();
    // 队首已是最高优先级：上移必须不可用（否则顺序语义就假了）。
    expect(screen.getByLabelText("提升优先级：整理笔记")).toBeDisabled();
    fireEvent.click(screen.getByLabelText("降低优先级：整理笔记"));
    expect(twin.moveTask).toHaveBeenCalledWith("t1", 1);

    fireEvent.click(screen.getByLabelText("删除需求：写周报"));
    expect(twin.removeTask).toHaveBeenCalledWith("t2");
  });

  it("被拉取的需求不再显示在面板里，只留一行本批读数", () => {
    renderTwin(
      makeTwin({
        batch: {
          id: "b1",
          tasks: [task("t9", "刷完超星课程")],
          startedAt: 1,
          status: "running",
        },
      }),
    );
    fireEvent.click(screen.getByText("任务面板"));

    // 面板只回答"还没开工的有哪些"：拉走的那条不在这里再列一遍。
    expect(screen.getByText(/面板是空的/)).toBeInTheDocument();
    expect(screen.queryByText("刷完超星课程")).toBeNull();
    // 但执行情况不能凭空消失：一行读数指向状态页。
    // （同一读数也会出现在批次气泡里 —— getAllByText 断言"存在"而非唯一。）
    expect(screen.getAllByText(/1 条 · 执行中/).length).toBeGreaterThan(0);
  });

  it("状态页照登她的归档判断（原文，前端不加工）", () => {
    renderTwin(
      makeTwin({
        turn: {
          user: "两条需求",
          reply:
            "【归档】刷课程 → 并入 超星学习\n【归档】整理桌面 → 新开 桌面整理\n开始执行……",
          activity: null,
          error: null,
        },
      }),
    );
    fireEvent.click(screen.getByText("状态"));
    expect(screen.getByText("刷课程 → 并入 超星学习")).toBeInTheDocument();
    expect(screen.getByText("整理桌面 → 新开 桌面整理")).toBeInTheDocument();
  });

  it("她没写归档行时如实说「没写」，不替她补分类", () => {
    renderTwin(
      makeTwin({
        turn: { user: "两条需求", reply: "都做完了。", activity: null, error: null },
      }),
    );
    fireEvent.click(screen.getByText("状态"));
    expect(screen.getByText("没写")).toBeInTheDocument();
  });

  it("失败批次给「放回面板」入口（动过手就不自动回队）", () => {
    const twin = makeTwin({
      batch: {
        id: "b3",
        tasks: [task("t1", "整理笔记")],
        startedAt: 1,
        status: "failed",
        error: "没发出去",
      },
    });
    renderTwin(twin);
    fireEvent.click(screen.getByText("任务面板"));
    fireEvent.click(screen.getByText("把这一批放回面板"));
    expect(twin.requeueBatch).toHaveBeenCalled();
  });

  it("待补充的需求：给出补充 / 取消两条路，且不混进待拉取", () => {
    const twin = makeTwin({
      queue: [
        { id: "t1", text: "整理笔记", createdAt: "" },
        {
          id: "t2",
          text: "把那个弄一下",
          createdAt: "",
          blocked: true,
          blockReason: "缺目标文件",
          returns: 2,
        },
      ],
    });
    renderTwin(twin);
    fireEvent.click(screen.getByText("任务面板"));

    expect(screen.getByText(/缺：缺目标文件/)).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("补充需求：把那个弄一下"));
    expect(twin.reviseTask).toHaveBeenCalledWith("t2");
    fireEvent.click(screen.getByLabelText("取消需求：把那个弄一下"));
    expect(twin.removeTask).toHaveBeenCalledWith("t2");
  });

  it("状态页显示上一次巡查的读数与异常（真值，不是「一切正常」）", () => {
    renderTwin(
      makeTwin({
        patrol: {
          at: Date.now(),
          queue: 2,
          blocked: 1,
          tickets: { running: 1, done: 2, failed: 0 },
          findings: [
            { code: "blocked", level: "warn", text: "1 条需求缺目标，等你补充或取消" },
          ],
        },
      }),
    );
    fireEvent.click(screen.getByText("状态"));
    expect(screen.getByText(/队列 2 · 待补充 1/)).toBeInTheDocument();
    // 右翼「等你决定」里也有同一条建议（带按钮），所以用 getAllByText。
    expect(screen.getAllByText("1 条需求缺目标，等你补充或取消").length).toBeGreaterThan(0);
  });

  it("自动干预开关默认关，点一下交给后端落档（唯一能让她自己动手的开关）", () => {
    const twin = makeTwin();
    renderTwin(twin);
    fireEvent.click(screen.getByText("状态"));
    const btn = screen.getByRole("button", { name: "异常时让她自己处理（默认关）" });
    expect(btn).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(btn);
    expect(twin.setAutoIntervene).toHaveBeenCalledWith(true);
  });

  it("拉取失败把原始原因写在批次上（不是「见下方错误」）", () => {
    renderTwin(
      makeTwin({
        batch: {
          id: "b2",
          tasks: [task("t1", "整理笔记")],
          startedAt: 1,
          status: "failed",
          error: "后端 500",
        },
      }),
    );
    fireEvent.click(screen.getByText("任务面板"));
    expect(screen.getByRole("alert")).toHaveTextContent("后端 500");
  });
});
