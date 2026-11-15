// Vault（凭据保险库）API 封装 — 走 stdio 桥（rest.ts::fetchJSON）。
// 服务端：crates/hermix-server/src/dashboard/vault_api.rs
// 安全约定：列表永不含密码；明文只在 reveal（点击显示）与编辑回填出现。

import { fetchJSON } from "./rest";

export interface VaultEntryMeta {
  id: string;
  name: string;
  url: string;
  username: string;
  notes: string;
  updated_at: string;
}

export interface VaultEntryFull extends VaultEntryMeta {
  notes: string;
  /** 仅 reveal / 编辑回填响应里有。 */
  secret?: string;
}

export interface VaultUpsertBody {
  name: string;
  url?: string;
  username?: string;
  secret?: string;
  notes?: string;
}

export async function vaultList(): Promise<VaultEntryMeta[]> {
  const r = await fetchJSON<{ entries: VaultEntryMeta[] }>("/api/vault/entries");
  return r.entries;
}

export async function vaultCreate(body: VaultUpsertBody): Promise<VaultEntryFull> {
  const r = await fetchJSON<{ entry: VaultEntryFull }>("/api/vault/entries", {
    method: "POST",
    body: JSON.stringify(body),
  });
  return r.entry;
}

export async function vaultUpdate(
  id: string,
  body: VaultUpsertBody,
): Promise<VaultEntryFull> {
  const r = await fetchJSON<{ entry: VaultEntryFull }>(
    `/api/vault/entries/${encodeURIComponent(id)}`,
    { method: "PUT", body: JSON.stringify(body) },
  );
  return r.entry;
}

export async function vaultDelete(id: string): Promise<void> {
  await fetchJSON<{ ok: boolean }>(`/api/vault/entries/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

/** UI 点击显示密码（每次点击现取，前端不缓存明文）。 */
export async function vaultReveal(id: string): Promise<string> {
  const r = await fetchJSON<{ secret: string | null }>(
    `/api/vault/entries/${encodeURIComponent(id)}/reveal`,
    { method: "POST" },
  );
  return r.secret ?? "";
}
