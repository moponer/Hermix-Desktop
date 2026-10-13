// exporters 单元测试：画布 HTML → xlsx 模型的纯解析层。
// 质量红线（officecli 教训）的协议侧钉死：
//  - colspan/rowspan 展开成真实网格坐标（合并覆盖位 = null）
//  - 表头检测按 <th> 语义，不猜第一行
//  - 数字列自动转数值（千分位剥离），文本保持字符串
//  - caption → 表名，非法字符剔除，无 caption → SheetN
//  - 无 <table> → null（不是空工作簿）
// ExcelJS 渲染层（modelToXlsxBytes）不做 jsdom 单测——动态 import 引擎
// 在 node 测试环境重且慢，由真机验收覆盖。
import { describe, it, expect } from "vitest";
import { htmlToXlsxModel } from "./exporters";

describe("htmlToXlsxModel", () => {
  it("简单表：行/列/表头语义齐全", () => {
    const model = htmlToXlsxModel(`<table>
      <tr><th>部门</th><th>营收</th></tr>
      <tr><td>华东</td><td>5,888</td></tr>
      <tr><td>华南</td><td>3,201</td></tr>
    </table>`);
    expect(model).not.toBeNull();
    expect(model!.sheets).toHaveLength(1);
    const s = model!.sheets[0];
    expect(s.header).toBe(true);
    expect(s.rows).toEqual([
      ["部门", "营收"],
      ["华东", 5888],
      ["华南", 3201],
    ]);
    expect(s.merges).toEqual([]);
  });

  it("colspan/rowspan 展开成网格坐标，覆盖位为 null", () => {
    const model = htmlToXlsxModel(`<table>
      <tr><td colspan="2">标题跨两列</td><td>B</td></tr>
      <tr><td rowspan="2">纵向占两行</td><td>x</td><td>y</td></tr>
      <tr><td>p</td><td>q</td></tr>
    </table>`);
    const s = model!.sheets[0];
    // 3 列 × 3 行规整网格
    expect(s.rows).toEqual([
      ["标题跨两列", null, "B"],
      ["纵向占两行", "x", "y"],
      [null, "p", "q"],
    ]);
    expect(s.merges).toEqual([
      { r1: 0, c1: 0, r2: 0, c2: 1 },
      { r1: 1, c1: 0, r2: 2, c2: 0 },
    ]);
    expect(s.header).toBe(false, "td 表不猜表头");
  });

  it("caption → 表名，非法字符剔除，超长截断 31", () => {
    const model = htmlToXlsxModel(`
      <table><caption>Q3: 营收[总]</caption><tr><td>1</td></tr></table>
      <table><caption>这是一个超过三十一个字符的表名吗让我数一数字符数到底够不够三十一啊</caption><tr><td>2</td></tr></table>
      <table><tr><td>3</td></tr></table>
    `);
    const [a, b, c] = model!.sheets;
    expect(a.name).toBe("Q3 营收总");
    expect(b.name.length).toBeLessThanOrEqual(31);
    expect(c.name).toBe("Sheet3");
  });

  it("无 <table> → null（不是空工作簿）", () => {
    expect(htmlToXlsxModel("<p>只有文字</p>")).toBeNull();
    expect(htmlToXlsxModel("")).toBeNull();
  });

  it("千分位数字转数值，百分比/货币保持字符串", () => {
    const model = htmlToXlsxModel(`<table>
      <tr><td>1,234.56</td><td>24%</td><td>¥5,888</td><td>-42</td><td>abc</td></tr>
    </table>`);
    const s = model!.sheets[0];
    expect(s.rows[0]).toEqual([1234.56, "24%", "¥5,888", -42, "abc"]);
  });

  it("多表 → 多 sheet", () => {
    const model = htmlToXlsxModel(`
      <table><caption>数据</caption><tr><td>1</td></tr></table>
      <table><tr><td>2</td></tr></table>
    `);
    expect(model!.sheets.map((s) => s.name)).toEqual(["数据", "Sheet2"]);
  });
});
