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
import { Plus, Star, Power, PowerOff, Trash2, Pencil, Zap, Loader2 } from "lucide-react";
import * as api from "../../api";
import { syncModelConfigLocal, writeModelConfigAndNotify } from "../../hooks/useCurrentModel";
import { ProviderIcon } from "@hermix/preview/ProviderIcon";
import { ModelAddDialog, type EditingTarget } from "./ModelAddDialog";
import { PROVIDERS, catalogMismatchNote } from "./settingsUtils";
import type { CustomProviderSummary } from "../../api";
import { DELETE, FAILED, LOADING, failure } from "../strings";
import { Badge, ConfirmDialog, SettingsGroup, SettingsRow, Switch } from "../ui";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { openExternalUrl } from "../../desktopBridge";

interface ModelRow {
  id: string; // `${providerId}:${modelId}`
  modelId: string;
  providerId: string;
  providerName: string;
  isBuiltin: boolean;
  isDefault: boolean;
  /** 内置免费渠道：显示"免费源"标记（数据会流经第三方，见列表顶部说明）。 */
  isFree: boolean;
  /**
   * 上游对该模型的说明（**逐条免责 / 使用条款**）。
   *
   * 社区免费网关的每个模型条目都带（"Prompts may be logged by the upstream
   * provider … not suitable for production or sensitive data"）——不显示
   * 就等于用户不知道自己同意了谁的条款。
   */
  note?: string;
  /**
   * 是否已在后端的启用清单（`model_selector.enabled`）里。
   *
   * 与"可添加"行严格分开：此前两者混在一起，点一次「设为默认」就把整份
   * 自定义/免费清单写进 enabled，付费厂商的模型被挤出可见范围。
   */
  isEnabled: boolean;
}

/** inline 测试的 `kind` → 中文。后端已做四分类，这里只做展示层翻译，
 *  不让用户对着一串 HTTP 状态码猜。 */
function inlineErrorText(r: api.InlineTestResponse): string {
  if (r.error) return r.error;
  switch (r.kind) {
    case "apikey": return "API Key 无效或未配置";
    case "quota": return "额度不足或触发限流";
    case "network": return "网络不可达";
    case "bad_request": return "端点地址有误";
    default: return `服务端返回 ${r.status}`;
  }
}

/** `model_found === false` 时的说明，文案与共用实现见 settingsUtils。 */

