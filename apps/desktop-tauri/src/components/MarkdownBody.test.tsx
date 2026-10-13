// @vitest-environment node

import { describe, it, expect } from "vitest";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { hardenSoftLineBreaks } from "@hermix/preview";
import { autoCloseUnbalancedFences } from "../lib/markdownFence";

// 验证 MarkdownBody.tsx 替换后的核心 pipeline:
//   autoCloseUnbalancedFences → hardenSoftLineBreaks → markdownToHast → toJsxRuntime
// 不依赖 jsdom（satteri 的 NAPI 在 jsdom 下会撞 DataView 视图转换问题）

const FEATURES = { features: { gfm: true } } as const;

interface HastNode {
  type: string;
  tagName?: string;
  children?: HastNode[];
  value?: string;
}

function walk(node: HastNode, fn: (n: HastNode) => void): void {
  fn(node);
  for (const c of node.children ?? []) walk(c, fn);
}

function isElement(node: HastNode, name: string): boolean {
  return node.type === "element" && node.tagName === name;
}

function countElements(node: HastNode, name: string): number {
  let n = 0;
  walk(node, (m) => {
    if (isElement(m, name)) n++;
  });
  return n;
}

function findElement(node: HastNode, name: string): HastNode | undefined {
  if (isElement(node, name)) return node;
  for (const c of node.children ?? []) {
    const f = findElement(c, name);
    if (f) return f;
  }
  return undefined;
}

describe("MarkdownBody pipeline (satteri 替换后)", () => {
  it("平衡 fence → 解析为单段 paragraph + pre（代码）", () => {
    const md = autoCloseUnbalancedFences(`你好。\n\n\`\`\`ts\nconst x = 1;\n\`\`\``);
    const hardened = hardenSoftLineBreaks(md);
    const hast = markdownToHast(hardened, FEATURES);
    expect(hast.type).toBe("root");
    expect(countElements(hast, "pre")).toBe(1);
    expect(countElements(hast, "p")).toBe(1);
  });

  it("硬换行（双空格 + \\n）渲染成 <br>", () => {
    const md = `第一行  \n第二行`;
    const hast = markdownToHast(md, FEATURES);
    const p = findElement(hast, "p");
    expect(p).toBeTruthy();
    // hast 中应有一个 br 紧跟第一段文本
    expect(countElements(hast, "br")).toBe(1);
  });

  it("GFM table 渲染成 <table>", () => {
    const md = `| a | b |\n| - | - |\n| 1 | 2 |`;
    const hast = markdownToHast(md, FEATURES);
    const table = findElement(hast, "table");
    expect(table).toBeTruthy();
    // thead + tbody
    expect(countElements(hast, "thead")).toBe(1);
    expect(countElements(hast, "tbody")).toBe(1);
  });

  it("中文 + 链接 + 强调 + 斜体", () => {
    const md = `正文含 [外部](https://example.com) 与**加粗**与*斜体*。`;
    const hast = markdownToHast(md, FEATURES);
    expect(countElements(hast, "a")).toBe(1);
    expect(countElements(hast, "strong")).toBe(1);
    expect(countElements(hast, "em")).toBe(1);
  });

  it("完整 pipeline 不抛错（harden → satteri → toJsxRuntime）", () => {
    const md = `# 标题\n\n段落 [link](http://x) **bold**。\n\n\`\`\`js\nconst x = 1;\n\`\`\``;
    const balanced = autoCloseUnbalancedFences(md);
    const hardened = hardenSoftLineBreaks(balanced);
    const hast = markdownToHast(hardened, FEATURES);
    expect(() =>
      toJsxRuntime(hast, { Fragment, jsx, jsxs }),
    ).not.toThrow();
  });

  it("不平衡 fence 被前置修复", () => {
    const open = `\`\`\`ts\nconst x = 1;\nconst y = 2;`;
    const balanced = autoCloseUnbalancedFences(open);
    expect(balanced.endsWith("```")).toBe(true);
  });
});
