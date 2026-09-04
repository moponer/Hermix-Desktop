/**
 * WorkspacePanels — 工作台管理面板集合（模板 / 提醒 / 设置）。
 *
 * 从画布搬到侧栏二级菜单后的承载组件：三者都是「打开才拉数据」的懒加载
 * 面板，内容由本组件自持，不向画布回传状态——写完只调 applyWorkspaceDetail
 * / bumpWorkspaceData 通知画布刷新。
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2, LayoutGrid, Undo2, Lock, FolderOpen, BarChart3 } from "lucide-react";
import * as api from "../../api";
import type {
  ChangeEntry,
  TemplateMeta,
  WorkspaceAutomation,
  WorkspaceDetail,
  WorkspaceModule,
  AutomationRunsEntry,
} from "../../types";
import { toastError, toastSuccess } from "../../hooks/useToast";
import {
  applyWorkspaceDetail,
  bumpWorkspaceData,
  createWorkspaceFromTemplate,
} from "../../hooks/useWorkspaceRegistry";

function tableModules(modules: WorkspaceModule[] = []): Extract<WorkspaceModule, { type: "table" }>[] {
  return modules.filter(
    (m): m is Extract<WorkspaceModule, { type: "table" }> => m.type === "table",
  );
}

/** 预置台账模板 — 选中即创建，创建中禁用全部条目防连点。 */
export function TemplatePanel({ onPicked }: { onPicked: () => void }) {
  const [templates, setTemplates] = useState<TemplateMeta[] | null>(null);
  const [creating, setCreating] = useState<string | null>(null);

  useEffect(() => {
    api
      .listWorkspaceTemplates()
      .then(setTemplates)
      .catch((e) => toastError(`加载模板失败: ${String(e)}`));
  }, []);

  const pick = useCallback(
    async (tpl: TemplateMeta) => {
      if (creating) return;
      setCreating(tpl.id);
      try {
        if (await createWorkspaceFromTemplate(tpl.id)) onPicked();
      } finally {
        setCreating(null);
      }
    },
    [creating, onPicked],
  );

  if (templates === null) return <PanelHint loading text="加载模板中…" />;
  if (templates.length === 0) return <PanelHint text="暂无可用模板" />;

  return (
    <div className="flex max-h-[46vh] flex-col gap-1.5 overflow-y-auto">
      {templates.map((t) => (
        <button
          key={t.id}
          disabled={creating !== null}
          onClick={() => void pick(t)}
          className="flex w-full flex-col gap-0.5 rounded-md border px-2.5 py-2 text-left transition-opacity hover:opacity-85 disabled:opacity-50"
          style={{ borderColor: "var(--color-border-secondary)", background: "var(--color-bg-base)" }}
        >
          <span className="flex items-center gap-1.5 text-[13px] font-medium" style={{ color: "var(--color-text-primary)" }}>
            {creating === t.id ? <Loader2 className="icon-sm animate-spin" /> : <LayoutGrid className="icon-sm" />}
            {t.name}
          </span>
          <span className="text-[12px] leading-snug" style={{ color: "var(--color-text-secondary)" }}>
            {t.description}
          </span>
          <span className="text-[11px]" style={{ color: "var(--color-text-faint)" }}>
            {t.tables.map((tb) => tb.name).join(" · ")}
          </span>
        </button>
      ))}
    </div>
  );
}