export function ModelListTab() {
  const [rows, setRows] = useState<ModelRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [testNote, setTestNote] = useState<Record<string, string>>({});
  /** 非空 = 弹窗处于编辑态，预填这一条。null = 新增。 */
  const [editing, setEditing] = useState<EditingTarget | null>(null);
  const [customs, setCustoms] = useState<CustomProviderSummary[]>([]);
  /** 后端 env 实况（GET /api/env，仅 is_set 可信，值已脱敏）。内置
   *  provider 的"密钥已配"判定用它，不再硬编码 true —— 硬编码会让
   *  编辑弹窗对未配 key 的厂商说"已配置"，用户留空保存后跑起来才报错。 */
  const [envVars, setEnvVars] = useState<Record<string, { is_set: boolean }>>({});
  /** 社区免费模型总开关的状态（后端 config.yaml 是唯一权威）。
   *  null = 后端没这个接口（老后端）——不是「关」，更不是「关着」。 */
  const [freeChannel, setFreeChannel] = useState<api.FreeChannelState | null>(null);
  /** 首次读取是否完成：区分「还在读」与「读不到」。 */
  const [freeRead, setFreeRead] = useState(false);
  /** 开关写入中：期间禁用开关，避免连点两次写出两种状态。 */
  const [freeBusy, setFreeBusy] = useState(false);
  /** 待确认的「关闭」动作：免费源是唯一可用模型时，关掉就没模型可用了。 */
  const [confirmDisableFree, setConfirmDisableFree] = useState(false);

  const load = useCallback(async (opts?: { silent?: boolean }) => {
    // 静默刷新：后台拉取完成后回填列表，不让整页再闪一次加载态。
    if (!opts?.silent) setLoading(true);
    try {
      // 后端是权威：/api/model/list 给出已启用模型与默认模型。
      const [list, customs, meta, envs, free] = await Promise.all([
        api.getModelList().catch(() => null),
        api.getCustomProviders().catch(() => []),
        api.getProviderMeta().catch(() => []),
        api.getEnvVars().catch(() => ({})),
        api.getFreeChannel().catch(() => null),
      ]);
      const customIds = new Set(customs.map((c) => c.id));
      const freeIds = new Set(customs.filter((c) => c.is_free_channel).map((c) => c.id));
      setCustoms(customs);
      setEnvVars(envs);
      setFreeChannel(free);
      setFreeRead(true);
      const metaNames = new Map(meta.map((m) => [m.id, m.id]));
      const def = list?.default_model ?? "";
      const toRow = (p: { provider_id: string; model_id: string; note?: string }, isEnabled: boolean): ModelRow => {
        const id = `${p.provider_id}:${p.model_id}`;
        const name =
          metaNames.get(p.provider_id) ??
          customs.find((c) => c.id === p.provider_id)?.display_name ??
          p.provider_id;
        return {
          id,
          modelId: p.model_id,
          providerId: p.provider_id,
          providerName: name,
          isBuiltin: !customIds.has(p.provider_id),
          isDefault: id === def,
          isFree: freeIds.has(p.provider_id),
          note: p.note,
          isEnabled,
        };
      };
      const built: ModelRow[] = (list?.models ?? []).map((m) => toRow(m, true));
      const seen = new Set(built.map((r) => r.id));
      // 已配置但未启用（后端 catalog）：显式"可添加"，点星才进 enabled。
      for (const m of list?.catalog ?? []) {
        const row = toRow(m, false);
        if (seen.has(row.id)) continue;
        seen.add(row.id);
        built.push(row);
      }
      // 自定义 provider 里登记了但后端没列出的模型也要露出来，
      // 否则"建了却看不到"会被当成保存失败。
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
            isFree: c.is_free_channel === true,
            isEnabled: false,
          });
        }
      }
      setRows(built);
      setError(null);
      // 后端权威清单镜像进本地缓存 + 广播事件：聊天端 useModelSelector
      // 事件回调立即读到最新（手机面/手工改 config.yaml 等绕过前端写路径
      // 的变更，也在下次进设置页时被这次镜像兜住）。
      if (list) {
        syncModelConfigLocal({
          enabled: (list.models ?? []).map((m) => ({ providerId: m.provider_id, modelId: m.model_id })),
          defaultModel: list.default_model ?? "",
        });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (!opts?.silent) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const setDefault = useCallback(async (id: string) => {
    try {
      // 只改默认，不碰启用清单：启用 / 默认是两个独立概念。
      // 星标只负责"设默认"，启用 / 禁用由电源键（toggleEnabled）负责。
      // 被设为默认的必须是已启用项——前端对未启用行的星标 disable，
      // 避免 default_model 指向未启用模型。
      const enabled = rows.filter((r) => r.isEnabled);
      await writeModelConfigAndNotify({
        enabled: enabled.map((r) => ({ providerId: r.providerId, modelId: r.modelId })),
        defaultModel: id,
      });
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [rows, load]);

  // 启用 / 禁用（与"设为默认"是两个独立动作）。
  // 禁用当前默认模型时，default 顺延到第一个剩余启用项，避免 default
  // 指向未启用模型。
  const toggleEnabled = useCallback(async (row: ModelRow) => {
    try {
      const willEnable = !row.isEnabled;
      const enabledRows = rows.filter((r) => r.isEnabled);
      const nextEnabled = willEnable
        ? [...enabledRows, row]
        : enabledRows.filter((r) => r.id !== row.id);

      const currentDefault = rows.find((r) => r.isDefault)?.id ?? "";
      const nextDefault =
        !willEnable && currentDefault === row.id
          ? nextEnabled[0]?.id ?? ""
          : currentDefault;

      await writeModelConfigAndNotify({
        enabled: nextEnabled.map((r) => ({ providerId: r.providerId, modelId: r.modelId })),
        defaultModel: nextDefault,
      });
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [rows, load]);

  /**
   * 开启后社区清单是**后台**拉的（几秒后到），开关响应不可能等它 ——
   * 这里轮询模型数，涨了就静默回填列表，让「开则拉取」在界面上看得见。
   */
  const pollFreeModels = useCallback(
    async (baseline: number) => {
      for (let i = 0; i < 10; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        const st = await api.getFreeChannel().catch(() => null);
        if (!st) return;
        if (st.models > baseline) {
          await load({ silent: true });
          return;
        }
      }
      // 预算用尽也刷一次：上游清单可能与 seed 完全重合（数量不涨但内容变了）。
      await load({ silent: true });
    },
    [load],
  );

  /**
   * 社区免费模型总开关 —— 真正落盘的那一步。
   *
   * 「开关 ↔ 数据」的联动口径：写入走**后端**（config.yaml 是唯一权威；
   * 后端同步摘除/重建免费条目并热推送），写完**回读**再渲染 —— 不做
   * 乐观更新：乐观值在写失败时会让界面显示「已关」而后台还在拉。
   */
  const applyFreeSwitch = useCallback(async (next: boolean) => {
    setFreeBusy(true);
    setError(null);
    try {
      const res = await api.setFreeChannelEnabled(next);
      await load();
      if (next) {
        // 基线取「开完之后的条数」（seed 清单）：社区清单拉到后会涨，
        // 涨了才静默回填列表。
        const st = await api.getFreeChannel().catch(() => null);
        void pollFreeModels(st?.models ?? 0);
      }
      toastSuccess(
        next
          ? `已开启社区免费模型（${res.seeded_models} 个已启用），正在拉取社区模型清单…`
          : `已关闭社区免费模型，移除了 ${res.removed_models} 个免费条目。`,
      );
    } catch (e) {
      // 回读真实状态：开关不能停在没落盘的值上。
      await load();
      const msg = e instanceof Error ? e.message : String(e);
      setError(`免费模型开关未生效：${msg}`);
      toastError(`免费模型开关未生效：${msg}`);
    } finally {
      setFreeBusy(false);
    }
  }, [load, pollFreeModels]);

  /**
   * 开关的点击入口。关之前先看一件事：**免费源是不是当前唯一能用的模型** ——
   * 是的话关闭等于让应用没有模型可用（条目会被摘掉），先确认再动手。
   */
  const toggleFreeChannel = useCallback(
    (next: boolean) => {
      if (!next) {
        const enabled = rows.filter((r) => r.isEnabled);
        if (enabled.length > 0 && enabled.every((r) => r.isFree)) {
          setConfirmDisableFree(true);
          return;
        }
      }
      void applyFreeSwitch(next);
    },
    [rows, applyFreeSwitch],
  );

  const remove = useCallback(async (row: ModelRow) => {
    try {
      // 模型级删除后端没有端点，这里是"把这个模型从启用清单里摘掉"。
      // 未启用的目录行不参与写回——否则删除动作会顺手启用别的目录行。
      const remaining = rows.filter((r) => r.isEnabled && r.id !== row.id);
      const nextDefault = remaining.find((r) => r.isDefault)?.id ?? "";
      await writeModelConfigAndNotify({
        enabled: remaining.map((r) => ({ providerId: r.providerId, modelId: r.modelId })),
        defaultModel: nextDefault,
      });
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [rows, load]);

  /** 行内测试。三条路径，因为凭据存放位置不同：
   *  · 内置项：Key 只在后端 env 里，前端读不到 → 把 providerId 交给
   *    inline 端点，由服务端按 `env_var_name_for_provider` 取。Key 不过网。
   *  · 自定义项：Key 在 config.yaml → 走 provider 级 test-connection，
   *    由服务端自己读配置。
   *  · 免费源：上面的端点都只打 GET /models，测不出**单个**免费模型的
   *    死活（免费池下架只体现为调用时 401/404）→ 走 1-token 真实探测；
   *    401/404 顺手上报摘除并刷新列表——死行当场消失，不再「明明测挂了
   *    却一直躺在列表里」。 */
  const testRow = useCallback(async (row: ModelRow) => {
    setTestingId(row.id);
    setTestNote((n) => ({ ...n, [row.id]: "" }));
    try {
      if (row.isFree) {
        const r = await api.probeFreeModel(row.modelId);
        if (r.ok) {
          setTestNote((n) => ({ ...n, [row.id]: "连接正常 · 模型可用" }));
        } else if (r.status === 401 || r.status === 403 || r.status === 404) {
          setTestNote((n) => ({
            ...n,
            [row.id]: failure("已失效", "上游拒绝了这个模型（401/404），已从列表移除。", `HTTP ${r.status}`),
          }));
          const dead = await api.reportDeadModel(row.modelId);
          if (dead.removed) await load();
        } else if (r.status === 0) {
          // status 0 = 传输层就失败了（DNS/连接/超时），**网关压根没连上**。
          // 这多半是网络不可达（企业网/内网直连/严格防火墙），不是模型坏了 ——
          // 文案必须说清楚，否则用户会去清模型、而真正的原因是网络。
          setTestNote((n) => ({
            ...n,
            [row.id]: failure(
              FAILED,
              "连不上社区网关（网络不可达），不是模型失效——请检查网络/代理，或改用自有端点。",
              r.error ?? "网络错误",
            ),
          }));
        } else {
          setTestNote((n) => ({
            ...n,
            [row.id]: failure(FAILED, "网关可达，但这次探测没成功（限流/超时不算失效，不会移除）。", r.error ?? `HTTP ${r.status}`),
          }));
        }
        return;
      }
      const builtin = PROVIDERS.find((p) => p.id === row.providerId);
      if (builtin) {
        const r = await api.testProviderInline({
          baseUrl: builtin.baseUrl,
          apiKey: "",
          model: row.modelId,
          providerId: builtin.id,
        });
        setTestNote((n) => ({
          ...n,
          [row.id]: r.ok
            ? `连接正常 · ${r.latency_ms} ms${r.model_found === false ? catalogMismatchNote(r.models) : ""}`
            : failure(FAILED, "请检查模型配置后重试。", inlineErrorText(r)),
        }));
      } else {
        const r = await api.testCustomProviderConnection(row.providerId);
        setTestNote((n) => ({
          ...n,
          [row.id]: r.ok ? "连接正常" : failure(FAILED, "请检查供应商配置后重试。", r.error ?? "未知原因"),
        }));
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setTestNote((n) => ({ ...n, [row.id]: failure("测试失败", "请检查网络后重试。", msg) }));
    } finally {
      setTestingId(null);
    }
  }, [load]);

  const sorted = useMemo(
    () =>
      [...rows].sort(
        (a, b) =>
          Number(b.isDefault) - Number(a.isDefault) ||
          Number(b.isEnabled) - Number(a.isEnabled),
      ),
    [rows],
  );

  return (
    <div className="p-6 max-w-3xl mx-auto">
      <div className="flex items-center justify-between gap-4">
        <h2 className="text-[length:var(--text-title)] font-semibold text-text-primary">模型</h2>
        <button onClick={() => { setEditing(null); setAddOpen(true); }} className={primaryBtn}>
          <Plus className="icon-sm" />
          添加模型
        </button>
      </div>

      {error && (
        <div className="mt-3 px-3 py-2 rounded-[var(--radius-md)] bg-red-500/12 text-[length:var(--text-ui)] text-red-400">
          {error}，请重试。
        </div>
      )}

      {/* 社区免费模型总开关：开 = 从社区公开端点拉取；关 = 不再拉取，
          并把已拉取的免费条目（目录 / 启用清单 / 默认模型）一并移除。 */}
      <SettingsGroup
        className="mt-4"
        title="社区免费模型"
        description="免费模型来自社区公开端点，随上游上下架变动，随时可能失效。"
      >
        <SettingsRow
          title="启用社区免费模型"
          description={
            !freeRead
              ? "读取开关状态中…"
              : freeChannel === null
                ? "当前后端不支持这个开关（需更新后端）。"
                : freeChannel.enabled
                  ? `已开启 · 当前登记 ${freeChannel.models} 个免费模型。关闭后不再拉取，已拉取的条目会被移除。`
                  : "已关闭 · 不再拉取社区免费模型，之前拉取的条目已移除。"
          }
          control={
            <Switch
              checked={freeChannel?.enabled === true}
              disabled={freeBusy || freeChannel === null}
              onChange={(v) => void toggleFreeChannel(v)}
              label="启用社区免费模型"
            />
          }
        />
        {/* 网关地址：连的是谁必须写明 —— 用户要能自己判断可达性、自己换端点。 */}
        {freeChannel?.endpoints && freeChannel.endpoints.length > 0 && (
          <SettingsRow
            title="网关地址"
            description={
              <span className="flex flex-col gap-2">
                {freeChannel.endpoints.map((ep) => (
                  <span key={ep.id} className="flex flex-col gap-0.5">
                    <span>{ep.display_name}</span>
                    <span>
                      推理
                      <UrlLink url={ep.base_url} />
                    </span>
                    <span>
                      模型清单
                      <UrlLink url={ep.models_url} />
                    </span>
                  </span>
                ))}
              </span>
            }
          />
        )}
      </SettingsGroup>

      {/* 免责 + 可达性：第三方端点必写的两件事 ——
          ① 数据流向谁、上游条款是什么；② 部分网络根本连不上（那是网络问题，
          不是模型坏了）。 */}
      {(freeChannel?.enabled || sorted.some((r) => r.isFree)) && (
        <div className="mt-3 px-3 py-2 rounded-[var(--radius-md)] bg-bg-tertiary/50 text-[length:var(--text-meta)] text-text-muted space-y-1">
          <p>
            对话内容会发往上面的第三方端点；上游声明可能记录提示词并用于改进服务，
            <span className="text-text-warning">请勿用于敏感内容</span>。
            每个免费模型自己的免责/使用条款见列表该行下方。
          </p>
          <p>
            部分网络环境（企业网、内网直连、严格防火墙）访问不到社区网关，
            表现为「测试连接」超时或失败 —— 那是网络不可达，不是模型坏了；
            需要时可改用自有端点（config.yaml 的 free_channel.base_url / models_url）。
          </p>
        </div>
      )}

      <div className="mt-4 overflow-hidden rounded-[var(--radius-lg)] bg-bg-card shadow-[var(--shadow-paper)]">
        {loading ? (
          <div className="flex items-center gap-2 px-4 py-6 text-xs text-text-muted" aria-busy="true">
            <Loader2 className="icon-sm animate-spin" />
            {LOADING}
          </div>
        ) : sorted.length === 0 ? (
          <div className="px-4 py-8 text-center text-xs text-text-muted">
            还没有配置任何模型。点右上角「添加模型」开始。
          </div>
        ) : (
          sorted.map((row) => (
            <div
              key={row.id}
              className="flex items-center gap-3 px-3 py-2.5 hover:bg-bg-hover"
            >
              {/* 厂商 logo 放在 28px 中性底块里：全站列表行都是同尺寸的
                  方块，模型行不能是个 16px 的裸 logo。底保持中性 —— 品牌
                  logo 自带颜色，铺在同色实底上会互相抵消（"logo 压 logo"），
                  所以这里用中性底而不是实心品牌底。 */}
              <span className="cat-icon cat-icon--sm shrink-0">
                <ProviderIcon providerId={row.providerId} size={16} />
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5">
                  <span className="text-[length:var(--text-ui)] text-text-primary truncate">{row.modelId}</span>
                  {row.isDefault && (
                    <span className="px-1.5 py-0.5 rounded text-[length:var(--text-meta)] bg-accent/15 text-accent shrink-0">默认</span>
                  )}
                  {row.isFree && (
                    <Badge tone="success" size="chip" pill={false}>免费源</Badge>
                  )}
                  {!row.isEnabled && (
                    <span className="text-[length:var(--text-meta)] text-text-faint shrink-0">未启用</span>
                  )}
                </div>
                <div className="text-[length:var(--text-meta)] text-text-muted truncate">
                  {row.providerName} · {row.isBuiltin ? "内置" : "自定义"}
                  {testNote[row.id] && <span className="ml-1">· {testNote[row.id]}</span>}
                </div>
                {/* 上游对该模型的免责/使用条款（社区免费网关的每条都带）。 */}
                {row.note && (
                  <p
                    className="mt-1 line-clamp-1 text-[length:var(--text-meta)] text-text-warning"
                    title={row.note}
                  >
                    {row.note}
                  </p>
                )}
              </div>

              <div className="flex items-center gap-0.5 shrink-0">
                <IconBtn
                  title={row.isEnabled ? "禁用该模型" : "启用该模型"}
                  onClick={() => void toggleEnabled(row)}
                >
                  {/* 两个状态用两个图标 + 语义色：绿=运行中，红=已停。
                      只换颜色不换图标的话，色弱用户分不出状态。 */}
                  {row.isEnabled ? (
                    <Power className="icon-sm text-[var(--color-success)]" />
                  ) : (
                    <PowerOff className="icon-sm text-[var(--color-error)]" />
                  )}
                </IconBtn>
                <IconBtn
                  title={row.isDefault ? "当前默认模型" : row.isEnabled ? "设为默认" : "请先启用该模型"}
                  disabled={!row.isEnabled}
                  onClick={() => void setDefault(row.id)}
                >
                  <Star className={`icon-sm ${row.isDefault ? "fill-current text-accent" : ""}`} />
                </IconBtn>
                <IconBtn title="编辑" onClick={() => { setEditing({
                  providerId: row.providerId,
                  modelId: row.modelId,
                  isCustom: !row.isBuiltin,
                  displayName: row.isBuiltin ? undefined : row.providerName,
                  baseUrl: row.isBuiltin
                    ? undefined
                    : customs.find((c) => c.id === row.providerId)?.base_url,
                  // èªå®ä¹é¡¹åç«¯ç´æ¥ç» has_keyï¼åç½®é¡¹æ²¡è¿ä¸ªå­æ®µï¼ä½è½åºç°å¨è¿å¼ åè¡¨éæ¬èº«å°±è¯´æå¯é¥å·²éââæå¡ç«¯ /api/model/list æ provider_has_key è¿æ»¤è¿ï¼æ¿å®å½çç¸æ¯è¾åå ä¸ä¸ªç«¯ç¹äº¤ç»æå¡ç«¯æ¥åã
                  hasKey: row.isBuiltin
                    ? Boolean(envVars[PROVIDERS.find((p) => p.id === row.providerId)?.keyEnvVar ?? ""]?.is_set)
                    : Boolean(customs.find((c) => c.id === row.providerId)?.has_key),
                }); setAddOpen(true); }}>
                  <Pencil className="icon-sm" />
                </IconBtn>
                <IconBtn title="测试连接" onClick={() => void testRow(row)}>
                  {testingId === row.id ? <Loader2 className="icon-sm animate-spin" /> : <Zap className="icon-sm" />}
                </IconBtn>
                {row.isEnabled && (
                  <IconBtn title={`${DELETE}模型`} onClick={() => void remove(row)}>
                    <Trash2 className="icon-sm" />
                  </IconBtn>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {addOpen && (
        <ModelAddDialog
          editing={editing ?? undefined}
          onClose={() => { setAddOpen(false); setEditing(null); }}
          onSaved={() => void load()}
        />
      )}

      {/* 免费源是唯一可用模型时，关掉它等于让应用没有模型可用 —— 先确认。 */}
      <ConfirmDialog
        open={confirmDisableFree}
        title="关闭社区免费模型"
        body="当前启用的模型全部来自社区免费源。关闭后这些条目会被移除，应用将没有可用模型，直到你添加并启用一个付费模型。"
        confirmLabel="仍然关闭"
        onCancel={() => setConfirmDisableFree(false)}
        onConfirm={() => {
          setConfirmDisableFree(false);
          void applyFreeSwitch(false);
        }}
      />
    </div>
  );
}

const primaryBtn =
  "flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-accent text-on-accent hover:opacity-90 shrink-0";

/**
 * 网关地址链接：显示完整 URL，点开交给系统浏览器。
 *
 * 不用 `<a href>`：Tauri 窗口里点外链会把整个应用导航走，必须走
 * `openExternalUrl`（Tauri 的 open crate）。用户要能自己核对端点、判断
 * 可达性、必要时照着改自己的 config.yaml。
 */
function UrlLink({ url }: { url: string }) {
  if (!url) return null;
  return (
    <button
      type="button"
      onClick={() => void openExternalUrl(url)}
      title={`在浏览器打开 ${url}`}
      className="ml-1 font-mono text-[length:var(--text-meta)] text-accent underline underline-offset-2 hover:opacity-80 break-all text-left"
    >
      {url}
    </button>
  );
}

function IconBtn({
  title,
  onClick,
  children,
  disabled = false,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      aria-label={title}
      disabled={disabled}
      className={`h-7 w-7 rounded-md flex items-center justify-center text-text-muted hover:text-text-primary hover:bg-bg-tertiary ${
        disabled ? "opacity-40 cursor-not-allowed hover:text-text-muted hover:bg-transparent" : ""
      }`}
    >
      {children}
    </button>
  );
}
