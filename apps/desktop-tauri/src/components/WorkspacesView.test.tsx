/**
 * parseCountStatQuery 单测 — stat 卡受控查询的最小解析契约。
 *
 * 只支持 count 家族 DSL；未识别格式必须返回 null（组件留空态），
 * 绝不猜语义执行。
 */

import { describe, it, expect } from "vitest";
import { parseCountStatQuery } from "./WorkspacesView";

describe("parseCountStatQuery", () => {
  it("纯 count", () => {
    expect(parseCountStatQuery("count")).toEqual({});
    expect(parseCountStatQuery("  COUNT ")).toEqual({});
  });

  it("count where 字段=值", () => {
    expect(parseCountStatQuery("count where status=在用")).toEqual({
      field: "status",
      value: "在用",
    });
  });

  it("count from 表", () => {
    expect(parseCountStatQuery("count from vehicles")).toEqual({ table: "vehicles" });
  });

  it("count from 表 where 字段=值（值可含空格与引号）", () => {
    expect(parseCountStatQuery("count from drivers where name='张 三'")).toEqual({
      table: "drivers",
      field: "name",
      value: "张 三",
    });
    expect(parseCountStatQuery("count where title=\"年检\"")).toEqual({
      field: "title",
      value: "年检",
    });
  });

  it("非 count 家族 DSL 返回 null", () => {
    for (const q of [
      "",
      "sum total",
      "count or something else entirely",
      "list rows",
      "select * from t",
    ]) {
      expect(parseCountStatQuery(q), q).toBeNull();
    }
  });
});
