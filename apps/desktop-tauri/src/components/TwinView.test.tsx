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
}));

function makeTwin(overrides: Partial<TwinInlineApi> = {}): TwinInlineApi {
  return {
    send: vi.fn(async () => true),
    stop: vi.fn(),
    streaming: false,
    turn: null,
    pending: [],
    resolve: vi.fn(),
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
  it("提交后投给她自己的发送器，不把人送去对话页", async () => {
    const twin = makeTwin();
    const { onOpenChat } = renderTwin(twin);

    const input = screen.getByLabelText("给助手阿黎发消息");
    fireEvent.change(input, { target: { value: "整理一下本周笔记" } });
    fireEvent.submit(input.closest("form")!);

    await waitFor(() =>
      expect(twin.send).toHaveBeenCalledWith(
        "整理一下本周笔记",
        expect.objectContaining({ permissionLevel: "default" }),
      ),
    );
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

  it("她这一轮在跑时输入框不可用，发送键换成停止", () => {
    renderTwin(makeTwin({ streaming: true }));
    expect(screen.getByLabelText("给助手阿黎发消息")).toBeDisabled();
    // 跑动中输入框是禁用的，暂停入口必须在主视线里：发送键就地变「停止」。
    // （此前这条断言还在找「发送」——那个按钮在这个状态下根本不渲染，
    //  2026-10-27 加停止态时测试没跟上。）
    expect(screen.queryByLabelText("发送")).toBeNull();
    expect(screen.getByLabelText("停止这一轮")).toBeInTheDocument();
  });

  it("本轮出错时把错误原文显示在输入口上方", () => {
    renderTwin(
      makeTwin({
        turn: { user: "跑一下", reply: "", activity: null, error: "API key 无效" },
      }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent("API key 无效");
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

    // 生效后这一轮下发 full —— 这就是"不再申请权限"的开关。
    const input = screen.getByLabelText("给助手阿黎发消息");
    fireEvent.change(input, { target: { value: "跑一下" } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() =>
      expect(twin.send).toHaveBeenCalledWith(
        "跑一下",
        expect.objectContaining({ permissionLevel: "full" }),
      ),
    );
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
    fireEvent.click(screen.getByText("数据"));
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
    fireEvent.click(screen.getByText("数据"));
    await waitFor(() => expect(circles()).toBe(4));
    expect(document.querySelector(".twin-ring__big")?.textContent).toBe("25%"); // 1/4
    expect(screen.getByText("本轮进度")).toBeInTheDocument();
  });
});
