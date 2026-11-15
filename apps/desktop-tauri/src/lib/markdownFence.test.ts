import { describe, it, expect } from "vitest";
import { autoCloseUnbalancedFences } from "./markdownFence";

describe("autoCloseUnbalancedFences", () => {
  it("returns balanced content untouched", () => {
    const input = "```ts\nconst x = 1;\n```\nhello";
    expect(autoCloseUnbalancedFences(input)).toBe(input);
  });

  it("appends a closing ``` when one opener has no closer", () => {
    const input = "```python\nprint('hi')\nstill in fence";
    const out = autoCloseUnbalancedFences(input);
    expect(out.endsWith("\n```")).toBe(true);
    // The original content must remain at the front.
    expect(out.startsWith(input)).toBe(true);
  });

  it("ignores inline ``` mentions in prose (does not count as fence opener)", () => {
    const input = "Use the ```-fence in markdown.\nThen prose continues.";
    // No line starts with 3+ backticks, so even count = 0 → untouched.
    expect(autoCloseUnbalancedFences(input)).toBe(input);
  });

  it("does not count single-backtick lines as fences", () => {
    const input = "`code` is inline\nstill one line of prose";
    expect(autoCloseUnbalancedFences(input)).toBe(input);
  });

  it("counts ~~~ tilde fences too", () => {
    const input = "~~~ruby\nputs :hi\nstill inside tilde fence";
    const out = autoCloseUnbalancedFences(input);
    expect(out.endsWith("\n```")).toBe(true);
  });

  it("preserves an already-closed tilde fence", () => {
    const input = "~~~ruby\nputs :hi\n~~~\nprose";
    expect(autoCloseUnbalancedFences(input)).toBe(input);
  });

  it("handles leading whitespace before fence marker", () => {
    // Indented fences count per CommonMark; we mirror that behavior.
    const input = "   ```ts\nlet y = 2;\n   still inside";
    const out = autoCloseUnbalancedFences(input);
    expect(out.endsWith("\n```")).toBe(true);
  });

  it("does not over-close when two openers stack before content", () => {
    // Two openings = even, no append.
    const input = "```a\nfirst\n```b\nnested language switch (CommonMark: closes first)";
    expect(autoCloseUnbalancedFences(input)).toBe(input);
  });

  it("real-world broken-LLM fixture from 2026-08-19 screenshot", () => {
    // Mimics the bug seen in the chat screenshots: a single ```python
    // opener, then prose the LLM forgot to close. We expect a clean
    // closing fence appended at the end.
    const input = [
      "```python",
      "def foo():",
      "    return 1",
      "",
      "Wait, validate passed and the file on disk starts with <html>.",
      "So the file on disk is correct.",
      "Let me check what could cause UI to not show.",
    ].join("\n");
    const out = autoCloseUnbalancedFences(input);
    expect(out.endsWith("\n```")).toBe(true);
    expect(autoCloseUnbalancedFences(out)).toBe(out); // idempotent
  });
});