import { describe, expect, it } from "vitest";

import { initialOf, toneOf, TONES } from "./initialIcon";

describe("initialOf", () => {
  it("汉字取第一个汉字", () => {
    expect(initialOf("浏览器自动化")).toBe("浏");
    expect(initialOf("数据")).toBe("数");
  });

  it("拉丁取首字母并大写", () => {
    expect(initialOf("GitHub 操作流程")).toBe("G");
    expect(initialOf("api 设计模式")).toBe("A");
    expect(initialOf("DevOps 实践")).toBe("D");
  });

  it("跳过前导空白、标点与 emoji（括号里的字也算首字）", () => {
    expect(initialOf("  PDF 文件处理")).toBe("P");
    expect(initialOf("【新】前端架构")).toBe("新");
    expect(initialOf("🔥 热门技能")).toBe("热");
    expect(initialOf("— 破折号开头")).toBe("破");
  });

  it("数字开头保留数字（3D 打印 / 7 天回顾）", () => {
    expect(initialOf("3D 打印")).toBe("3");
    expect(initialOf("7 天回顾")).toBe("7");
  });

  it("空名退化成一个中点，不会渲染空白方块", () => {
    expect(initialOf("")).toBe("·");
    expect(initialOf(null)).toBe("·");
    expect(initialOf(undefined)).toBe("·");
    expect(initialOf("   ")).toBe("·");
  });
});

describe("toneOf", () => {
  it("同名必然同色（与调用次数无关）", () => {
    const a = toneOf("浏览器自动化");
    expect(toneOf("浏览器自动化")).toBe(a);
    expect(TONES).toContain(a);
  });

  it("不同名字会分到不同色相（抽样不撞成一片）", () => {
    const names = ["API 设计模式", "浏览器自动化", "数据库设计", "前端架构", "测试驱动开发", "代码审查"];
    const used = new Set(names.map(toneOf));
    expect(used.size).toBeGreaterThan(1);
  });

  it("空名也有确定的色相，不会返回 undefined", () => {
    expect(TONES).toContain(toneOf(""));
    expect(TONES).toContain(toneOf(null));
  });
});
