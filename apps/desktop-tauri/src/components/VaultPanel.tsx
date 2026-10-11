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

/** lucide 风格内联图标（界面禁用 emoji，见 P-40）。 */
function IconEye() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7-10-7-10-7Z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

function IconEyeOff() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M10.7 5.1A11 11 0 0 1 12 5c6.4 0 10 7 10 7a18 18 0 0 1-3 4.1" />
      <path d="M6.6 6.6A18 18 0 0 0 2 12s3.6 7 10 7a10.7 10.7 0 0 0 5.3-1.4" />
      <path d="M9.9 9.9a3 3 0 0 0 4.2 4.2" />
      <path d="M2 2l20 20" />
    </svg>
  );
}

/** 复制用户名：人像图标（两个按钮用同一复制图标无法区分用途，故按语义取形）。 */
function IconUser() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2" />
      <circle cx="12" cy="7" r="4" />
    </svg>
  );
}

/** 复制密码：钥匙图标。 */
function IconKey() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <circle cx="7.5" cy="15.5" r="5.5" />
      <path d="m21 2-9.6 9.6" />
      <path d="m15.5 7.5 3 3L22 7l-3-3" />
    </svg>
  );
}

/** 复制成功对勾。 */
function IconCheck() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}

/** 中性地球图标：无网址 / 域名非法 / favicon 加载失败时的兜底。 */
function IconGlobe({ size = 16 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18" />
      <path d="M12 3a15 15 0 0 1 0 18 15 15 0 0 1 0-18Z" />
    </svg>
  );
}

/** 图标按钮的 hover 说明浮层。
 *  不用原生 title：WebView2 里约 1s 才弹出且样式不可控。
 *  浮层向左上溢出，但列表容器是 `overflow-y-auto p-6`，上溢只落在 24px 内边距里
 *  （overflow 裁切边界是 padding box），因此不会被裁断。 */
function Tip({ label }: { label: string }) {
  return (
    <span
      className="pointer-events-none absolute bottom-full left-1/2 z-20 mb-1 -translate-x-1/2 whitespace-nowrap rounded-md border border-border-primary bg-bg-elevated px-2 py-1 text-xs text-text-primary opacity-0 shadow-pop transition-opacity duration-150 group-hover:opacity-100 group-focus-visible:opacity-100"
      role="tooltip"
    >
      {label}
    </span>
  );
}

