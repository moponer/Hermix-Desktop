import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ClarifyCard } from "./ClarifyBubble";
import type { ChatMessage, ClarifyMessageMeta } from "../types";

function makeMessage(clarify: ClarifyMessageMeta | undefined): ChatMessage {
  return {
    id: "m1",
    role: "clarify",
    content: "",
    timestamp: new Date().toISOString(),
    clarify,
  } as ChatMessage;
}

describe("ClarifyCard", () => {
  it("renders the question and choice buttons when pending", () => {
    const msg = makeMessage({
      requestId: "r1",
      question: "选择方案?",
      choices: ["A", "B"],
    });
    render(<ClarifyCard message={msg} />);
    expect(screen.getByText("需要您确认")).toBeTruthy();
    expect(screen.getByText("选择方案?")).toBeTruthy();
    expect(screen.getByText("A")).toBeTruthy();
    expect(screen.getByText("B")).toBeTruthy();
  });

  it("disappears (renders nothing) once answered", () => {
    const msg = makeMessage({
      requestId: "r1",
      question: "选择方案?",
      choices: ["A", "B"],
      answer: "A",
    });
    const { container } = render(<ClarifyCard message={msg} />);
    expect(container.firstChild).toBeNull();
  });

  it("does not render the resolved '您的回答' footer when answered", () => {
    const msg = makeMessage({
      requestId: "r1",
      question: "选择方案?",
      choices: ["A", "B"],
      answer: "A",
    });
    render(<ClarifyCard message={msg} />);
    expect(screen.queryByText("您的回答")).toBeNull();
  });

  it("calls onAnswer with the chosen option", () => {
    const onAnswer = vi.fn();
    const msg = makeMessage({
      requestId: "r1",
      question: "选择方案?",
      choices: ["A", "B"],
    });
    render(<ClarifyCard message={msg} onAnswer={onAnswer} />);
    fireEvent.click(screen.getByText("B"));
    expect(onAnswer).toHaveBeenCalledWith("r1", "B");
  });
});
