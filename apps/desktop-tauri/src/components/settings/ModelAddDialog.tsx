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

import { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronLeft, Loader2, CheckCircle2, AlertCircle } from "lucide-react";
import * as api from "../../api";
import { writeModelConfigAndNotify } from "../../hooks/useCurrentModel";
import { PROVIDERS, catalogMismatchNote, type ProviderDef } from "./settingsUtils";
import { ProviderIcon } from "@hermix/preview/ProviderIcon";
import { Button, Modal } from "../ui";
import { SAVE, SAVING } from "../strings";

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
  /** 该供应商是否已配置密钥。**只传布尔量，绝不回填密钥本身**——
   *  内置在服务端 env、自定义在 config.yaml，都不该流到前端。
   *
   *  它不只是给界面看的：编辑一条已配好密钥的条目时密码框本来就是空的，
   *  没有这个标记就分不清「没配（该填）」和「已配（留空即不改）」，用户
   *  每次改无关字段都得重输一遍 Key——而重输本身还有打错的风险。 */
  hasKey?: boolean;
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
  // 密钥**永不回填**，只带一个"是否已配"的布尔量，用来把密码框的
  // placeholder / 提示语改成「已配置」还是「未配置」，让"留空 = 不改"
  // 这条语义对用户可见。
  const editingHasKey = editing?.hasKey ?? false;
  // 新增态同样要知道 key 是否已配：内置 provider 的密钥在 env 里，
  // 用户可能是"key 早就配好、现在加第二个模型"。没有这个查询时
  // 提示语会对已配好 key 的厂商说"尚未配置"。
  const [envHasKey, setEnvHasKey] = useState(false);
  useEffect(() => {
    const keyVar = choice?.kind === "builtin" ? choice.def.keyEnvVar : null;
    if (!keyVar || editingHasKey) {
      setEnvHasKey(false);
      return;
    }
    let alive = true;
    void api
      .getEnvVars()
      .then((envs) => {
        if (alive) setEnvHasKey(Boolean(envs[keyVar]?.is_set));
      })
      .catch(() => {
        if (alive) setEnvHasKey(false);
      });
    return () => {
      alive = false;
    };
  }, [choice, editingHasKey]);
  const hasKey = editingHasKey || envHasKey;
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
    // 三条路径，因为三类的密钥**存放位置不同**，前端能不能拿到也不同：
    //  · 内置：Key 只在服务端 env → inline 端点带 providerId，由服务端取。
    //  · 自定义且已配：Key 在 config.yaml → 走 provider 级 test-connection，
    //    服务端自己读配置。**这一条是"留空=不改"的正解**：此前这里因为
    //    密码框是空的就直接报「要输入 API Key」，可密钥明明已经存好了，
    //    用户只想改模型名却被迫重输一遍。
    //  · 自定义新建：还没落盘 → 必须现场填，走 inline。
    const isBuiltin = choice?.kind === "builtin";
    const savedCustom = !isBuiltin && editing?.isCustom === true && editingHasKey;
    if (!savedCustom && !effectiveBaseUrl.trim()) {
      setError("接口地址不能为空");
      return;
    }
    if (!isBuiltin && !savedCustom && !apiKey.trim()) {
      setError("接口地址与 API Key 都不能为空");
      return;
    }
    setTesting(true);
    setError(null);
    try {
      if (savedCustom) {
        const r = await api.testCustomProviderConnection(editing!.providerId);
        // 归一化成 InlineTestResponse，复用同一块结果展示，不必写第二套 UI。
        setTest({
          ok: r.ok,
          kind: r.ok ? null : "provider",
          status: r.status,
          latency_ms: 0,
          error: r.error,
          models: r.models?.map((m) => m.id) ?? null,
          model_found: null,
        });
        return;
      }
      const r = await api.testProviderInline({
        baseUrl: effectiveBaseUrl.trim(),
        apiKey: apiKey.trim(),
        model: modelId.trim() || undefined,
        providerId: isBuiltin ? choice.def.id : undefined,
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
        // 改名 = 把清单里的旧条目**换成**新条目，不是再加一条。
        //
        // 此前的实现只做「不在就追加」：改完名清单里会同时挂着新旧两条，
        // 而 `onSaved → load()` 拉回来的又是权威清单，用户看到的是"名字没
        // 变、旁边多了一个"这种最难自查的现象。
        //
        // 默认模型必须原样带回。`saveModelList(models, null)` 里的 null 到了
        // 后端是 `default_model: None`（update_model_list 直接写进 config.yaml），
        // 也就是「改个模型名顺手把默认模型清了」—— 而默认模型此刻可能正是
        // 被改名的这一条，那就得把它迁到新 id 上。
        const newId = modelId.trim();
        if (newId) {
          const cur = await api.getModelList();
          const isNew = (m: { provider_id: string; model_id: string }) =>
            `${m.provider_id}:${m.model_id}` === `${choice.def.id}:${newId}`;
          const oldModel = editing?.modelId;
          const oldId = oldModel ? `${choice.def.id}:${oldModel}` : null;
          let enabled = (cur?.models ?? []).map((m) => ({
            provider_id: m.provider_id,
            model_id: m.model_id,
          }));
          if (oldId && oldModel !== newId) {
            enabled = enabled.filter((m) => `${m.provider_id}:${m.model_id}` !== oldId);
          }
          if (!enabled.some(isNew)) {
            enabled.push({ provider_id: choice.def.id, model_id: newId });
          }
          const def = cur?.default_model ?? null;
          // 统一写路径：镜像 + 事件 + 后端一次完成。此前裸调 saveModelList，
          // 聊天端选择器要等下次挂载 reconcile 才能看到新增/改名。
          await writeModelConfigAndNotify({
            enabled: enabled.map((m) => ({ providerId: m.provider_id, modelId: m.model_id })),
            defaultModel: (def && def === oldId ? `${choice.def.id}:${newId}` : def) ?? "",
          });
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
  return (
    <Modal
      open
      onClose={onClose}
      width="base"
      scrim
      /* 表单弹窗：点遮罩不该把填了一半的表单扔掉。 */
      closeOnBackdrop={false} closeOnEscape={false}
      title={
        <span className="flex items-center gap-2">
          {choice && (
            <button
              type="button"
              onClick={() => { setChoice(null); setTest(null); setError(null); }}
              className="grid size-8 shrink-0 place-items-center rounded-[var(--radius-md)] text-[var(--color-text-muted)] transition-colors hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
              aria-label="返回上一步"
            >
              <ChevronLeft className="icon-lg" />
            </button>
          )}
          <span className="truncate">
            {editing
              ? `编辑 ${choice && choice.kind === "builtin" ? choice.def.name : "自定义"} 模型`
              : choice
                ? `添加 ${choice.kind === "builtin" ? choice.def.name : "自定义"} 模型`
                : "添加模型"}
          </span>
        </span>
      }
      bodyClassName="overflow-y-auto px-4 py-4 space-y-4"
      footer={
        choice ? (
          <>
            <Button variant="secondary" size="base" onClick={onClose}>
              取消
            </Button>
            <Button
              variant="primary"
              size="base"
              onClick={() => void save()}
              disabled={saving}
              loading={saving}
            >
              {saving ? SAVING : SAVE}
            </Button>
          </>
        ) : undefined
      }
    >
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
                className="w-full flex items-center gap-3 px-3 py-2.5 rounded-[var(--radius-md)] bg-bg-tertiary/50 transition-colors hover:bg-bg-quaternary text-left"
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
              className="w-full flex items-center gap-3 px-3 py-2.5 rounded-[var(--radius-md)] bg-bg-tertiary/40 transition-colors hover:bg-bg-quaternary text-left"
            >
              <span className="w-4 h-4 shrink-0 rounded-full bg-bg-quaternary" />
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

          <Field
            label="API Key"
            hint={
              hasKey
                ? `已配置。留空则保持当前密钥不变——改端点或模型名不必重输。${
                    choice && choice.kind === "builtin"
                      ? `（保存在 ${choice.def.keyEnvVar}）`
                      : ""
                  }`
                : "尚未配置。留空表示不设置；该供应商在配好密钥前不会出现在模型列表里。"
            }
          >
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={hasKey ? "留空保持不变" : "sk-..."}
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
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-[var(--radius-md)] bg-bg-tertiary text-[length:var(--text-ui)] text-text-secondary transition-colors hover:bg-bg-quaternary hover:text-text-primary disabled:opacity-40"
            >
              {testing && <Loader2 className="icon-sm animate-spin" />}
              测试连接
            </button>
            {test && <TestResult r={test} />}
          </div>

          <details className="rounded-[var(--radius-md)] bg-bg-tertiary/50 px-3 py-2">
            <summary className="text-xs text-text-secondary cursor-pointer">高级配置</summary>
            <div className="mt-2 space-y-1.5">
              <Cap label="支持工具调用" checked={caps.tools} onChange={(v) => setCaps((c) => ({ ...c, tools: v }))} />
              <Cap label="支持图片输入" checked={caps.images} onChange={(v) => setCaps((c) => ({ ...c, images: v }))} />
              <Cap label="支持思考模式" checked={caps.thinking} onChange={(v) => setCaps((c) => ({ ...c, thinking: v }))} />
            </div>
          </details>

          {error && (
            <div className="px-3 py-2 rounded-[var(--radius-md)] bg-[var(--color-error-soft)] text-[length:var(--text-ui)] text-[var(--color-error)]">
              {error}
            </div>
          )}
        </>
      )}
    </Modal>
  );
}

const inputCls =
  "mt-1 w-full rounded-[var(--radius-md)] bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40";

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
      <span className="flex flex-wrap items-center gap-1.5 text-xs text-text-secondary">
        <CheckCircle2 className="icon-sm shrink-0 text-green-500" />
        连接成功{r.latency_ms > 0 ? ` · ${r.latency_ms}ms` : ""}
        {r.model_found === false && (
          /* 「模型名」指的是**厂商 /v1/models 那份目录**，不是本应用的模型清单
             —— 原措辞「但列表里没找到这个模型名」指代不清，读起来像在说
             「你配的这个模型不存在」。这里点明来源，并把后端拿回来的目录原样
             摊开：用户据此能自己分辨「名字写错了」还是「端点不对」，而这正是
             拿平台侧模型 id 去问厂商 API 时最常见的两种情况。 */
          <span className="text-text-faint">
            {catalogMismatchNote(r.models)}
          </span>
        )}
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