/** 网站图标：按域名从 favicon.im 现取（https://favicon.im/<domain>）。
 *  URL 非法或无域名时回落地球图标；加载失败（404 / 网络）同样回落。 */
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
        <IconGlobe size={size} />
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
      className="shrink-0 rounded-[3px] ring-1 ring-border-primary"
      onError={() => setFailed(true)}
    />
  );
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

  // 打磨：删除行内两步确认 / 复制瞬时反馈 / 弹窗入场动画状态。
  const [confirmingId, setConfirmingId] = useState<string | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [showSecret, setShowSecret] = useState(false);
  const [modalShown, setModalShown] = useState(false);
  const copyTimer = useRef<number | null>(null);

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

  // 弹窗挂载后下一帧翻转显隐状态，触发入场过渡（reduced-motion 由类名兜底）。
  useEffect(() => {
    if (form) {
      const t = window.setTimeout(() => setModalShown(true), 10);
      return () => window.clearTimeout(t);
    }
    setModalShown(false);
    return undefined;
  }, [form]);

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
      .catch(() => setError("复制失败（剪贴板不可用）"));
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
        setConfirmingId(null);
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
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibibold text-text-primary">凭据保险库</h2>
          <p className="text-xs text-text-secondary">
            AES-256-GCM 本地加密 · 主密钥在 Windows 凭据管理器 · 列表不含密码
          </p>
        </div>
        <button
          className="active:scale-[0.98] rounded-lg bg-accent px-3 py-1.5 text-sm text-on-accent transition-transform hover:bg-accent-hover"
          onClick={openAdd}
        >
          + 新增凭据
        </button>
      </div>

      <input
        className="rounded-lg border border-border-secondary bg-bg-secondary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
        placeholder="搜索名称 / 网址 / 用户名…"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />

      {error && (
        <div className="rounded-lg bg-error-soft px-3 py-2 text-sm text-error">{error}</div>
      )}

      {loading ? (
        <div className="py-8 text-center text-sm text-text-secondary">加载中…</div>
      ) : filtered.length === 0 ? (
        <div className="py-8 text-center text-sm text-text-secondary">
          {entries.length === 0 ? "还没有凭据，点右上角「新增凭据」开始" : "没有匹配的条目"}
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {filtered.map((e) => {
            const shown = revealed[e.id] !== undefined;
            const isConfirming = confirmingId === e.id;
            return (
              <div
                key={e.id}
                className="flex items-center gap-2 rounded-xl border border-border-primary bg-bg-card px-4 py-3 transition-colors hover:bg-bg-card-hover"
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
                    onClick={() => (shown ? maskReveal(e.id) : void reveal(e.id))}
                  >
                    {shown ? <IconEyeOff /> : <IconEye />}
                    {shown ? "掩回" : "显示"}
                  </button>
                  <button
                    className="group relative flex items-center rounded-lg p-1.5 text-text-secondary transition-colors hover:bg-bg-secondary"
                    aria-label="复制用户名"
                    onClick={() => copy(e.username, `${e.id}:user`)}
                  >
                    {copiedKey === `${e.id}:user` ? <IconCheck /> : <IconUser />}
                    <Tip label="复制用户名" />
                  </button>
                  {shown && (
                    <button
                      className="group relative flex items-center rounded-lg p-1.5 text-text-secondary transition-colors hover:bg-bg-secondary"
                      aria-label="复制密码"
                      onClick={() => copy(revealed[e.id], `${e.id}:secret`)}
                    >
                      {copiedKey === `${e.id}:secret` ? <IconCheck /> : <IconKey />}
                      <Tip label="复制密码" />
                    </button>
                  )}
                  <button
                    className="rounded-lg px-2 py-1 text-xs text-text-secondary transition-colors hover:bg-bg-secondary"
                    onClick={() => startEdit(e)}
                  >
                    编辑
                  </button>
                  {isConfirming ? (
                    <>
                      <button
                        className="rounded-lg px-2 py-1 text-xs font-medium text-error transition-colors hover:bg-error-soft"
                        onClick={() => void remove(e.id)}
                      >
                        确认删除
                      </button>
                      <button
                        className="rounded-lg px-2 py-1 text-xs text-text-secondary transition-colors hover:bg-bg-secondary"
                        onClick={() => setConfirmingId(null)}
                      >
                        取消
                      </button>
                    </>
                  ) : (
                    <button
                      className="rounded-lg px-2 py-1 text-xs text-error transition-colors hover:bg-error-soft"
                      onClick={() => setConfirmingId(e.id)}
                    >
                      删除
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {form && (
        <div
          className={`fixed inset-0 z-50 flex items-center justify-center bg-black/40 transition-opacity duration-150 ${
            modalShown ? "opacity-100" : "opacity-0"
          }`}
          onClick={() => setForm(null)}
        >
          <div
            className={`w-full max-w-md rounded-3xl border border-border-primary bg-bg-elevated p-5 shadow-modal transition-[opacity,transform] duration-200 ease-out motion-reduce:transition-none ${
              modalShown ? "scale-100 opacity-100" : "scale-95 opacity-0"
            }`}
            onClick={(ev) => ev.stopPropagation()}
          >
            <h3 className="mb-4 text-base font-semibold text-text-primary">
              {form.id ? "编辑凭据" : "新增凭据"}
            </h3>
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
                        className="w-full rounded-lg border border-border-secondary bg-bg-secondary py-2 pl-8 pr-3 text-sm text-text-primary outline-none focus:border-accent"
                        value={form[key]}
                        onChange={(ev) => setForm((f) => (f ? { ...f, [key]: ev.target.value } : f))}
                        autoComplete="off"
                      />
                    </div>
                  ) : (
                    <input
                      type={type}
                      className="rounded-lg border border-border-secondary bg-bg-secondary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
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
                    className="w-full rounded-lg border border-border-secondary bg-bg-secondary px-3 py-2 pr-10 text-sm text-text-primary outline-none focus:border-accent"
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
                    {showSecret ? <IconEyeOff /> : <IconEye />}
                    <Tip label={showSecret ? "隐藏密码" : "显示密码"} />
                  </button>
                </div>
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-xs text-text-secondary">备注</span>
                <textarea
                  rows={2}
                  className="resize-none rounded-lg border border-border-secondary bg-bg-secondary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
                  value={form.notes}
                  onChange={(ev) => setForm((f) => (f ? { ...f, notes: ev.target.value } : f))}
                />
              </label>
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button
                className="active:scale-[0.98] rounded-lg px-3 py-1.5 text-sm text-text-secondary transition-transform hover:bg-bg-secondary"
                onClick={() => setForm(null)}
              >
                取消
              </button>
              <button
                className="active:scale-[0.98] rounded-lg bg-accent px-3 py-1.5 text-sm text-on-accent transition-transform hover:bg-accent-hover disabled:opacity-50"
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
