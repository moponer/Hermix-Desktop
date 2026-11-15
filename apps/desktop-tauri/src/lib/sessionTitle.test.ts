import { describe, expect, it } from "vitest";
import { cleanSessionTitle } from "./sessionTitle";

describe("cleanSessionTitle", () => {
  it("剥掉 JSON 数组外壳里的 think 推理原文（2026-10-28 删除确认卡实报形态）", () => {
    const raw = '["<think>已识别为超星课程自动学习任务；按登录、选课、逐节学习、测验提交、状…"]';
    // 首选路径（删未闭合 think 连同其后内容）会剥成 `["` 空壳，
    // 兜底路径只剥标签、保留推理文字 —— 它是标题里唯一可读的内容。
    expect(cleanSessionTitle(raw)).toBe(
      "已识别为超星课程自动学习任务；按登录、选课、逐节学习、测验提交、状…",
    );
  });

  it("删除完整的 think 块，保留块外的真标题", () => {
    expect(cleanSessionTitle("<think>推理过程</think>整理照片")).toBe("整理照片");
  });

  it("未闭合的 think：兜底路径保留推理文字并剥掉数组外壳", () => {
    expect(cleanSessionTitle('["<think>只有推理没有结尾')).toBe("只有推理没有结尾");
  });

  it("合法 JSON 数组取第一个非空字符串成员", () => {
    expect(cleanSessionTitle('["周报转 PDF", "备用标题"]')).toBe("周报转 PDF");
  });

  it("截断的数组外壳（尾部挂着 ...\" 残迹）剥干净", () => {
    expect(cleanSessionTitle('["把昨天那份周报转成 PDF 发我"...')).toBe(
      "把昨天那份周报转成 PDF 发我",
    );
  });

  it("剥掉成对包裹引号", () => {
    expect(cleanSessionTitle('"关于登录逻辑的讨论"')).toBe("关于登录逻辑的讨论");
  });

  it("以方括号开头的正常标题不动外壳", () => {
    expect(cleanSessionTitle("[重要] 会议纪要")).toBe("[重要] 会议纪要");
  });

  it("普通标题原样保留（压空白）", () => {
    expect(cleanSessionTitle("  关于  登录逻辑\n的讨论  ")).toBe("关于 登录逻辑 的讨论");
  });

  it("剥完只剩符号/空白时返回空串，兜底交给调用方", () => {
    expect(cleanSessionTitle('["<think>')).toBe("");
    expect(cleanSessionTitle("   ")).toBe("");
    expect(cleanSessionTitle(null)).toBe("");
    expect(cleanSessionTitle(undefined)).toBe("");
  });
});
