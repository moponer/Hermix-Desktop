// components/settings/PersonasPanel.tsx
//
// 角色（persona）管理：内置角色全文只读展示，用户角色全文可编辑
// （保存 / 删除）。数据来自 /api/persona/personalities（内置人设的
// 全文由 GET 直接返回；PUT 对内置名返回 403，前端直接禁用编辑）。
// SOUL.md 是全局人格底座，一并在本视图提供全文编辑。
// 默认人设切换写 config.personality（PUT /api/config 为 merge-patch
// 语义：传名字即设、传 null 即清除回退到 SOUL.md / 默认灵魂）。

import { useCallback, useEffect, useState } from "react";
import { Check, Loader2, RotateCcw, Save, Trash2 } from "lucide-react";
import * as api from "../../api";
import { Button, ConfirmDialog, SkeletonCard } from "../ui";

/** 内置人设的中文显示名；自定义人设直接展示文件名。 */
const DISPLAY_NAMES: Record<string, string> = {
  fengpo: "疯批美人",
  duzi: "毒舌直球",
  coder: "工程师",
  writer: "写作者",
  analyst: "分析师",
};

export function PersonasPanel() {
  const [list, setList] = useState<api.PersonalitySummary[]>([]);
  const [soul, setSoul] = useState<api.PersonaTextFile | null>(null);
  const [active, setActive] = useState<string | null>(null); // persona 名或 "SOUL.md"
  const [content, setContent] = useState("");
  const [builtin, setBuiltin] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [defaultPersona, setDefaultPersona] = useState<string | null>(null);
  const [settingDefault, setSettingDefault] = useState(false);

  const openPersona = async (name: string, isBuiltin: boolean) => {
    setActive(name);
    setBuiltin(isBuiltin);
    setDirty(false);
    setNotice(null);
    try {
      const file = await api.getPersonality(name);
      setContent(file.content ?? "");
    } catch {
      setContent("");
    }
  };

  const loadList = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [people, soulFile, cfg] = await Promise.all([
        api.listPersonalities(),
        api.getPersonaSoul().catch(() => null),
        api.getConfig().catch(() => null),
      ]);
      setList(people);
      setSoul(soulFile);
      const cfgPersona = cfg && typeof cfg.personality === "string" ? cfg.personality : null;
      setDefaultPersona(cfgPersona || null);
      if (!active && people.length > 0) openPersona(people[0].name, people[0].is_builtin);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    loadList();
  }, [loadList]);

  /** 写 config.personality（merge-patch：名字即设，null 即清除回退默认灵魂）。 */
  const switchDefault = async (name: string | null) => {
    setSettingDefault(true);
    setNotice(null);
    try {
      await api.updateConfig({ personality: name });
      setDefaultPersona(name);
      setNotice(
        name
          ? `已把「${DISPLAY_NAMES[name] ?? name}」设为默认人设，下一条消息生效。`
          : "已切回默认灵魂（SOUL.md / 出厂人格），下一条消息生效。",
      );
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    } finally {
      setSettingDefault(false);
    }
  };

  const openSoul = async () => {
    setActive("SOUL.md");
    setBuiltin(false);
    setDirty(false);
    setNotice(null);
    try {
      const file = soul ?? (await api.getPersonaSoul());
      setSoul(file);
      setContent(file.content ?? "");
    } catch {
      setContent("");
    }
  };

  const save = async () => {
    if (active == null) return;
    setSaving(true);
    setNotice(null);
    try {
      if (active === "SOUL.md") {
        await api.savePersonaSoul(content);
        setSoul({ path: soul?.path ?? "<soul>", content, updated_at: null });
      } else {
        await api.savePersonality(active, content);
      }
      setDirty(false);
      setNotice("已保存，下一次对话生效。");
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };
  const doDelete = async (name: string) => {
    setConfirmDelete(null);
    try {
      await api.removePersonality(name);
      setActive(null);
      await loadList();
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="space-y-3">
      {error && (
        <p className="rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] px-4 py-3 text-[length:var(--text-meta)] text-[var(--color-text-secondary)]">
          角色接口不可用（{error}）。
        </p>
      )}
      <div className="grid grid-cols-1 gap-3 md:grid-cols-[220px_1fr]">
        {/* 列表 */}
        <div className="space-y-1 rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] p-1.5">
          <button
            onClick={openSoul}
            className={`flex w-full items-center justify-between rounded-md px-2.5 py-1.5 text-left text-[length:var(--text-meta)] ${
              active === "SOUL.md"
                ? "bg-[var(--color-bg-active)] text-[var(--color-accent)]"
                : "text-[var(--color-text-primary)] hover:bg-[var(--color-bg-hover)]"
            }`}
          >
            <span className="font-medium">SOUL.md</span>
            <span className="text-[var(--color-text-muted)]">全局</span>
          </button>
          {list.map((p) => (
            <button
              key={p.name}
              onClick={() => openPersona(p.name, p.is_builtin)}
              className={`flex w-full items-center justify-between rounded-md px-2.5 py-1.5 text-left text-[length:var(--text-meta)] ${
                active === p.name
                  ? "bg-[var(--color-bg-active)] text-[var(--color-accent)]"
                  : "text-[var(--color-text-primary)] hover:bg-[var(--color-bg-hover)]"
              }`}
            >
              <span className="flex min-w-0 items-center gap-1.5">
                <span className="truncate font-medium">{DISPLAY_NAMES[p.name] ?? p.name}</span>
                {defaultPersona === p.name && (
                  <Check className="size-3 shrink-0 text-[var(--color-accent)]" />
                )}
              </span>
              <span className={p.is_builtin ? "text-[var(--color-text-muted)]" : "text-[var(--color-accent)]"}>
                {defaultPersona === p.name ? "默认" : p.is_builtin ? "内置" : "自定义"}
              </span>
            </button>
          ))}
          {loading && <SkeletonCard lines={1} />}
        </div>

        {/* 编辑区 */}
        <div className="space-y-2 rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] p-3">
          {active == null ? (
            <p className="py-10 text-center text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
              选择左侧角色查看提示词全文。
            </p>
          ) : (
            <>
              <div className="flex items-center gap-2">
                <h3 className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
                  {active === "SOUL.md"
                    ? "SOUL.md（默认灵魂）"
                    : `${DISPLAY_NAMES[active] ?? active}（${active}）`}
                </h3>
                {builtin && (
                  <span className="rounded-full bg-[var(--color-bg-tertiary)] px-2 py-0.5 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
                    内置角色只读 — 可另存为自定义版本
                  </span>
                )}
                <div className="ml-auto flex items-center gap-2">
                  {active === "SOUL.md" ? (
                    defaultPersona ? (
                      <Button variant="tonal-outline" size="sm" onClick={() => switchDefault(null)} disabled={settingDefault}>
                        <RotateCcw className="size-3.5" />
                        切回默认灵魂（清掉 {DISPLAY_NAMES[defaultPersona] ?? defaultPersona}）
                      </Button>
                    ) : (
                      <span className="rounded-full bg-[var(--color-bg-tertiary)] px-2 py-0.5 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
                        默认生效中
                      </span>
                    )
                  ) : (
                    defaultPersona !== active && (
                      <Button variant="tonal-outline" size="sm" onClick={() => switchDefault(active)} disabled={settingDefault}>
                        {settingDefault ? <Loader2 className="size-3.5 animate-spin" /> : <Check className="size-3.5" />}
                        设为默认
                      </Button>
                    )
                  )}
                  {!builtin && active !== "SOUL.md" && (
                    <Button variant="danger-outline" size="sm" onClick={() => setConfirmDelete(active)}>
                      <Trash2 className="size-3.5" /> 删除
                    </Button>
                  )}
                  {!builtin && (
                    <Button variant="primary" size="sm" onClick={save} disabled={saving || (!dirty && active !== "SOUL.md")}>
                      {saving ? <Loader2 className="size-3.5 animate-spin" /> : <Save className="size-3.5" />}
                      保存
                    </Button>
                  )}
                </div>
              </div>
              <textarea
                value={content}
                onChange={(e) => { setContent(e.target.value); setDirty(true); }}
                readOnly={builtin}
                spellCheck={false}
                className="h-[380px] w-full resize-y rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] p-3 font-mono text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-primary)] outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
              />
              {notice && (
                <p className="text-[length:var(--text-meta)] text-[var(--color-text-secondary)]">{notice}</p>
              )}
            </>
          )}
        </div>
      </div>

      <ConfirmDialog
        open={confirmDelete != null}
        title={`删除角色「${confirmDelete ?? ""}」？`}
        body="删除后使用该角色的对话将回退到默认人设。"
        onConfirm={() => confirmDelete && doDelete(confirmDelete)}
        onCancel={() => setConfirmDelete(null)}
      />
    </div>
  );
}
