// Vault 凭据保险库面板 — 列表 / 搜索 / 新增 / 编辑 / 删除 / 密码点击显示。
// 安全约定：列表渲染永不含密码；明文只在点击 👁 后现取（vaultReveal），
// 且 30s 后自动掩回，前端不缓存明文。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  vaultList,
  vaultCreate,
  vaultUpdate,
  vaultDelete,
  vaultReveal,
  type VaultEntryMeta,
} from "../api/vault";

interface FormState {
  id: string | null; // null = 新增
  name: string;
  url: string;
  username: string;
  secret: string;
  notes: string;
}

const EMPTY_FORM: FormState = {
  id: null,
  name: "",
  url: "",
  username: "",
  secret: "",
  notes: "",
};

export function VaultPanel() {
  const [entries, setEntries] = useState<VaultEntryMeta[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<FormState | null>(null); // null = 关闭表单
  const [saving, setSaving] = useState(false);
  const [revealed, setRevealed] = useState<Record<string, string>>({});
  const revealTimers = useRef<Record<string, number>>({});

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setEntries(await vaultList());
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    // 卸载时清掉所有掩回定时器。
    const timers = revealTimers.current;
    return () => Object.values(timers).forEach((t) => window.clearTimeout(t));
  }, [refresh]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter(
      (e) =>
        e.name.toLowerCase().includes(q) ||
        e.url.toLowerCase().includes(q) ||
        e.username.toLowerCase().includes(q),
    );
  }, [entries, query]);

  /** 点击显示密码：现取明文，30s 后自动掩回（前端不缓存）。 */
  const reveal = useCallback(async (id: string) => {
    try {
      const secret = await vaultReveal(id);
      setRevealed((r) => ({ ...r, [id]: secret }));
      window.clearTimeout(revealTimers.current[id]);
      revealTimers.current[id] = window.setTimeout(() => {
        setRevealed((r) => {
          const { [id]: _drop, ...rest } = r;
          return rest;
        });
      }, 30_000);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    }
  }, []);

  const maskReveal = useCallback((id: string) => {
    window.clearTimeout(revealTimers.current[id]);
    setRevealed((r) => {
      const { [id]: _drop, ...rest } = r;
      return rest;
    });
  }, []);

  const save = useCallback(async () => {
    if (!form) return;
    if (!form.name.trim()) {
      setError("名称必填");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      if (form.id) {
        // 编辑：secret 为空串 = 清空密码；未动过密码字段时后端靠
        // 「用户没点过密码框」判断——这里简化为：原值填回时传 undefined 不可行
        // （前端不缓存明文），约定：secret 留空且用户未点击「改密码」→ 传 undefined。
        await vaultUpdate(form.id, {
          name: form.name,
          url: form.url,
          username: form.username,
          secret: form.secret === "" ? undefined : form.secret,
          notes: form.notes,
        });
      } else {
        await vaultCreate({
          name: form.name,
          url: form.url,
          username: form.username,
          secret: form.secret,
          notes: form.notes,
        });
      }
      setForm(null);
      await refresh();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setSaving(false);
    }
  }, [form, refresh]);

  const remove = useCallback(
    async (id: string, name: string) => {
      if (!window.confirm(`删除「${name}」？此操作不可恢复。`)) return;
      try {
        await vaultDelete(id);
        maskReveal(id);
        await refresh();
      } catch (e) {
        setError(String(e instanceof Error ? e.message : e));
      }
    },
    [refresh, maskReveal],
  );

  /** 编辑回填：密码先 reveal（用户显式点了编辑 = 授权看到密码）；
   *  notes 直接来自列表元数据（不含密码，无需单独取）。 */
  const startEdit = useCallback((meta: VaultEntryMeta) => {
    vaultReveal(meta.id)
      .then((secret) => {
        setForm({
          id: meta.id,
          name: meta.name,
          url: meta.url,
          username: meta.username,
          secret,
          notes: meta.notes,
        });
      })
      .catch((e) => setError(String(e instanceof Error ? e.message : e)));
  }, []);

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-[var(--text-primary)]">凭据保险库</h2>
          <p className="text-xs text-[var(--text-secondary)]">
            AES-256-GCM 本地加密 · 主密钥在 Windows 凭据管理器 · 列表不含密码
          </p>
        </div>
        <button
          className="rounded-lg bg-[var(--accent)] px-3 py-1.5 text-sm text-white hover:opacity-90"
          onClick={() => setForm({ ...EMPTY_FORM })}
        >
          + 新增凭据
        </button>
      </div>

      <input
        className="rounded-lg border border-[var(--border)] bg-[var(--bg-secondary)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
        placeholder="搜索名称 / 网址 / 用户名…"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />

      {error && (
        <div className="rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950 dark:text-red-300">
          {error}
        </div>
      )}

      {loading ? (
        <div className="py-8 text-center text-sm text-[var(--text-secondary)]">加载中…</div>
      ) : filtered.length === 0 ? (
        <div className="py-8 text-center text-sm text-[var(--text-secondary)]">
          {entries.length === 0 ? "还没有凭据，点右上角「新增凭据」开始" : "没有匹配的条目"}
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {filtered.map((e) => (
            <div
              key={e.id}
              className="flex items-center gap-3 rounded-xl border border-[var(--border)] bg-[var(--bg-primary)] px-4 py-3"
            >
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium text-[var(--text-primary)]">
                  {e.name}
                </div>
                <div className="truncate text-xs text-[var(--text-secondary)]">
                  {[e.url, e.username].filter(Boolean).join(" · ") || "—"}
                </div>
              </div>
              <button
                className="rounded-lg px-2 py-1 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-secondary)]"
                title="显示 / 掩回密码"
                onClick={() => (revealed[e.id] !== undefined ? maskReveal(e.id) : void reveal(e.id))}
              >
                {revealed[e.id] !== undefined ? "🙈 掩回" : "👁 显示"}
              </button>
              <button
                className="rounded-lg px-2 py-1 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-secondary)]"
                onClick={() => startEdit(e)}
              >
                编辑
              </button>
              <button
                className="rounded-lg px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:hover:bg-red-950"
                onClick={() => void remove(e.id, e.name)}
              >
                删除
              </button>
            </div>
          ))}
        </div>
      )}

      {form && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40" onClick={() => setForm(null)}>
          <div
            className="w-full max-w-md rounded-2xl border border-[var(--border)] bg-[var(--bg-primary)] p-5 shadow-xl"
            onClick={(ev) => ev.stopPropagation()}
          >
            <h3 className="mb-4 text-base font-semibold text-[var(--text-primary)]">
              {form.id ? "编辑凭据" : "新增凭据"}
            </h3>
            <div className="flex flex-col gap-3">
              {(
                [
                  ["name", "名称 *", "text"],
                  ["url", "网址", "text"],
                  ["username", "用户名", "text"],
                  ["secret", form.id ? "密码（留空 = 不修改）" : "密码", "password"],
                ] as const
              ).map(([key, label, type]) => (
                <label key={key} className="flex flex-col gap-1">
                  <span className="text-xs text-[var(--text-secondary)]">{label}</span>
                  <input
                    type={type}
                    className="rounded-lg border border-[var(--border)] bg-[var(--bg-secondary)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
                    value={form[key]}
                    onChange={(ev) => setForm((f) => (f ? { ...f, [key]: ev.target.value } : f))}
                    autoComplete="off"
                  />
                </label>
              ))}
              <label className="flex flex-col gap-1">
                <span className="text-xs text-[var(--text-secondary)]">备注</span>
                <textarea
                  rows={2}
                  className="resize-none rounded-lg border border-[var(--border)] bg-[var(--bg-secondary)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
                  value={form.notes}
                  onChange={(ev) => setForm((f) => (f ? { ...f, notes: ev.target.value } : f))}
                />
              </label>
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button
                className="rounded-lg px-3 py-1.5 text-sm text-[var(--text-secondary)] hover:bg-[var(--bg-secondary)]"
                onClick={() => setForm(null)}
              >
                取消
              </button>
              <button
                className="rounded-lg bg-[var(--accent)] px-3 py-1.5 text-sm text-white hover:opacity-90 disabled:opacity-50"
                disabled={saving}
                onClick={() => void save()}
              >
                {saving ? "保存中…" : "保存"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
