// 「刚才发生」一行的措辞。
//
// 真实缺陷（2026-11-06 值守页截图）：右翼面板三行都是裸的 `terminal`，
// 用户看不出她跑的是什么命令。tool_start 帧本来就带 arguments，只是前端
// 没往下传。这里钉住"带参数就写参数、没参数不编造"的行为。
import { describe, expect, it } from "vitest";
import { TOOL_LABEL_MAX, toolStepLabel } from "./toolStepLabel";

describe("toolStepLabel", () => {
  it("命令优先：terminal 写出具体命令", () => {
    expect(toolStepLabel("terminal", { command: "git status" })).toBe(
      "terminal: git status",
    );
  });

  it("多行命令压成一行（行内换行会撑破单行读数）", () => {
    expect(toolStepLabel("terminal", { command: "$p = 1;\n Get-ChildItem" })).toBe(
      "terminal: $p = 1; Get-ChildItem",
    );
  });

  it("文件类工具写路径", () => {
    expect(toolStepLabel("read_file", { path: "src/App.tsx" })).toBe(
      "read_file: src/App.tsx",
    );
  });

  it("命令与路径同时存在时取命令", () => {
    expect(toolStepLabel("code_execution", { command: "pytest", path: "tests" })).toBe(
      "code_execution: pytest",
    );
  });

  it("参数缺失 / 形状不对时只留工具名，不编造", () => {
    expect(toolStepLabel("task_update")).toBe("task_update");
    expect(toolStepLabel("task_update", null)).toBe("task_update");
    expect(toolStepLabel("task_update", { note: "x" })).toBe("task_update");
    expect(toolStepLabel("task_update", { command: "   " })).toBe("task_update");
  });

  it("超长命令截断并加省略号，总长有界", () => {
    const out = toolStepLabel("terminal", { command: "x".repeat(500) });
    expect(out.length).toBe("terminal: ".length + TOOL_LABEL_MAX);
    expect(out.endsWith("…")).toBe(true);
  });
});
