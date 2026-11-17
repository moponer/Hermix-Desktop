/**
 * ReasoningBlock 折叠行为测试。
 *
 * 落笔背景（2026-11-17）：用户实机反馈"思考过程的总结没按顺序折叠，
 * 和总结正文一起出现，很乱"。根因是模型把规划旁白写进 content（text
 * step），过去只有 reasoning step 有折叠 UI、text step 裸渲染。修复把
 * 过程旁白也接到同一个 ReasoningBlock 上，因此这里锁定它的折叠契约：
 * 默认收起、active 才自动展开、用户点过之后以用户为准、label 可定制、
 * 空内容不渲染。
 */
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ReasoningBlock } from "./ThinkingBubble";

// satteri 的 NAPI 在 jsdom 下会撞 DataView 视图转换错误（见
// MarkdownBody.test.tsx 的文件头注释）。这里测的是折叠壳，不是 markdown
// 渲染，把 MarkdownBody 替成纯文本即可。
vi.mock("./MarkdownBody", () => ({
  MarkdownBody: ({ content }: { content: string }) => <div>{content}</div>,
}));

describe("ReasoningBlock", () => {
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

  it("自定义 label 用于过程旁白（与思考同一折叠语言）", () => {
    render(<ReasoningBlock content="先并行核对 x/sys 的符号" label="过程" />);
    expect(screen.getByRole("button", { name: "过程" })).toBeInTheDocument();
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
