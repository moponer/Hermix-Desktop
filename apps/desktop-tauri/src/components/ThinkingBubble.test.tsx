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
  it("默认收起：只渲染标题行，不渲染正文", () => {
    const { container } = render(<ReasoningBlock content="先解决 go.sum 写入被拦的问题" />);
    expect(screen.getByRole("button", { name: "思考" })).toBeInTheDocument();
    expect(container.querySelector(".tl-reasoning-row")).toBeNull();
    expect(screen.queryByText(/先解决 go.sum/)).not.toBeInTheDocument();
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
