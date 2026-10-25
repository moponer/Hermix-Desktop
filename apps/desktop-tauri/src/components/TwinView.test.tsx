import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

// MarkdownBody 的 markdown→hast 在 jsdom 下缺 ArrayBuffer 依赖
//（satteri hast-reader），主对话测试同样 mock 掉——本文件只测接线。
vi.mock("./MarkdownBody", () => ({
  MarkdownBody: ({ content }: { content: string }) => <div data-testid="md">{content}</div>,
}));
import { TwinView } from "./TwinView";
import type { TwinInlineApi } from "../app/routes";

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

  it("她这一轮在跑时输入框与发送键都不可用", () => {
    renderTwin(makeTwin({ streaming: true }));
    expect(screen.getByLabelText("给助手阿黎发消息")).toBeDisabled();
    expect(screen.getByLabelText("发送")).toBeDisabled();
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
