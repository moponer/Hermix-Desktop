// 行内 code 的语法高亮**不加粗**（2026-10-24）。
//
// 行内 code（`null` / `true`）渲染成"底色药丸 + 语法高亮"，而调色板里
// keyword / function / boolean / constant 都是 fontWeight 600。底色已经占
// 了"这是代码"的重音，再叠一层字重就是两个重音互相抢 —— 正文里一段话五个
// 行内 code，就是五块"加粗带底色"的词（用户实机反馈："一部分重点加粗带
// 背景色的特殊词汇……界面有点乱"）。
//
// 代码块里没有底色，字重是区分关键字的唯一手段 → 照旧。
import { describe, it, expect } from "vitest";
import { highlightToReact } from "./codeHighlight";
import { renderToStaticMarkup } from "react-dom/server";

function render(node: React.ReactNode): string {
  return renderToStaticMarkup(<>{node}</>);
}

describe("highlightToReact 的 inline 模式", () => {
  it("行内：保留颜色，去掉字重", () => {
    const html = render(highlightToReact("null", "js", false, true));
    // 高亮确实跑了（有 token 色值）
    expect(/color:\s*#/.test(html)).toBe(true);
    // 但不加粗
    expect(html).not.toContain("font-weight:600");
    expect(html).not.toContain("font-weight: 600");
  });

  it("代码块：字重照旧（那里没有底色，字重是唯一区分）", () => {
    const html = render(highlightToReact("const x = 1;", "js", false, false));
    expect(html).toContain("font-weight:600");
  });
});
