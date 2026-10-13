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

  it("html 渲染页面而不是落进代码视图", () => {
    expect(resolveKind("index.html")).toBe("html");
    expect(resolveKind("legacy.htm")).toBe("html");
    // 旧实现里 .html 被 detectLanguage 认成 markup → code，页面完全看不到
    expect(resolveKind("index.html")).not.toBe("code");
  });

  it("svg 是一张图，不是源码", () => {
    expect(resolveKind("logo.svg")).toBe("image");
  });

  it("OOXML 走浏览器内引擎，老二进制格式落 markitdown 文本", () => {
    expect(resolveKind("a.docx")).toBe("docx");
    expect(resolveKind("a.pptx")).toBe("pptx");
    expect(resolveKind("a.doc")).toBe("markdown");
    expect(resolveKind("a.ppt")).toBe("markdown");
    // .xls 虽是老二进制，但 SheetJS 读得动 BIFF，前端解析优于文本抽取
    expect(resolveKind("a.xls")).toBe("spreadsheet");
  });

  it("表格优先于 artifactKind：xlsx 不该被当成老格式", () => {
    expect(resolveKind("a.xlsx")).toBe("spreadsheet");
    expect(resolveKind("a.xlsx", "office")).toBe("spreadsheet");
  });
});

describe("applyViewMode", () => {
  it("文本视图只改 pdf 与 html", () => {
    expect(applyViewMode("pdf", "text")).toBe("markdown");
    expect(applyViewMode("html", "text")).toBe("code");
    expect(applyViewMode("pdf", "layout")).toBe("pdf");
    expect(applyViewMode("html", "layout")).toBe("html");
  });

  it("已有可读文本的类型不受视图模式影响", () => {
    expect(applyViewMode("docx", "text")).toBe("docx");
    expect(applyViewMode("markdown", "text")).toBe("markdown");
    expect(applyViewMode("code", "text")).toBe("code");
  });
});
