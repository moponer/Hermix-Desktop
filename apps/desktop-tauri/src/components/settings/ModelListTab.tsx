// components/settings/ModelListTab.tsx
//
// 模型配置的主视图：**模型中心制**，不是供应商中心制。
//
// 一屏回答三个问题：我现在有哪些模型、哪个是默认、怎么测/改/删。
// 供应商只是模型的属性（内置 / 自定义标签），不再是一层要展开的树 ——
// 原 ModelSettingsTab 的"先选供应商再勾模型"多了一层心智负担，也是
// 双权威漂移 bug 的温床。
//
// 权威只有一处：后端 config.yaml。这里的 localStorage 只是缓存镜像，
// 所有写操作走既有写路径后重新拉取。

import { useCallback, useEffect, useMemo, useState } from "react";
import { Plus, Star, Trash2, Pencil, Zap, Loader2 } from "lucide-react";
import * as api from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import { ProviderIcon } from "../ProviderIcon";
import { ModelAddDialog } from "./ModelAddDialog";

interface ModelRow {
  id: string; // `${providerId}:${modelId}`
  modelId: string;
  providerId: string;
  providerName: string;
  isBuiltin: boolean;
  isDefault: boolean;
}

const MODEL_CONFIG_KEY = STORAGE_KEYS.modelConfig;

