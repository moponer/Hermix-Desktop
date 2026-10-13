// exporters docx IR 层单测：画布 HTML → DocxBlock[] 的纯解析。
// 钉死的契约：
//  - 标题层级 H1-H6 语义保留（不猜字号）
//  - strong/em 嵌套继承（<strong><em> 双开）
//  - 列表嵌套 → level 递进；li 内子列表不重复出项
//  - 表格 <th> 语义表头 + 原生 colSpan/rowSpan 透传
//  - slide 几何在 slide 块之间插 pagebreak；a4 不插
//  - style/script 不进 IR
import { describe, it, expect } from "vitest";
import { htmlToDocxBlocks } from "./exporters";

describe("htmlToDocxBlocks", () => {
  it("标题/段落/加粗斜体语义", () => {
    const blocks = htmlToDocxBlocks(
      "<h1>季度汇报</h1><p>营收 <strong>增长</strong>，<em>利润</em>稳定，<strong><em>双开</em></strong>。",
      "a4",
    );
    expect(blocks).toEqual([
      { kind: "heading", level: 1, spans: [{ text: "季度汇报" }] },
      {
        kind: "para",
        spans: [
          { text: "营收 " },
          { text: "增长", bold: true },
          { text: "，" },
          { text: "利润", italic: true },
          { text: "稳定，" },
          { text: "双开", bold: true, italic: true },
          { text: "。" },
        ],
      },
    ]);
  });

  it("列表嵌套 → level 递进，子列表不重复出项", () => {
    const blocks = htmlToDocxBlocks(
      "<ul><li>一级<ul><li>二级</li></ul></li><li>另一条</li></ul>",
      "a4",
    );
    expect(blocks).toHaveLength(1);
    const list = blocks[0];
    expect(list.kind).toBe("list");
    if (list.kind !== "list") return;
    expect(list.ordered).toBe(false);
    expect(list.items).toEqual([
      { level: 0, spans: [{ text: "一级" }] },
      { level: 1, spans: [{ text: "二级" }] },
      { level: 0, spans: [{ text: "另一条" }] },
    ]);
  });

  it("表格：<th> 语义表头 + colSpan/rowSpan 透传", () => {
    const blocks = htmlToDocxBlocks(
      `<table><tr><th>部门</th><th colspan="2">营收</th></tr>
       <tr><td>华东</td><td>1</td><td>2</td></tr></table>`,
      "a4",
    );
    expect(blocks).toHaveLength(1);
    const t = blocks[0];
    expect(t.kind).toBe("table");
    if (t.kind !== "table") return;
    expect(t.header).toBe(true);
    expect(t.rows[0][0]).toMatchObject({ header: true, colSpan: 1, rowSpan: 1 });
    expect(t.rows[0][1]).toMatchObject({ header: true, colSpan: 2 });
    expect(t.rows[1][0]).toMatchObject({ header: false, spans: [{ text: "华东" }] });
  });

  it("slide 几何：slide 块之间插 pagebreak；a4 不插", () => {
    const html = "<slide><h1>P1</h1></slide><slide><h1>P2</h1></slide><slide><h1>P3</h1></slide>";
    const slides = htmlToDocxBlocks(html, "slide16x9");
    const breaks = slides.filter((b) => b.kind === "pagebreak");
    expect(breaks).toHaveLength(2);

    const doc = htmlToDocxBlocks(
      "<section><h1>C1</h1></section><section><h1>C2</h1></section>",
      "a4",
    );
    expect(doc.filter((b) => b.kind === "pagebreak")).toHaveLength(0);
  });

  it("style/script 不进 IR；br 转换行符", () => {
    const blocks = htmlToDocxBlocks(
      "<style>.x{color:red}</style><p>第一行<br>第二行</p><script>evil()</script>",
      "a4",
    );
    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe("para");
    if (blocks[0].kind !== "para") return;
    expect(blocks[0].spans.map((s) => s.text).join("")).toBe("第一行\n第二行");
  });

  it("有序列表按 DOM 顺序透传（编号由渲染层生成）", () => {
    const blocks = htmlToDocxBlocks(
      "<ol><li>第一步</li><li>第二步</li></ol>",
      "a4",
    );
    const list = blocks[0];
    expect(list.kind).toBe("list");
    if (list.kind !== "list") return;
    expect(list.ordered).toBe(true);
    expect(list.items.map((i) => i.spans[0].text)).toEqual(["第一步", "第二步"]);
  });
});
