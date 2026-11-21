/**
 * 过程折叠契约测试（2026-11-17 用户实机反馈）。
 *
 * 用户要的是：思考/执行过程（工具、推理、旁白）**按原来的顺序整体折成
 * 一个块**，正文一开始输出就直接显示在块外。截图里的毛病是运行中正文
 * 和一大串过程糊在一起。
 *
 * 这里锁两件事：
 * 1. `canFoldProcess` —— "正文开写即折"的判据（本次修复核心）；
 * 2. ReasoningBlock 自身的折叠契约（默认收起、active 展开、点击切换、
 *    空内容不渲染）—— 过程块展开后里面的每段思考仍按此规则呈现。
 */
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ReasoningBlock } from "./ThinkingBubble";
import { canFoldProcess } from "./messageParser";
import type { TimelineStep } from "../types";

// satteri 的 NAPI 在 jsdom 下会撞 DataView 视图转换错误（见
// MarkdownBody.test.tsx 的文件头注释）。这里测的是折叠壳，不是 markdown
// 渲染，把 MarkdownBody 替成纯文本即可。
vi.mock("./MarkdownBody", () => ({
  MarkdownBody: ({ content }: { content: string }) => <div>{content}</div>,
}));

const text = (content: string): TimelineStep => ({ kind: "text", content });
const reasoning = (content: string): TimelineStep => ({ kind: "reasoning", content, startedAt: "" });
const tools = (): TimelineStep => ({
  kind: "tools",
  events: [{ type: "tool_start", tool: "ls", arguments: "", created_at: "" }] as never,
});

describe("canFoldProcess（正文开写即折）", () => {
  it("轮次已 settled 时可折", () => {
    expect(canFoldProcess([reasoning("想"), text("答复")], true)).toBe(true);
  });

  it("运行中：正文尚未出现时不折（还在想/调工具，过程平铺）", () => {
    expect(canFoldProcess([reasoning("想"), tools()], false)).toBe(false);
    expect(canFoldProcess([reasoning("想"), text("  ")], false)).toBe(false);
  });

  it("运行中：收尾正文一开写就可折", () => {
    const steps = [reasoning("想"), tools(), text("先解决 go.sum 的问题")];
    expect(canFoldProcess(steps, false)).toBe(true);
  });

  it("旁白 causes 不误折：最后一段文字是空白时仍视为未开写", () => {
    const steps = [tools(), text("先看上次 tidy"), text("\n  \t")];
    expect(canFoldProcess(steps, false)).toBe(false);
  });

  it("没有 text step 的运行中不折", () => {
    expect(canFoldProcess([], false)).toBe(false);
    expect(canFoldProcess([reasoning("想")], false)).toBe(false);
  });
});

describe("ReasoningBlock 折叠契约", () => {
  it("默认收起：头行带单行预览，不渲染完整", () => {
    const { container } = render(<ReasoningBlock content="先解决 go.sum 写入被拦的问题" />);
    expect(screen.getByRole("button", { name: "思考" })).toBeInTheDocument();
    expect(container.querySelector(".tl-reasoning-row")).toBeNull();
    const preview = container.querySelector(".tl-reasoning-preview");
    expect(preview).not.toBeNull();
    expect(preview!.textContent).toContain("先解决 go.sum");
  });

  it("active 时自动展开正文", () => {
    const { container } = render(<ReasoningBlock content="正在核对 API 签名" active />);
    expect(screen.getByRole("button", { name: "思考中…" })).toBeInTheDocument();
    expect(container.querySelector(".tl-reasoning-row")).not.toBeNull();
  });

  it("点击标题切换展开/收起", () => {
    const { container } = render(<ReasoningBlock content="先看上次 tidy 的结果" />);
    const head = screen.getByRole("button", { name: "思考" });
    fireEvent.click(head);
    expect(container.querySelector(".tl-reasoning-row")).not.toBeNull();
    fireEvent.click(head);
    expect(container.querySelector(".tl-reasoning-row")).toBeNull();
  });

  it("空内容（含 think 标签残留）不渲染", () => {
    const { container } = render(<ReasoningBlock content="<think></think>  " />);
    expect(container.firstChild).toBeNull();
  });

  it("剥离 think / reasoning 包裹标签后展示正文", () => {
    const { container } = render(<ReasoningBlock content="<think>先并行处理</think>" active />);
    expect(container.textContent).toContain("先并行处理");
    expect(container.textContent).not.toContain("<think>");
  });
});

/**
 * 耗时定格契约（2026-11-18 用户实拍反馈）。
 *
 * 症状：思考已经结束、后面的思考还在推进，但**已结束那段**的秒数一直涨。
 * 根因是 spanMs 在没有 endedAt 时拿 Date.now() 当终点，而流式路径漏补
 * endedAt（实测某条消息 12 段里 11 段为 null）→ 每次重渲染都重算。
 * 这里钉住：段一结束就定格，之后再渲染也不变。
 */
describe("ReasoningBlock 耗时定格", () => {
  const START = 1_700_000_000_000;

  it("结束后定格，不随重渲染增长", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(START));
    const startedAt = new Date(START - 4_000).toISOString();
    const { container, rerender } = render(
      <ReasoningBlock content="想完了" active startedAt={startedAt} />,
    );
    // 进行中不显示数字（那会是个一直在跳的值）
    expect(container.querySelector(".tl-reasoning-count")).toBeNull();

    rerender(<ReasoningBlock content="想完了" active={false} startedAt={startedAt} />);
    expect(container.querySelector(".tl-reasoning-count")?.textContent).toBe("4s");

    // 后续两分钟里又发生了别的思考 —— 这一段的数字必须还是 4s
    vi.setSystemTime(new Date(START + 120_000));
    rerender(<ReasoningBlock content="想完了" active={false} startedAt={startedAt} />);
    expect(container.querySelector(".tl-reasoning-count")?.textContent).toBe("4s");
    vi.useRealTimers();
  });

  it("有 endedAt 时按真实起止算（重载历史）", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(START));
    const { container } = render(
      <ReasoningBlock
        content="历史段"
        startedAt={new Date(START - 9_000).toISOString()}
        endedAt={new Date(START - 2_000).toISOString()}
      />,
    );
    expect(container.querySelector(".tl-reasoning-count")?.textContent).toBe("7s");
    vi.useRealTimers();
  });
});