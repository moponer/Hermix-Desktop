// 消息操作行 —— 真渲染测试。
//
// 2026-10-01 踩坑：`onRetryLastMessage` 在 MessageList / MessageBubble /
// TextBubble 三层都**声明了类型、传进了 JSX，却漏了解构**。tsc 报
// `TS2304 Cannot find name`，但那三层是纯透传，单测覆盖不到；真正暴露
// 的是浏览器里的 `ReferenceError: onRetryLastMessage is not defined`。
//
// 所以这里渲染**真实的 `AssistantMessage`**，而不是复制一份它的 JSX：
// 复制的那份永远测不到「prop 传进来但没解构」这类接线错误。
//
// ⚠ 必须 mock 掉 `MarkdownBody`：`AssistantMessage` 内部会渲染它，而
// satteri 是 NAPI 模块，**在 jsdom 下拿 DataView 做视图转换必炸**
// （`MarkdownBody.test.tsx` 开头就写明了「不要 jsdom」）。markdown 管线
// 本身由那个测试覆盖，本文件只关心操作行与 prop 接线。
vi.mock("./MarkdownBody", () => ({
  MarkdownBody: ({ content }: { content: string }) => <div data-testid="md">{content}</div>,
}));

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { AssistantMessage } from "./TextBubble";
import type { ChatMessage } from "../types";

function msg(over: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "a1",
    role: "assistant",
    content: "这是回答正文。",
    timestamp: "2026-10-01T14:18:00.000Z",
    ...over,
  } as ChatMessage;
}

beforeEach(() => {
  localStorage.clear();
});

describe("MessageActions 接在真实消息上", () => {
  it("传了 onRetryLastMessage → 渲染「重新生成」且点击触发回调", () => {
    const onRetry = vi.fn();
    render(<AssistantMessage message={msg()} onRetryLastMessage={onRetry} />);
    const btn = screen.getByRole("button", { name: "重新生成" });
    expect(btn).toBeInTheDocument();
    fireEvent.click(btn);
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("没传 onRetryLastMessage → **不渲染**该按钮（不留点了没反应的）", () => {
    render(<AssistantMessage message={msg()} />);
    expect(screen.queryByRole("button", { name: "重新生成" })).toBeNull();
  });

  it("复制按钮存在且可点", () => {
    render(<AssistantMessage message={msg()} />);
    expect(screen.getByRole("button", { name: "复制消息" })).toBeInTheDocument();
  });

  it("赞 / 踩 可点、互斥、选中态透出 aria-pressed", () => {
    render(<AssistantMessage message={msg()} />);
    const up = () => screen.getByRole("button", { name: "有帮助" });
    const down = () => screen.getByRole("button", { name: "没帮上忙" });

    expect(up().getAttribute("aria-pressed")).toBe("false");
    // 用 fireEvent 而不是裸 `.click()` —— 后者不走 act()，React 的状态
    // 更新不会在下一行断言前冲掉，读到的还是旧 DOM（第一次写这个测试
    // 就栽在这）。
    fireEvent.click(up());
    expect(up().getAttribute("aria-pressed")).toBe("true");
    expect(down().getAttribute("aria-pressed")).toBe("false");

    // 再点踩 → 赞被取消（互斥）
    fireEvent.click(down());
    expect(up().getAttribute("aria-pressed")).toBe("false");
    expect(down().getAttribute("aria-pressed")).toBe("true");
  });

  it("投票写进本机记录并能恢复（刷新后仍在）", () => {
    const { unmount } = render(<AssistantMessage message={msg()} />);
    fireEvent.click(screen.getByRole("button", { name: "有帮助" }));
    expect(JSON.parse(localStorage.getItem("hermix.messageVotes") ?? "{}")).toEqual({ a1: "up" });

    unmount();
    render(<AssistantMessage message={msg()} />);
    expect(screen.getByRole("button", { name: "有帮助" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("坏掉的 localStorage 不让消息渲染崩", () => {
    localStorage.setItem("hermix.messageVotes", "{ 这不是 JSON");
    expect(() => render(<AssistantMessage message={msg()} />)).not.toThrow();
    expect(screen.getByRole("button", { name: "有帮助" })).toBeInTheDocument();
  });

  it("时刻常驻（HH:MM），不靠 hover", () => {
    const { container } = render(<AssistantMessage message={msg()} />);
    const clock = container.querySelector(".message-action-clock");
    expect(clock).not.toBeNull();
    expect(clock!.textContent).toMatch(/^\d{2}:\d{2}$/);
  });

  it("空正文不渲染操作行（复制没有可复制的东西）", () => {
    const { container } = render(<AssistantMessage message={msg({ content: "   " })} />);
    expect(container.querySelector(".message-action-clock")).toBeNull();
  });
});
