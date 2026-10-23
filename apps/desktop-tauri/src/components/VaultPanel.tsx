// Vault 凭据保险库面板 — 列表 / 搜索 / 新增 / 编辑 / 删除 / 密码点击显示。
// 安全约定：列表渲染永不含密码；明文只在点击显示图标后现取（vaultReveal），
// 且 30s 后自动掩回，前端不缓存明文。
//
// 配色一律走 tokens.css 的工具类（bg-accent / text-on-accent / text-text-primary …）。
// 禁写 `bg-[var(--accent)]` 这类裸 var() 任意值：本项目颜色令牌全部带 `--color-`
// 前缀，写错前缀不会报错，Tailwind 只会发出一条 `background-color: var(--accent)`，
// 该 var 未定义 → 整条声明在计算值阶段失效 → 底色回落 transparent，
// 而 `text-white` 照常生效 → 实色按钮变成白底白字，肉眼完全看不见。
// 回归守卫：apps/desktop-tauri/scripts/check-css-var-refs.cjs

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ShieldCheck, Eye, EyeOff, User, KeyRound, Check, Globe, X } from "lucide-react";
import { COPY_FAILED, SAVE, SAVING, failure } from "./strings";
import {
  vaultList,
  vaultCreate,
  vaultUpdate,
  vaultDelete,
  vaultReveal,
  type VaultEntryMeta,
} from "../api/vault";
import { openExternalUrl } from "../desktopBridge";
import { ConfirmDialog, Modal, PageHeader, SkeletonCard } from "./ui";
import {
  buildBlobMap,
  filterEntries,
  sortEntries,
  type VaultSortKey,
} from "./vaultSearch";

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

/** 图标按钮的 hover 说明浮层。
 *  不用原生 title：WebView2 里约 1s 才弹出且样式不可控。
 *  浮层向左上溢出，但列表容器是 `overflow-y-auto p-6`，上溢只落在 24px 内边距里
 *  （overflow 裁切边界是 padding box），因此不会被裁断。 */
function Tip({ label }: { label: string }) {
  return (
    <span
      className="pointer-events-none absolute bottom-full left-1/2 z-20 mb-1 -translate-x-1/2 whitespace-nowrap rounded-md bg-bg-elevated px-2 py-1 text-xs text-text-primary opacity-0 shadow-pop transition-opacity duration-150 group-hover:opacity-100 group-focus-visible:opacity-100"
      role="tooltip"
    >
      {label}
    </span>
  );
}

/** 网站图标：按域名从 favicon.im 现取（https://favicon.im/<domain>）。
 *  URL 非法或无域名时回落地球图标；加载失败（404 / 网络）同样回落。
 *  地球兜底走图标档位（14 → icon-md，16 → icon-lg），不再用裸 size={N}。 */
function Favicon({ url, size = 16 }: { url: string; size?: number }) {
  const [failed, setFailed] = useState(false);
  const domain = useMemo(() => {
    try {
      return new URL(url).hostname;
    } catch {
      return "";
    }
  }, [url]);
  if (!domain || failed) {
    return (
      <span className="flex shrink-0 text-text-faint" aria-hidden="true">
        <Globe className={size <= 14 ? "icon-md" : "icon-lg"} />
      </span>
    );
  }
  return (
    <img
      src={`https://favicon.im/${domain}`}
      width={size}
      height={size}
      alt=""
      loading="lazy"
      className="shrink-0 rounded-[3px]"
      onError={() => setFailed(true)}
    />
  );
}

/** RFC3339 → 本地时间显示；解析失败原样返回。 */
function formatTime(iso: string): string {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? iso : new Date(t).toLocaleString();
}

