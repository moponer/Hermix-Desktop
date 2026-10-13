// 表格保真模型的回归测试。
//
// 黄金样本 `__fixtures__/styled-foreign.xlsx` 由 **xlsx-js-style**（另一个
// 引擎）写出，不是本项目用的 ExcelJS 写的 —— 这样测的是"我们的读取层能
// 不能还原外来文件的格式"，而不是同库往返的自证。样本覆盖：字体（粗体/
// 斜体/字号/字体名/颜色）、填充、边框、对齐、数字格式、列宽、行高、合并。

import { describe, expect, it, beforeAll } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { FidelityLevel, SheetModel } from "./workbook";
import {
  applyTint,
  colName,
  colWidthPx,
  parseRange,
  parseXlsx,
  resolveColor,
  rowHeightPx,
  themeColor,
} from "./workbook";

function fixture(): Uint8Array {
  return new Uint8Array(readFileSync(resolve(__dirname, "__fixtures__/styled-foreign.xlsx")));
}

describe("颜色解析", () => {
  it("ARGB 丢掉 alpha 位取 RGB", () => {
    expect(resolveColor({ argb: "FF4472C4" })).toBe("#4472C4");
  });

  it("六位 RGB（无 alpha）也接受", () => {
    expect(resolveColor({ argb: "4472C4" })).toBe("#4472C4");
  });

  it("主题索引落到 Office 默认色板", () => {
    expect(themeColor(4, 0)).toBe("#4472C4"); // accent1
    expect(themeColor(1, 0)).toBe("#000000"); // dk1
    expect(themeColor(0, 0)).toBe("#FFFFFF"); // lt1
  });

  it("文件自带色板时优先用文件的", () => {
    expect(themeColor(4, 0, ["FF0000", "00FF00", "00FF00", "00FF00", "123456"])).toBe("#123456");
  });

  it("越界的主题索引不猜", () => {
    expect(themeColor(99, 0)).toBeUndefined();
  });

  it("indexed 调色板按 OOXML 规范取值", () => {
    expect(resolveColor({ indexed: 2 })).toBe("#FF0000");
    expect(resolveColor({ indexed: 1 })).toBe("#FFFFFF");
  });

  it("tint 正数往白提、负数往黑压", () => {
    expect(applyTint("#000000", 0.5)).toBe("#808080");
    expect(applyTint("#FFFFFF", 0)).toBe("#FFFFFF");
    expect(applyTint("#FFFFFF", -0.5)).toBe("#808080");
  });

  it("空颜色返回 undefined 而不是黑", () => {
    expect(resolveColor(undefined)).toBeUndefined();
  });
});

describe("单位与地址换算", () => {
  it("列宽字符单位按 Excel 公式换像素", () => {
    // 18.83 字符 × 7px + 5px = 137px。少那 5px 列就会比 Excel 窄一截。
    expect(colWidthPx(18.83203125)).toBe(137);
  });

  it("没有列宽时不给宽度（交给默认列宽）", () => {
    expect(colWidthPx(undefined)).toBeNull();
    expect(colWidthPx(0)).toBeNull();
  });

  it("行高磅值按 96dpi 换像素", () => {
    expect(rowHeightPx(15)).toBe(20);
    expect(rowHeightPx(30)).toBe(40);
  });

  it("列号转 Excel 列标", () => {
    expect(colName(0)).toBe("A");
    expect(colName(25)).toBe("Z");
    expect(colName(26)).toBe("AA");
    expect(colName(51)).toBe("AZ");
    expect(colName(52)).toBe("BA");
  });

  it("A1 记法解析成 0 基区间", () => {
    expect(parseRange("A1")).toEqual({ row: 0, col: 0, rowSpan: 1, colSpan: 1 });
    expect(parseRange("B2:D5")).toEqual({ row: 1, col: 1, rowSpan: 4, colSpan: 3 });
  });

  it("不是区间的字符串返回 null", () => {
    expect(parseRange("nonsense")).toBeNull();
  });
});

describe("xlsx 格式还原（外来引擎写出的样本）", () => {
  // 只解析一次：引擎有近 1 MB，首次动态 import 在 jsdom 里要好几秒，
  // 而且每个用例都重新解析一遍没有意义。
  let sheet: SheetModel;
  let fidelity: FidelityLevel;

  beforeAll(async () => {
    const model = await parseXlsx(fixture());
    [sheet] = model.sheets;
    fidelity = model.fidelity;
  }, 60_000);

  it("带样式的来源被标记为 styled", () => {
    expect(fidelity).toBe("styled");
    expect(sheet.name).toBe("样式表");
  });

  it("第一行是数据不是表头（旧实现在这里吃掉一整行）", () => {
    expect(sheet.rows).toHaveLength(5);
    expect(sheet.rows[0]?.[0]?.text).toBe("标题行");
  });

  it("字体：粗体 / 字号 / 字体名 / 颜色", () => {
    const a1 = sheet.rows[0]?.[0];
    expect(a1?.style?.bold).toBe(true);
    expect(a1?.style?.size).toBe(14);
    expect(a1?.style?.font).toBe("Microsoft YaHei");
    expect(a1?.style?.color).toBe("#FFFFFF");
  });

  it("填充底色", () => {
    expect(sheet.rows[0]?.[0]?.style?.bg).toBe("#4472C4");
  });

  it("对齐：水平居中 + 垂直居中", () => {
    const a1 = sheet.rows[0]?.[0]?.style;
    expect(a1?.align).toBe("center");
    expect(a1?.valign).toBe("middle");
  });

  it("边框：上细线 + 下双线", () => {
    const style = sheet.rows[0]?.[0]?.style;
    expect(style?.border?.top?.css).toBe("1px solid");
    expect(style?.border?.top?.color).toBe("#000000");
    expect(style?.border?.bottom?.css).toBe("3px double");
  });

  it("斜体 + 红色文字", () => {
    const a3 = sheet.rows[2]?.[0]?.style;
    expect(a3?.italic).toBe(true);
    expect(a3?.color).toBe("#CC0000");
  });

  it("数字格式：千分位与百分比按 Excel 显示", () => {
    expect(sheet.rows[1]?.[2]?.style?.numFmt).toBe("#,##0.00");
    expect(sheet.rows[1]?.[2]?.text).toBe("1,234.50");
    expect(sheet.rows[2]?.[2]?.text).toBe("15.67%");
  });

  it("日期按日期格式显示，不是 Excel 序列号也不是 Date.toString()", () => {
    const text = sheet.rows[1]?.[3]?.text ?? "";
    expect(text).toMatch(/^\d{1,4}[-/]\d{1,2}[-/]\d{1,4}$/);
    // 旧实现把它渲染成 "Mon Jan 15 2024 08:00:43 GMT+0800 (…)" 这种
    expect(text).not.toContain("GMT");
    expect(text).not.toBe("45306");
  });

  it("列宽与行高换算成像素", () => {
    expect(sheet.colWidths[0]).toBe(137);
    expect(sheet.rowHeights[0]).toBe(40);
  });

  it("合并区还原成 rowSpan / colSpan", () => {
    expect(sheet.merges).toContainEqual({ row: 4, col: 0, rowSpan: 1, colSpan: 2 });
  });

  it("每行列数对齐到最宽行，被合并覆盖的格为 null", () => {
    expect(sheet.colCount).toBe(4);
    expect(sheet.rows[0]).toHaveLength(4);
    expect(sheet.rows[4]?.[1]).toBeNull();
  });
});
