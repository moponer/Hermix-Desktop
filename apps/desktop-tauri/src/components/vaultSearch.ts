// Vault 搜索/排序纯函数 — 独立成模块以便单测（vaultSearch.test.ts）。
// 拼音支持：每条凭据预生成「原文字段 + 名称全拼(无声调) + 名称首字母串」的
// 搜索 blob，过滤用 includes 匹配——中文名 / zhenjiang / zj 三种输入都能命中。
import { pinyin } from "pinyin-pro";
import type { VaultEntryMeta } from "../api/vault";

export type VaultSortKey = "updated" | "nameAsc" | "nameDesc";

/** 名称的拼音形态：全拼（zhenjiang…）+ 首字母（zj…）。nonZh 连续保留原样。 */
function namePinyin(name: string): { full: string; initials: string } {
  const full = pinyin(name, { toneType: "none", type: "array", nonZh: "consecutive" }).join("");
  const initials = pinyin(name, { pattern: "first", toneType: "none", type: "array", nonZh: "consecutive" }).join("");
  return { full, initials };
}

/** 搜索 blob：名称/网址/用户名/备注 + 名称全拼 + 首字母，空格分隔，整体小写。 */
export function buildSearchBlob(e: VaultEntryMeta): string {
  const { full, initials } = namePinyin(e.name);
  return [e.name, e.url, e.username, e.notes, full, initials].join(" ").toLowerCase();
}

/** 列表加载后一次性建好 id → blob 映射，键击过滤零重算。 */
export function buildBlobMap(entries: VaultEntryMeta[]): Map<string, string> {
  return new Map(entries.map((e) => [e.id, buildSearchBlob(e)]));
}

export function filterEntries(
  entries: VaultEntryMeta[],
  query: string,
  blobs: Map<string, string>,
): VaultEntryMeta[] {
  const q = query.trim().toLowerCase();
  if (!q) return entries;
  return entries.filter((e) => blobs.get(e.id)?.includes(q));
}

export function sortEntries(entries: VaultEntryMeta[], key: VaultSortKey): VaultEntryMeta[] {
  const sorted = [...entries];
  if (key === "updated") {
    // updated_at 是 Utc RFC3339（hermix-vault lib.rs chrono::Utc::now().to_rfc3339()）；
    // Date.parse 对任意合法偏移量都给出同一时刻，比字符串比较鲁棒。
    sorted.sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  } else {
    const dir = key === "nameAsc" ? 1 : -1;
    sorted.sort((a, b) => dir * a.name.localeCompare(b.name, "zh"));
  }
  return sorted;
}