/** 工作台提醒：定义清单 + 最近触发记录。 */
export function AutomationPanel({ workspaceId }: { workspaceId: string }) {
  const [defs, setDefs] = useState<WorkspaceAutomation[] | null>(null);
  const [runs, setRuns] = useState<AutomationRunsEntry[]>([]);

  useEffect(() => {
    Promise.all([api.listWorkspaceAutomations(workspaceId), api.workspaceAutomationRuns(workspaceId)])
      .then(([d, r]) => {
        setDefs(d);
        setRuns(r);
      })
      .catch((e) => toastError(`加载提醒失败: ${String(e)}`));
  }, [workspaceId]);

  if (defs === null) return <PanelHint loading text="加载提醒中…" />;
  if (defs.length === 0) {
    return <PanelHint text="还没有提醒 —— 在对话中告诉 Hermix「新增年检临期提醒」即可" />;
  }

  return (
    <div className="flex max-h-[46vh] flex-col gap-1.5 overflow-y-auto">
      {defs.map((a) => {
        const history = runs.find((r) => r.automation_id === a.id)?.runs ?? [];
        const isCond = a.kind === "condition";
        return (
          <div
            key={a.id}
            className="rounded-md border px-2.5 py-2"
            style={{ borderColor: "var(--color-border-secondary)", background: "var(--color-bg-base)" }}
          >
            <div className="flex items-center gap-1.5 text-[12px] font-medium" style={{ color: "var(--color-text-primary)" }}>
              {a.name}
              <span className="rounded px-1 text-[11px]" style={{ background: "var(--color-bg-elevated)", color: "var(--color-text-faint)" }}>
                {isCond ? "条件提醒" : "定时分析"}
              </span>
            </div>
            <div className="mt-0.5 text-[11px]" style={{ color: "var(--color-text-secondary)" }}>
              {isCond ? <>频率 {a.frequency} · 条件 <code className="text-[11px]">{a.condition}</code></> : <>计划 {a.schedule}</>}
            </div>
            {history.length > 0 && (
              <div className="mt-1 border-t pt-1 text-[11px]" style={{ borderColor: "var(--color-border-subtle)", color: "var(--color-text-faint)" }}>
                最近触发 {history.length} 次 · 最新 {new Date(history[0].started_at).toLocaleString()}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** 工作台设置：只读保护 + 模块清单 + 各表变更历史（可撤销）。 */
export function SettingsPanel({ detail }: { detail: WorkspaceDetail }) {
  const [saving, setSaving] = useState(false);
  const [changes, setChanges] = useState<Record<string, ChangeEntry[]>>({});
  const [undoing, setUndoing] = useState<string | null>(null);
  const tables = tableModules(detail.schema.modules);
  const readonly = detail.schema.permissions?.mode === "readonly";

  const loadChanges = useCallback(() => {
    for (const t of tables) {
      api
        .listTableChanges(detail.id, t.id)
        .then((list) => setChanges((prev) => ({ ...prev, [t.id]: list })))
        .catch(() => undefined);
    }
  }, [detail.id, tables]);

  useEffect(loadChanges, [loadChanges]);

  const toggleReadonly = useCallback(async () => {
    if (saving) return;
    setSaving(true);
    try {
      const cur = (detail.schema.permissions ?? {}) as { mode?: string; allow_delete?: boolean };
      const mode = readonly ? ("readwrite" as const) : ("readonly" as const);
      const schema = { ...detail.schema, permissions: { ...cur, mode } };
      await api.updateWorkspaceSchema(detail.id, schema);
      applyWorkspaceDetail({ ...detail, schema });
      toastSuccess(readonly ? "已恢复可读写" : "已开启只读保护");
    } catch (e) {
      toastError(`保存权限失败: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  }, [detail, readonly, saving]);

  const undo = useCallback(
    async (tableId: string) => {
      if (undoing) return;
      setUndoing(tableId);
      try {
        const undone = await api.undoLastChange(detail.id, tableId);
        if (!undone) {
          toastError("该表没有可撤销的变更");
          return;
        }
        const label = undone.op === "insert" ? "新增" : undone.op === "update" ? "修改" : "删除";
        toastSuccess(`已撤销「${label}」操作`);
        loadChanges();
        bumpWorkspaceData();
      } catch (e) {
        toastError(`撤销失败: ${String(e)}`);
      } finally {
        setUndoing(null);
      }
    },
    [detail.id, undoing, loadChanges],
  );

  return (
    <div className="flex max-h-[56vh] flex-col gap-2 overflow-y-auto">
      <div
        className="rounded-md border px-2.5 py-2"
        style={{ borderColor: "var(--color-border-secondary)", background: "var(--color-bg-base)" }}
      >
        <div className="flex items-center justify-between gap-2">
          <div className="text-[12px]" style={{ color: "var(--color-text-primary)" }}>
            {readonly ? <span className="inline-flex items-center gap-1"><Lock className="icon-sm" />只读保护</span> : "可读写"}
          </div>
          <button
            onClick={() => void toggleReadonly()}
            disabled={saving}
            className="rounded-md border px-2 py-0.5 text-[11px] transition-opacity hover:opacity-80 disabled:opacity-50"
            style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-secondary)" }}
          >
            {saving ? <Loader2 className="icon-sm animate-spin" /> : readonly ? "恢复可写" : "开启只读"}
          </button>
        </div>
        <div className="mt-1 text-[11px]" style={{ color: "var(--color-text-faint)" }}>
          只读保护下所有写入（含 LLM 工具）都会被拒绝
        </div>
      </div>

      <div className="flex flex-col gap-1">
        <div className="text-[11px]" style={{ color: "var(--color-text-faint)" }}>
          模块（{detail.schema.modules?.length ?? 0}）
        </div>
        {(detail.schema.modules ?? []).map((m) => (
          <div
            key={m.id}
            className="flex items-center justify-between rounded border px-2 py-1 text-[12px]"
            style={{ borderColor: "var(--color-border-subtle)", color: "var(--color-text-secondary)" }}
          >
            <span className="truncate">{m.type === "table" ? <FolderOpen className="icon-sm" /> : <BarChart3 className="icon-sm" />} {m.name}</span>
            <span className="shrink-0 text-[11px]" style={{ color: "var(--color-text-faint)" }}>
              {m.type === "table" ? `${m.fields.length} 字段` : `${m.widgets?.length ?? 0} 组件`}
            </span>
          </div>
        ))}
      </div>

      <div className="flex flex-col gap-1.5">
        <div className="text-[11px]" style={{ color: "var(--color-text-faint)" }}>
          变更历史（撤销最近操作）
        </div>
        {tables.length === 0 ? (
          <div className="text-[11px]" style={{ color: "var(--color-text-faint)" }}>暂无表模块</div>
        ) : (
          tables.map((t) => {
            const list = changes[t.id] ?? [];
            return (
              <div key={t.id} className="rounded border px-2 py-1.5" style={{ borderColor: "var(--color-border-subtle)" }}>
                <div className="flex items-center justify-between">
                  <span className="truncate text-[12px]" style={{ color: "var(--color-text-secondary)" }}>{t.name}</span>
                  <button
                    onClick={() => void undo(t.id)}
                    disabled={undoing !== null || list.length === 0}
                    className="flex shrink-0 items-center gap-1 rounded border px-1.5 py-0.5 text-[11px] transition-opacity hover:opacity-80 disabled:opacity-40"
                    style={{ borderColor: "var(--color-border-secondary)", color: "var(--color-text-secondary)" }}
                    title="撤销该表最近一次操作"
                  >
                    {undoing === t.id ? <Loader2 size={10} className="animate-spin" /> : <Undo2 size={10} />}
                    撤销
                  </button>
                </div>
                {list.length === 0 ? (
                  <div className="mt-0.5 text-[11px]" style={{ color: "var(--color-text-faint)" }}>暂无变更</div>
                ) : (
                  <div className="mt-0.5 flex flex-col gap-0.5">
                    {list.slice(0, 3).map((c) => (
                      <div key={c.seq} className="flex items-center gap-1.5 text-[11px]" style={{ color: "var(--color-text-faint)" }}>
                        <span
                          className="rounded px-1"
                          style={{
                            background:
                              c.op === "insert"
                                ? "var(--color-bg-elevated)"
                                : c.op === "update"
                                  ? "var(--color-bg-tertiary)"
                                  : "var(--color-error-bg, var(--color-bg-elevated))",
                          }}
                        >
                          {c.op === "insert" ? "新增" : c.op === "update" ? "修改" : "删除"}
                        </span>
                        <span className="truncate">
                          {c.changed_at.slice(0, 16).replace("T", " ")} · {c.changed_by}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function PanelHint({ text, loading = false }: { text: string; loading?: boolean }) {
  return (
    <div className="flex items-center gap-1.5 py-3 text-[12px]" style={{ color: "var(--color-text-faint)" }}>
      {loading && <Loader2 className="icon-md animate-spin" />}
      {text}
    </div>
  );
}
