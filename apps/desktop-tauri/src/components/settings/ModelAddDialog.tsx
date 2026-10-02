// components/settings/ModelAddDialog.tsx
//
// 添加模型：两步。第一步选"内置供应商 / 自定义"，第二步填连接信息并可直接
// 测试连接（不必先保存）。
//
// 为什么分两步而不是一屏塞满：内置供应商只需要一个 API Key（端点地址是
// 已知的），自定义才需要地址 + Key + 模型名 + 高级配置。把两类混在一张
// 表单里，内置用户要面对一堆跟他无关的输入框（原 ModelSettingsTab 就是
// 这么做的，1000 行、三层折叠，双权威漂移的 bug 也从这儿来）。
//
// 测试连接走 `POST /api/providers/test`：**未落盘的配置也能测**，避免"先
// 建一个 provider 再测、取消时留下垃圾"。

import { useCallback, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { X, ChevronLeft, Loader2, CheckCircle2, AlertCircle } from "lucide-react";
import * as api from "../../api";
import { PROVIDERS, type ProviderDef } from "./settingsUtils";
import { ProviderIcon } from "../ProviderIcon";

type Choice =
  | { kind: "builtin"; def: ProviderDef }
  | { kind: "custom" };

/** 编辑目标：打开弹窗时预填当前这条模型，保存走"更新"而非"新建"。 */
export interface EditingTarget {
  providerId: string;
  modelId: string;
  isCustom: boolean;
  /** 自定义项的展示名（自定义 id 由展示名派生，回填时保持一致）。 */
  displayName?: string;
  /** 自定义项的端点基址。内置项留空——用 PROVIDERS 里的官方地址。 */
  baseUrl?: string;
}

interface Props {
  onClose: () => void;
  /** 保存完成：内置走 env var + 启用模型，自定义走 createCustomProvider。 */
  onSaved: () => void;
  /** 非空 = 编辑模式：预填该模型，标题与保存语义都变。 */
  editing?: EditingTarget;
}

export function ModelAddDialog({ onClose, onSaved, editing }: Props) {
  const editChoice: Choice | null = !editing
    ? null
    : editing.isCustom
      ? { kind: "custom" }
      : {
          kind: "builtin",
          def: PROVIDERS.find((p) => p.id === editing.providerId) ?? PROVIDERS[0],
        };
  const [choice, setChoice] = useState<Choice | null>(editChoice);
  const [apiKey, setApiKey] = useState("");
  // 编辑时预填端点：自定义用存下来的；内置用该厂商的官方地址（用户仍可改，
  // 代理/中转场景要能覆盖）。
  const [baseUrl, setBaseUrl] = useState(() => {
    if (!editing) return "";
    if (editing.baseUrl) return editing.baseUrl;
    return editChoice && editChoice.kind === "builtin" ? editChoice.def.baseUrl : "";
  });
  const [modelId, setModelId] = useState(editing?.modelId ?? "");
  const [displayName, setDisplayName] = useState(editing?.displayName ?? "");
  const [caps, setCaps] = useState({ tools: true, images: false, thinking: false });
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<api.InlineTestResponse | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 内置供应商选中后带出它的官方端点；用户仍能改（代理/中转场景）。
  const effectiveBaseUrl = useMemo(
    () => (choice?.kind === "builtin" ? baseUrl || choice.def.baseUrl : baseUrl),
    [choice, baseUrl],
  );

  const testNow = useCallback(async () => {
    // 内置项不要求本地填 Key——密钥在后端 env 里，由服务端按 providerId 取。
    // 端点地址仍然必填（它是测试目标，且允许用户改成代理地址）。
    const needsKey = choice?.kind !== "builtin";
    if (!effectiveBaseUrl.trim() || (needsKey && !apiKey.trim())) {
      setError(needsKey ? "接口地址与 API Key 都不能为空" : "接口地址不能为空");
      return;
    }
    setTesting(true);
    setError(null);
    try {
      const r = await api.testProviderInline({
        baseUrl: effectiveBaseUrl.trim(),
        apiKey: apiKey.trim(),
        model: modelId.trim() || undefined,
        providerId: choice?.kind === "builtin" ? choice.def.id : undefined,
      });
      setTest(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setTesting(false);
    }
  }, [effectiveBaseUrl, apiKey, modelId, choice]);

  const save = useCallback(async () => {
    if (!choice) return;
    setSaving(true);
    setError(null);
    try {
      if (choice.kind === "builtin") {
        // 内置供应商的凭据是环境变量（MINIMAX_API_KEY 这类），与后端
        // agent_builder 的读取方式保持一致，不另造存储。
        // 编辑态允许不填 Key：空值保持原样，不把已配好的 Key 洗掉。
        if (apiKey.trim()) {
          await api.setEnvVar(choice.def.keyEnvVar, apiKey.trim());
        }
        // 模型名已预填时确保它在启用清单里（编辑内置模型 = 保住它可用）。
        if (modelId.trim()) {
          const cur = await api.getModelList();
          const id = `${choice.def.id}:${modelId.trim()}`;
          const existing = (cur?.models ?? []).map((m) => ({
            provider_id: m.provider_id,
            model_id: m.model_id,
          }));
          if (!existing.some((m) => `${m.provider_id}:${m.model_id}` === id)) {
            await api.saveModelList(
              [...existing, { provider_id: choice.def.id, model_id: modelId.trim() }],
              null,
            );
          }
        }
      } else {
        // 自定义：POST /api/custom-providers 是 upsert（entry().or_insert 后
        // 覆盖字段），编辑时沿用原 id 就完成更新，不必先删再建。
        const safeId =
          editing?.providerId ??
          `custom-${(displayName.trim() || "provider")
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, "-")
            .replace(/^-|-$/g, "")}`;
        await api.createCustomProvider({
          id: safeId,
          display_name: displayName.trim() || safeId,
          base_url: effectiveBaseUrl.trim(),
          api_key: apiKey.trim(),
          models: modelId.trim() || undefined,
        });
      }
      onSaved();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }, [choice, apiKey, displayName, effectiveBaseUrl, modelId, editing, onSaved, onClose]);

  return createPortal(
    <div
      className="fixed inset-0 z-[110] flex items-center justify-center"
      style={{ background: "var(--color-overlay-scrim)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="添加模型"
        className="flex flex-col rounded-xl border border-border-primary shadow-modal overflow-hidden"
        style={{ width: "min(94vw, 560px)", maxHeight: "86vh", background: "var(--color-bg-app)" }}
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-border-primary bg-bg-secondary shrink-0">
          {choice && (
            <button
              onClick={() => { setChoice(null); setTest(null); setError(null); }}
              className="h-7 w-7 rounded-md flex items-center justify-center text-text-muted hover:text-text-primary hover:bg-bg-hover"
              aria-label="返回上一步"
            >
              <ChevronLeft className="icon-md" />
            </button>
          )}
          <span className="text-sm font-semibold text-text-primary">
            {editing
              ? `编辑 ${choice && choice.kind === "builtin" ? choice.def.name : "自定义"} 模型`
              : choice
                ? `添加 ${choice.kind === "builtin" ? choice.def.name : "自定义"} 模型`
                : "添加模型"}
          </span>
          <button
            onClick={onClose}
            className="ml-auto h-7 w-7 rounded-md flex items-center justify-center text-text-muted hover:text-text-primary hover:bg-bg-hover"
            aria-label="关闭"
          >
            <X className="icon-md" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-4 py-4 space-y-4">
          {!choice && (
            <>
              <p className="text-xs text-text-muted">
                内置供应商只需填 API Key；自定义用于任何 OpenAI 兼容端点。
              </p>
              <div className="space-y-1.5">
                {PROVIDERS.map((p) => (
                  <button
                    key={p.id}
                    onClick={() => setChoice({ kind: "builtin", def: p })}
                    className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg border border-border-secondary hover:bg-bg-hover text-left"
                  >
                    <ProviderIcon providerId={p.id} className="icon-lg" />
                    <span className="flex-1 min-w-0">
                      <span className="block text-[length:var(--text-ui)] text-text-primary truncate">{p.name}</span>
                      <span className="block text-[length:var(--text-meta)] text-text-muted truncate">{p.description}</span>
                    </span>
                    <span className="text-[length:var(--text-meta)] text-text-faint shrink-0">内置</span>
                  </button>
                ))}
                <button
                  onClick={() => setChoice({ kind: "custom" })}
                  className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg border border-dashed border-border-secondary hover:bg-bg-hover text-left"
                >
                  <span className="w-4 h-4 shrink-0 rounded-full border border-dashed border-border-secondary" />
                  <span className="flex-1 min-w-0">
                    <span className="block text-[length:var(--text-ui)] text-text-primary">自定义</span>
                    <span className="block text-[length:var(--text-meta)] text-text-muted">
                      任何 OpenAI 兼容端点（LM Studio / Ollama / 中转）
                    </span>
                  </span>
                  <span className="text-[length:var(--text-meta)] text-text-faint shrink-0">自定义</span>
                </button>
              </div>
            </>
          )}

          {choice && (
            <>
              {choice.kind === "custom" && (
                <Field label="显示名称">
                  <input
                    value={displayName}
                    onChange={(e) => setDisplayName(e.target.value)}
                    placeholder="例如 我的中转"
                    className={inputCls}
                  />
                </Field>
              )}

              <Field
                label="接口地址"
                hint={choice.kind === "builtin" ? "已按官方端点预填，用中转/代理时可改" : undefined}
              >
                <input
                  value={effectiveBaseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                  placeholder="https://api.example.com/v1"
                  className={inputCls}
                />
              </Field>

              <Field label="API Key">
                <input
                  type="password"
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                  placeholder="sk-..."
                  className={inputCls}
                />
              </Field>

              <Field label="模型名称" hint="留空则用该端点的默认模型">
                <input
                  value={modelId}
                  onChange={(e) => setModelId(e.target.value)}
                  placeholder="例如 deepseek-chat"
                  className={inputCls}
                />
              </Field>

              <div className="flex items-center gap-2">
                <button
                  onClick={() => void testNow()}
                  disabled={testing}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-border-secondary text-xs text-text-secondary hover:text-text-primary disabled:opacity-40"
                >
                  {testing && <Loader2 className="icon-sm animate-spin" />}
                  测试连接
                </button>
                {test && <TestResult r={test} />}
              </div>

              <details className="rounded-lg border border-border-secondary px-3 py-2">
                <summary className="text-xs text-text-secondary cursor-pointer">高级配置</summary>
                <div className="mt-2 space-y-1.5">
                  <Cap label="支持工具调用" checked={caps.tools} onChange={(v) => setCaps((c) => ({ ...c, tools: v }))} />
                  <Cap label="支持图片输入" checked={caps.images} onChange={(v) => setCaps((c) => ({ ...c, images: v }))} />
                  <Cap label="支持思考模式" checked={caps.thinking} onChange={(v) => setCaps((c) => ({ ...c, thinking: v }))} />
                </div>
              </details>

              {error && (
                <div className="px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/30 text-xs text-red-400">
                  {error}
                </div>
              )}
            </>
          )}
        </div>

        {choice && (
          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-border-primary bg-bg-secondary shrink-0">
            <button onClick={onClose} className={btnGhost}>取消</button>
            <button onClick={() => void save()} disabled={saving} className={btnPrimary}>
              {saving && <Loader2 className="icon-sm animate-spin" />}
              保存
            </button>
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}

const inputCls =
  "mt-1 w-full px-3 py-2 rounded-lg bg-bg-tertiary text-[length:var(--text-ui)] text-text-primary border border-border-secondary outline-none focus:border-accent transition-colors";
const btnPrimary =
  "flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-accent text-white hover:opacity-90 disabled:opacity-50";
const btnGhost =
  "px-3 py-1.5 rounded-lg text-xs text-text-secondary border border-border-secondary hover:text-text-primary";

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="text-[length:var(--text-meta)] text-text-secondary">{label}</span>
      {children}
      {hint && <span className="block mt-1 text-[length:var(--text-meta)] text-text-faint">{hint}</span>}
    </label>
  );
}

function Cap({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-center gap-2 text-xs text-text-secondary cursor-pointer">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label}
    </label>
  );
}

/** 按后端返回的 kind 给中文结论——用户要的是"下一步做什么"，不是 HTTP 状态码。 */
function TestResult({ r }: { r: api.InlineTestResponse }) {
  if (r.ok) {
    return (
      <span className="flex items-center gap-1.5 text-xs text-green-500">
        <CheckCircle2 className="icon-sm" />
        连接成功{r.latency_ms > 0 ? ` · ${r.latency_ms}ms` : ""}
        {r.model_found === false && " · 但列表里没找到这个模型名"}
      </span>
    );
  }
  const why =
    r.kind === "apikey" ? "API Key 无效或无权访问"
      : r.kind === "quota" ? "额度不足或触发限流"
        : r.kind === "network" ? "连不上（地址错误 / 网络不通）"
          : r.kind === "bad_request" ? "请求被拒（端点或 Key 格式不对）"
            : "服务端异常";
  return (
    <span className="flex items-center gap-1.5 text-xs text-red-400">
      <AlertCircle className="icon-sm" />
      {why}
    </span>
  );
}
