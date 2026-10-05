// @vitest-environment node

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { linkifyChildren } from "./MarkdownBody";

/**
 * 行内路径链接的显示契约。
 *
 * 正文里 LLM 写的是一整串绝对路径，但**显示**只能是文件名 —— 铺满整行
 * 会把句子撕成两段，读者真正要找的是 `db1.py` 本身。目录信息不能丢：
 * title 属性仍是完整路径，悬停可见、点击也仍按完整路径解析。
 *
 * 图标尺寸锁 11px：等于 14px 正文的 cap 高度，下沿坐在基线上、顶不越 cap
 * 线（13px 会高出 2.8px）。实测数据见 components-prose.css。
 *
 * 走 node 环境而非 jsdom：satteri 是 NAPI 模块，jsdom 下会撞 DataView
 * 视图转换问题（见 MarkdownBody.test.tsx 顶部说明）。本用例只调
 * linkifyChildren，不经过 satteri 解析。
 */

const noop = () => {};

function render(text: string): string {
  return renderToStaticMarkup(<>{linkifyChildren(text, noop)}</>);
}

describe("行内路径链接（PathLink）", () => {
  it("只显示文件名，完整路径留给 title", () => {
    const html = render("改完了 F:\\spiker\\hermix\\db1.py 这个文件");

    // 可见文本 = 文件名
    expect(html).toContain(">db1.py<");
    // 完整路径仍在 title 里（悬停可见 + 点击按它解析）
    expect(html).toContain('title="打开 F:\\spiker\\hermix\\db1.py"');
  });

  it("图标 11px 且不越出 cap 线", () => {
    const html = render("改完了 F:\\spiker\\hermix\\db1.py 这个文件");
    expect(html).toContain('width="11"');
    expect(html).not.toContain('width="13"');
  });

  it("正斜杠路径同样只取文件名", () => {
    const html = render("see apps/desktop-tauri/src/App.tsx for details");
    expect(html).toContain(">App.tsx<");
    expect(html).not.toContain(">apps/desktop-tauri/src/App.tsx<");
  });

  it("裸文件名（无目录）原样显示，不做目录改写", () => {
    const html = render("改 App.tsx 的登录逻辑");
    expect(html).toContain(">App.tsx<");
    expect(html).toContain("md-path-link--bare");
  });
});
