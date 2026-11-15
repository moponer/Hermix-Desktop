import { describe, expect, it } from "vitest";

import type { VaultEntryMeta } from "../api/vault";
import {
  buildBlobMap,
  buildSearchBlob,
  filterEntries,
  sortEntries,
  type VaultSortKey,
} from "./vaultSearch";

function entry(over: Partial<VaultEntryMeta>): VaultEntryMeta {
  return {
    id: "id",
    name: "",
    url: "",
    username: "",
    notes: "",
    updated_at: "2026-01-01T00:00:00+00:00",
    ...over,
  };
}

const zhenjiang = entry({
  id: "a",
  name: "镇江危化品平台",
  url: "http://218.3.12.89/a.html",
  username: "shengty2021",
  notes: "",
});
const taobao = entry({
  id: "b",
  name: "淘宝",
  url: "https://taobao.com",
  username: "tb_user",
  notes: "购物",
});
const entries = [zhenjiang, taobao];

describe("buildSearchBlob", () => {
  it("包含名称全拼与首字母串（小写）", () => {
    const blob = buildSearchBlob(zhenjiang);
    expect(blob).toContain("zhenjiang");
    expect(blob).toContain("zjwhppt");
    expect(blob).toBe(blob.toLowerCase());
  });

  it("包含原文四字段", () => {
    const blob = buildSearchBlob(taobao);
    expect(blob).toContain("淘宝");
    expect(blob).toContain("taobao.com");
    expect(blob).toContain("tb_user");
    expect(blob).toContain("购物");
  });
});

describe("filterEntries", () => {
  const blobs = buildBlobMap(entries);

  it("中文子串命中", () => {
    expect(filterEntries(entries, "镇江", blobs).map((e) => e.id)).toEqual(["a"]);
  });

  it("全拼命中", () => {
    expect(filterEntries(entries, "zhenjiang", blobs).map((e) => e.id)).toEqual(["a"]);
  });

  it("首字母命中", () => {
    expect(filterEntries(entries, "zj", blobs).map((e) => e.id)).toEqual(["a"]);
  });

  it("网址 / 用户名 / 备注命中", () => {
    expect(filterEntries(entries, "taobao", blobs).map((e) => e.id)).toEqual(["b"]);
    expect(filterEntries(entries, "tb_user", blobs).map((e) => e.id)).toEqual(["b"]);
    expect(filterEntries(entries, "购物", blobs).map((e) => e.id)).toEqual(["b"]);
  });

  it("无匹配返回空；空查询返回全部", () => {
    expect(filterEntries(entries, "nomatch", blobs)).toEqual([]);
    expect(filterEntries(entries, "  ", blobs)).toEqual(entries);
  });
});

describe("sortEntries", () => {
  const older = entry({ id: "old", name: "淘宝", updated_at: "2026-01-01T00:00:00+00:00" });
  const newer = entry({ id: "new", name: "镇江", updated_at: "2026-06-01T00:00:00+00:00" });

  it("updated 按时间降序", () => {
    const out = sortEntries([older, newer], "updated");
    expect(out.map((e) => e.id)).toEqual(["new", "old"]);
  });

  it("nameAsc / nameDesc 中文拼音序", () => {
    const asc = sortEntries([newer, older], "nameAsc");
    expect(asc.map((e) => e.id)).toEqual(["old", "new"]);
    const desc = sortEntries([older, newer], "nameDesc");
    expect(desc.map((e) => e.id)).toEqual(["new", "old"]);
  });

  it("不改变入参数组", () => {
    const src = [newer, older];
    sortEntries(src, "updated");
    expect(src.map((e) => e.id)).toEqual(["new", "old"]);
  });
});

// 引用 VaultSortKey 避免未使用导出告警（类型导出即契约）。
const _k: VaultSortKey = "updated";
void _k;