export function ModelListTab() {
  const [rows, setRows] = useState<ModelRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [testNote, setTestNote] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    setLoading(true);
    try {
      // 后端是权威：/api/model/list 给出已启用模型与默认模型。
      const [list, customs, meta] = await Promise.all([
        api.getModelList().catch(() => null),
        api.getCustomProviders().catch(() => []),
        api.getProviderMeta().catch(() => []),
      ]);
      const customIds = new Set(customs.map((c) => c.id));
      const metaNames = new Map(meta.map((m) => [m.id, m.id]));
      const def = list?.default_model ?? "";
      const built: ModelRow[] = (list?.models ?? []).map((m) => ({
        id: `${m.provider_id}:${m.model_id}`,
        modelId: m.model_id,
        providerId: m.provider_id,
        providerName: metaNames.get(m.provider_id) ?? m.provider_id,
        isBuiltin: !customIds.has(m.provider_id),
        isDefault: `${m.provider_id}:${m.model_id}` === def,
      }));
      // 自定义 provider 里登记了但还没进启用清单的模型也要露出来，
      // 否则"建了却看不到"会被当成保存失败。
      const seen = new Set(built.map((r) => r.id));
      for (const c of customs) {
        for (const model of c.models ?? []) {
          const id = `${c.id}:${model}`;
          if (seen.has(id)) continue;
          seen.add(id);
          built.push({
            id,
            modelId: model,
            providerId: c.id,
            providerName: c.display_name ?? c.id,
            isBuiltin: false,
            isDefault: id === def,
          });
        }
      }
      setRows(built);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const setDefault = useCallback(async (id: string) => {
    try {
      await api.saveModelList(
        rows.map((r) => ({ provider_id: r.providerId, model_id: r.modelId })),
        id,
      );
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [rows, load]);

  const remove = useCallback(async (row: ModelRow) => {
    try {
      if (!row.isBuiltin) {
        // 自定义 provider 的删除按 provider 走；模型级删除后端没有端点，
        // 这里是"把这个模型从启用清单里摘掉"。
        const remaining = rows.filter((r) => r.id !== row.id);
        await api.saveModelList(
          remaining.map((r) => ({ provider_id: r.providerId, model_id: r.modelId })),
          rows.find((r) => r.isDefault && r.id !== row.id)?.id ?? null,
        );
      } else {
        const remaining = rows.filter((r) => r.id !== row.id);
        await api.saveModelList(
          remaining.map((r) => ({ provider_id: r.providerId, model_id: r.modelId })),
          rows.find((r) => r.isDefault && r.id !== row.id)?.id ?? null,
        );
      }
      // 镜像同步到本地缓存，让聊天选择器立即看到（后端已是权威）。
      storage.setItem(
        MODEL_CONFIG_KEY,
        JSON.stringify({
          enabled: rows
            .filter((r) => r.id !== row.id)
            .map((r) => ({ providerId: r.providerId, modelId: r.modelId })),
          defaultModel: rows.find((r) => r.isDefault && r.id !== row.id)?.id ?? "",
        }),
      );
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [rows, load]);

  /** 行内测试：内置供应商没有保存的 base_url，只能用已存的 env key 走
   *  provider 级端点；自定义直接用它的 base_url。 */
  const testRow = useCallback(async (row: ModelRow) => {
    setTestingId(row.id);
    try {
      const customs = await api.getCustomProviders().catch(() => []);
      const custom = customs.find((c) => c.id === row.providerId);
      if (custom) {
        const r = await api.testCustomProviderConnection(row.providerId);
        setTestNote((n) => ({
          ...n,
          [row.id]: r.ok ? "连接成功" : `失败：${r.error ?? "未知原因"}`,
        }));
      } else {
        setTestNote((n) => ({
          ...n,
          [row.id]: "内置供应商：请在此行的编辑里用新 Key 测试连接",
        }));
      }
    } catch (e) {
      setTestNote((n) => ({ ...n, [row.id]: `测试失败：${e instanceof Error ? e.message : String(e)}` }));
    } finally {
      setTestingId(null);
    }
  }, []);

  const sorted = useMemo(
    () => [...rows].sort((a, b) => Number(b.isDefault) - Number(a.isDefault)),
    [rows],
  );

  return (
    <div className="p-6 max-w-3xl mx-auto">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h3 className="text-[length:var(--text-ui)] font-semibold text-text-primary">自定义模型</h3>
          <p className="text-xs text-text-muted mt-0.5">
            模型添加后写入 config.yaml 持久保存。内置供应商只需 API Key，自定义支持任何 OpenAI 兼容端点。
          </p>
        </div>
        <button onClick={() => setAddOpen(true)} className={primaryBtn}>
          <Plus className="icon-sm" />
          添加模型
        </button>
      </div>

      {error && (
        <div className="mt-3 px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/30 text-xs text-red-400">
          {error}
        </div>
      )}

      <div className="mt-4 rounded-xl border border-border-primary overflow-hidden">
        {loading ? (
          <div className="flex items-center gap-2 px-4 py-6 text-xs text-text-muted">
            <Loader2 className="icon-sm animate-spin" />
            正在读取模型配置…
          </div>
        ) : sorted.length === 0 ? (
          <div className="px-4 py-8 text-center text-xs text-text-muted">
            还没有配置任何模型。点右上角「添加模型」开始。
          </div>
        ) : (
          sorted.map((row) => (
            <div
              key={row.id}
              className="flex items-center gap-3 px-3 py-2.5 border-b border-border-subtle last:border-b-0 hover:bg-bg-hover"
            >
              <ProviderIcon providerId={row.providerId} className="icon-lg shrink-0" />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5">
                  <span className="text-[length:var(--text-ui)] text-text-primary truncate">{row.modelId}</span>
                  {row.isDefault && (
                    <span className="px-1.5 py-0.5 rounded text-[length:var(--text-meta)] bg-accent/15 text-accent shrink-0">默认</span>
                  )}
                </div>
                <div className="text-[length:var(--text-meta)] text-text-muted truncate">
                  {row.providerName} · {row.isBuiltin ? "内置" : "自定义"}
                  {testNote[row.id] && <span className="ml-1">· {testNote[row.id]}</span>}
                </div>
              </div>

              <div className="flex items-center gap-0.5 shrink-0">
                <IconBtn title="设为默认" onClick={() => void setDefault(row.id)}>
                  <Star className={`icon-sm ${row.isDefault ? "fill-current text-accent" : ""}`} />
                </IconBtn>
                <IconBtn title="编辑" onClick={() => setAddOpen(true)}>
                  <Pencil className="icon-sm" />
                </IconBtn>
                <IconBtn title="测试连接" onClick={() => void testRow(row)}>
                  {testingId === row.id ? <Loader2 className="icon-sm animate-spin" /> : <Zap className="icon-sm" />}
                </IconBtn>
                <IconBtn title="删除" onClick={() => void remove(row)}>
                  <Trash2 className="icon-sm" />
                </IconBtn>
              </div>
            </div>
          ))
        )}
      </div>

      {addOpen && <ModelAddDialog onClose={() => setAddOpen(false)} onSaved={() => void load()} />}
    </div>
  );
}

const primaryBtn =
  "flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-accent text-white hover:opacity-90 shrink-0";

function IconBtn({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      aria-label={title}
      className="h-7 w-7 rounded-md flex items-center justify-center text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
    >
      {children}
    </button>
  );
}
