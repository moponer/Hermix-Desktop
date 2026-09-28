import { describe, it, expect } from "vitest";
import { resolveKind, applyViewMode } from "./resolveKind";

describe("resolveKind", () => {
  it("按扩展名分派到各自的渲染路径", () => {
    expect(resolveKind("README.md")).toBe("markdown");
    expect(resolveKind("shot.png")).toBe("image");
    expect(resolveKind("paper.pdf")).toBe("pdf");
    expect(resolveKind("book.xlsx")).toBe("spreadsheet");
    expect(resolveKind("main.rs")).toBe("code");
    expect(resolveKind("notes.txt")).toBe("text");
  });

  it("OOXML 走浏览器内引擎，只有前端无解的老格式才交给 officecli", () => {
    expect(resolveKind("a.docx")).toBe("docx");
    expect(resolveKind("a.pptx")).toBe("pptx");
    expect(resolveKind("a.doc")).toBe("office");
    expect(resolveKind("a.ppt")).toBe("office");
    // .xls 虽是老二进制，但 SheetJS 读得动 BIFF，前端解析优于拉起 officecli
    expect(resolveKind("a.xls")).toBe("spreadsheet");
  });

  it("表格优先于 office：xlsx 不该被送去 officecli", () => {
    expect(resolveKind("a.xlsx")).toBe("spreadsheet");
    expect(resolveKind("a.xlsx", "office")).toBe("spreadsheet");
  });
});

describe("applyViewMode", () => {
  it("文本视图只改 office 与 pdf", () => {
    expect(applyViewMode("office", "text")).toBe("markdown");
    expect(applyViewMode("pdf", "text")).toBe("markdown");
    expect(applyViewMode("office", "layout")).toBe("office");
    expect(applyViewMode("pdf", "layout")).toBe("pdf");
  });

  it("已有可读文本的类型不受视图模式影响", () => {
    expect(applyViewMode("docx", "text")).toBe("docx");
    expect(applyViewMode("markdown", "text")).toBe("markdown");
    expect(applyViewMode("code", "text")).toBe("code");
  });
});