export function VaultPanel() {
  const [entries, setEntries] = useState<VaultEntryMeta[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<FormState | null>(null); // null = 关闭表单
  const [saving, setSaving] = useState(false);
  const [revealed, setRevealed] = useState<Record<string, string>>({});
  const revealTimers = useRef<Record<string, number>>({});

  // 打磨：删除确认走 ui/ConfirmDialog（R3-6）/ 复制瞬时反馈。
  const [confirmTarget, setConfirmTarget] = useState<{ id: string; name: string } | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [showSecret, setShowSecret] = useState(false);
  const copyTimer = useRef<number | null>(null);
  // 点击行选中的凭据；右侧详情面板据此渲染（从 entries 按 id 查，筛选不丢选中）。
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [sortKey, setSortKey] = useState<VaultSortKey>("updated");

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

  const blobMap = useMemo(() => buildBlobMap(entries), [entries]);
  const visible = useMemo(
    () => sortEntries(filterEntries(entries, query, blobMap), sortKey),
    [entries, query, blobMap, sortKey],
  );
  const selected = useMemo(
    () => entries.find((e) => e.id === selectedId) ?? null,
    [entries, selectedId],
  );

  /** 重置列表：清搜索条件 + 恢复默认排序。 */
  const resetFilters = useCallback(() => {
    setQuery("");
    setSortKey("updated");
  }, []);

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

  /** 复制文本到剪贴板，1.5s 后撤掉「已复制」反馈。 */
  const copy = useCallback((text: string, key: string) => {
    if (!text) return;
    navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setCopiedKey(key);
        if (copyTimer.current) window.clearTimeout(copyTimer.current);
        copyTimer.current = window.setTimeout(
          () => setCopiedKey((c) => (c === key ? null : c)),
          1500,
        );
      })
      .catch(() => setError(failure(COPY_FAILED, "请手动选中内容后复制。")));
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
        // 编辑：secret 为空串 = 不改密码（前端不缓存明文，留空即未编辑）。
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
      setShowSecret(false);
      await refresh();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setSaving(false);
    }
  }, [form, refresh]);

  const remove = useCallback(async (id: string) => {
      try {
        await vaultDelete(id);
        maskReveal(id);
        setSelectedId((cur) => (cur === id ? null : cur));
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
        setShowSecret(false);
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

  const openAdd = useCallback(() => {
    setShowSecret(false);
    setForm({ ...EMPTY_FORM });
  }, []);

  // 离开面板时清掉复制反馈定时器。
  useEffect(() => {
    return () => {
      if (copyTimer.current) window.clearTimeout(copyTimer.current);
    };
  }, []);

  return (
    <div className="flex h-full flex-col gap-4 p-6">
      <PageHeader
        icon={<ShieldCheck className="icon-lg" />}
        title="凭据保险库"
        as="h1"
        actions={
          <button
            className="active:scale-[0.98] rounded-lg bg-accent px-3 py-1.5 text-sm text-on-accent transition-transform hover:bg-accent-hover"
            onClick={openAdd}
          >
            + 新增凭据
          </button>
        }
      >
        <span className="text-[length:var(--text-meta)] text-text-secondary">
          AES-256-GCM 本地加密 · 主密钥在 Windows 凭据管理器 · 列表不含密码
        </span>
      </PageHeader>

      <div className="flex min-h-0 flex-1 gap-4">
        <section className="flex min-w-0 flex-1 flex-col gap-3" aria-label="凭据列表">
          <div className="relative">
            <input
              className="w-full rounded-lg bg-bg-secondary py-2 pl-3 pr-9 text-sm text-text-primary outline-none focus:border-accent"
              placeholder="搜索名称 / 拼音 / 网址 / 用户名…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(ev) => {
                if (ev.key === "Escape") {
                  ev.stopPropagation();
                  setQuery("");
                }
              }}
            />
            {query !== "" && (
              <button
                className="group absolute right-2 top-1/2 flex -translate-y-1/2 items-center rounded-md p-1 text-text-secondary transition-colors hover:bg-bg-card-hover"
                aria-label="清除搜索"
                onClick={() => setQuery("")}
              >
                <X className="icon-md" />
                <Tip label="清除搜索" />
              </button>
            )}
          </div>

          {error && (
            <div className="rounded-lg bg-error-soft px-3 py-2 text-sm text-text-error">{error}</div>
          )}

          {!loading && entries.length > 0 && (
            <div className="flex items-center justify-between">
              <p className="text-[length:var(--text-meta)] text-text-faint">
                共 {entries.length} 条
                {query.trim() !== "" && (
                  <>
                    {" · 匹配 "}
                    {visible.length}
                    {" 条 · "}
                    <button
                      className="text-text-secondary underline-offset-2 transition-colors hover:text-text-primary hover:underline"
                      onClick={resetFilters}
                    >
                      重置
                    </button>
                  </>
                )}
              </p>
              <select
                className="rounded-lg bg-bg-secondary px-2 py-1 text-[length:var(--text-meta)] text-text-primary outline-none focus:border-accent"
                value={sortKey}
                onChange={(e) => setSortKey(e.target.value as VaultSortKey)}
                aria-label="排序方式"
              >
                <option value="updated">最近更新</option>
                <option value="nameAsc">名称 A→Z</option>
                <option value="nameDesc">名称 Z→A</option>
              </select>
            </div>
          )}

          {loading ? (
            /* 骨架屏：凭据行形状预告（图 + 名 + 用户名行） */
            <div className="flex flex-col gap-2 py-1">
              {Array.from({ length: 5 }).map((_, i) => (
                <SkeletonCard key={i} lines={1} />
              ))}
            </div>
          ) : visible.length === 0 ? (
            <div className="py-8 text-center text-sm text-text-secondary">
              {entries.length === 0 ? "还没有凭据，点右上角「新增凭据」开始" : "没有匹配的条目"}
            </div>
          ) : (
            <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto pb-1">
              {visible.map((e) => {
                const shown = revealed[e.id] !== undefined;
                const isSelected = e.id === selectedId;
                return (
                  <div
                    key={e.id}
                    onClick={() => setSelectedId(e.id)}
                    className={`flex cursor-pointer items-center gap-2 rounded-xl px-4 py-3 transition-colors hover:bg-bg-card-hover ${ isSelected ? "border-accent bg-bg-card-hover" : "border-border-primary bg-bg-card" }`}
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <Favicon url={e.url} size={16} />
                        <div className="truncate text-sm font-medium text-text-primary">{e.name}</div>
                      </div>
                      <div className="truncate text-xs text-text-secondary">
                        {[e.url, e.username].filter(Boolean).join(" · ") || "—"}
                      </div>
                      {shown && revealed[e.id] && (
                        <div className="mt-1 break-all font-mono text-xs text-text-secondary">
                          {revealed[e.id]}
                        </div>
                      )}
                      {e.notes.trim() !== "" && (
                        <div className="mt-1 truncate text-xs text-text-faint">{e.notes}</div>
                      )}
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      <button
                        className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs text-text-secondary transition-colors hover:bg-bg-secondary"
                        aria-label={shown ? "掩回密码" : "显示密码"}
                        aria-pressed={shown}
                        onClick={(ev) => {
                          ev.stopPropagation();
                          if (shown) maskReveal(e.id);
                          else void reveal(e.id);
                        }}
                      >
                        {shown ? <EyeOff className="icon-md" /> : <Eye className="icon-md" />}
                        {shown ? "掩回" : "显示"}
                      </button>
                      <button
                        className="group relative flex items-center rounded-lg p-1.5 text-text-secondary transition-colors hover:bg-bg-secondary"
                        aria-label="复制用户名"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          copy(e.username, `${e.id}:user`);
                        }}
                      >
                        {copiedKey === `${e.id}:user` ? <Check className="icon-md" /> : <User className="icon-md" />}
                        <Tip label="复制用户名" />
                      </button>
                      {shown && (
                        <button
                          className="group relative flex items-center rounded-lg p-1.5 text-text-secondary transition-colors hover:bg-bg-secondary"
                          aria-label="复制密码"
                          onClick={(ev) => {
                            ev.stopPropagation();
                            copy(revealed[e.id], `${e.id}:secret`);
                          }}
                        >
                          {copiedKey === `${e.id}:secret` ? <Check className="icon-md" /> : <KeyRound className="icon-md" />}
                          <Tip label="复制密码" />
                        </button>
                      )}
                      <button
                        className="rounded-lg px-2 py-1 text-xs text-text-secondary transition-colors hover:bg-bg-secondary"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          startEdit(e);
                        }}
                      >
                        编辑
                      </button>
                      {/* R3-6：删除确认统一走 ui/ConfirmDialog，行内两步按钮废止 */}
                      <button
                        className="rounded-lg px-2 py-1 text-xs text-text-error transition-colors hover:bg-error-soft"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          setConfirmTarget({ id: e.id, name: e.name || e.username || e.id });
                        }}
                      >
                        删除
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </section>

        {selected && (
          <aside
            className="flex w-80 shrink-0 flex-col overflow-y-auto rounded-xl bg-bg-card"
            aria-label="凭据详情"
          >
            <div className="flex items-start gap-2 border-b border-border-primary p-4">
              <Favicon url={selected.url} size={16} />
              <div className="min-w-0 flex-1">
                <h3 className="truncate text-[length:var(--text-ui)] font-semibold text-text-primary">
                  {selected.name}
                </h3>
                <p className="text-[length:var(--text-meta)] text-text-faint">
                  更新于 {formatTime(selected.updated_at)}
                </p>
              </div>
              <button
                className="group relative flex items-center rounded-md p-1 text-text-secondary transition-colors hover:bg-bg-secondary"
                aria-label="关闭详情"
                onClick={() => setSelectedId(null)}
              >
                <X className="icon-md" />
                <Tip label="关闭详情" />
              </button>
            </div>
            <div className="flex flex-col gap-4 p-4">
              {selected.url !== "" && (
                <div>
                  <p className="text-[length:var(--text-meta)] text-text-faint">网址</p>
                  <button
                    className="break-all text-left text-[length:var(--text-ui)] text-accent hover:underline"
                    onClick={() => void openExternalUrl(selected.url)}
                  >
                    {selected.url}
                  </button>
                </div>
              )}
              <div>
                <p className="text-[length:var(--text-meta)] text-text-faint">用户名</p>
                <div className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-[length:var(--text-ui)] text-text-primary">
                    {selected.username || "—"}
                  </span>
                  {selected.username !== "" && (
                    <button
                      className="group relative flex items-center rounded-md p-1 text-text-secondary transition-colors hover:bg-bg-secondary"
                      aria-label="复制用户名"
                      onClick={() => copy(selected.username, `${selected.id}:user`)}
                    >
                      {copiedKey === `${selected.id}:user` ? <Check className="icon-md" /> : <User className="icon-md" />}
                      <Tip label="复制用户名" />
                    </button>
                  )}
                </div>
              </div>
              <div>
                <p className="text-[length:var(--text-meta)] text-text-faint">密码</p>
                {revealed[selected.id] !== undefined ? (
                  <div className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 break-all font-mono text-[length:var(--text-meta)] text-text-primary">
                      {revealed[selected.id]}
                    </span>
                    <button
                      className="group relative flex items-center rounded-md p-1 text-text-secondary transition-colors hover:bg-bg-secondary"
                      aria-label="掩回密码"
                      onClick={() => maskReveal(selected.id)}
                    >
                      <EyeOff className="icon-md" />
                      <Tip label="掩回密码" />
                    </button>
                    <button
                      className="group relative flex items-center rounded-md p-1 text-text-secondary transition-colors hover:bg-bg-secondary"
                      aria-label="复制密码"
                      onClick={() => copy(revealed[selected.id], `${selected.id}:secret`)}
                    >
                      {copiedKey === `${selected.id}:secret` ? <Check className="icon-md" /> : <KeyRound className="icon-md" />}
                      <Tip label="复制密码" />
                    </button>
                  </div>
                ) : (
                  <div className="flex items-center gap-2">
                    {/* 固定位数圆点：不跟随真实密码长度，避免泄露长度信息。 */}
                    <span className="flex-1 font-mono text-[length:var(--text-meta)] text-text-secondary">
                      ••••••••
                    </span>
                    <button
                      className="group relative flex items-center gap-1 rounded-lg px-2 py-1 text-xs text-text-secondary transition-colors hover:bg-bg-secondary"
                      aria-label="显示密码"
                      onClick={() => void reveal(selected.id)}
                    >
                      <Eye className="icon-md" />
                      显示
                    </button>
                  </div>
                )}
              </div>
              {selected.notes.trim() !== "" && (
                <div>
                  <p className="text-[length:var(--text-meta)] text-text-faint">备注</p>
                  <p className="whitespace-pre-wrap break-words text-[length:var(--text-ui)] text-text-secondary">
                    {selected.notes}
                  </p>
                </div>
              )}
              <div className="flex gap-2 border-t border-border-primary pt-4">
                <button
                  className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[length:var(--text-meta)] text-text-primary transition-colors hover:bg-bg-hover"
                  onClick={() => startEdit(selected)}
                >
                  编辑
                </button>
                <button
                  className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[length:var(--text-meta)] text-text-error transition-colors hover:bg-error-soft"
                  onClick={() => setConfirmTarget({ id: selected.id, name: selected.name || selected.username || selected.id })}
                >
                  删除
                </button>
              </div>
            </div>
          </aside>
        )}
      </div>

      {form && (
        <Modal
          open
          onClose={() => setForm(null)}
          title={form.id ? "编辑凭据" : "新增凭据"}
          width="base"
          footer={
            <>
              <button
                className="rounded-lg px-3 py-1.5 text-sm text-text-secondary transition-colors hover:bg-bg-secondary"
                onClick={() => setForm(null)}
              >
                取消
              </button>
              <button
                className="rounded-lg bg-accent px-3 py-1.5 text-sm text-on-accent transition-colors hover:bg-accent-hover disabled:opacity-50"
                disabled={saving}
                onClick={() => void save()}
              >
                {saving ? SAVING : SAVE}
              </button>
            </>
          }
        >
          <div className="flex flex-col gap-3">
              {(
                [
                  ["name", "名称 *", "text"],
                  ["url", "网址", "text"],
                  ["username", "用户名", "text"],
                ] as const
              ).map(([key, label, type]) => (
                <label key={key} className="flex flex-col gap-1">
                  <span className="text-xs text-text-secondary">{label}</span>
                  {key === "url" ? (
                    <div className="relative">
                      <span className="pointer-events-none absolute left-2.5 top-1/2 flex -translate-y-1/2 text-text-faint">
                        <Favicon url={form.url} size={14} />
                      </span>
                      <input
                        type={type}
                        className="w-full rounded-lg bg-bg-secondary py-2 pl-8 pr-3 text-sm text-text-primary outline-none focus:border-accent"
                        value={form[key]}
                        onChange={(ev) => setForm((f) => (f ? { ...f, [key]: ev.target.value } : f))}
                        autoComplete="off"
                      />
                    </div>
                  ) : (
                    <input
                      type={type}
                      className="rounded-lg bg-bg-secondary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
                      value={form[key]}
                      onChange={(ev) => setForm((f) => (f ? { ...f, [key]: ev.target.value } : f))}
                      autoComplete="off"
                    />
                  )}
                </label>
              ))}
              <label className="flex flex-col gap-1">
                <span className="text-xs text-text-secondary">
                  {form.id ? "密码（留空 = 不修改）" : "密码"}
                </span>
                <div className="relative">
                  <input
                    type={showSecret ? "text" : "password"}
                    className="w-full rounded-lg bg-bg-secondary px-3 py-2 pr-10 text-sm text-text-primary outline-none focus:border-accent"
                    value={form.secret}
                    onChange={(ev) =>
                      setForm((f) => (f ? { ...f, secret: ev.target.value } : f))
                    }
                    autoComplete="off"
                  />
                  <button
                    type="button"
                    className="group absolute right-2 top-1/2 flex -translate-y-1/2 items-center rounded-md p-1 text-text-secondary transition-colors hover:bg-bg-card-hover"
                    aria-label={showSecret ? "隐藏密码" : "显示密码"}
                    aria-pressed={showSecret}
                    onClick={() => setShowSecret((s) => !s)}
                  >
                    {showSecret ? <EyeOff className="icon-md" /> : <Eye className="icon-md" />}
                    <Tip label={showSecret ? "隐藏密码" : "显示密码"} />
                  </button>
                </div>
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-xs text-text-secondary">备注</span>
                <textarea
                  rows={2}
                  className="resize-none rounded-lg bg-bg-secondary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
                  value={form.notes}
                  onChange={(ev) => setForm((f) => (f ? { ...f, notes: ev.target.value } : f))}
                />
              </label>
          </div>
        </Modal>
      )}

      {/* R3-6：删除凭据统一确认弹窗（原行内两步按钮已废止） */}
      <ConfirmDialog
        open={confirmTarget !== null}
        title="删除凭据"
        body={`确定删除「${confirmTarget?.name ?? ""}」？删除后无法恢复。`}
        confirmLabel="删除"
        onConfirm={() => {
          if (confirmTarget) void remove(confirmTarget.id);
          setConfirmTarget(null);
        }}
        onCancel={() => setConfirmTarget(null)}
      />
    </div>
  );
}
