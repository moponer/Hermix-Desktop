// MCP 内置目录：可浏览 + 一键安装。
//
// 设计对齐参考项目的市场面板：分类 → 卡片（图标 + 名称 + 一句描述 + 安装按钮），
// 但**把安全结论摆在安装按钮旁边**——这是我们和普通市场最大的区别：目录里的
// 东西要执行在用户机器上，"能不能装"必须是看得见的判断，不是藏进日志。
//
// 三档（后端 mcp_scan 判定，前端只负责如实呈现）：
// - pass  可直接使用      → 直接装
// - warn  需确认后使用    → 展示将执行的命令 + 逐条风险，用户确认后装
// - block 已拒绝          → 按钮禁用，列出命中的规则
//
// 2026-10-04：目录改为**纯本地内置**（后端取消官方 registry / 自建子注册中心
// 两个远程源），随之三处变化：
// 1. 数据源选择条整条删除——只有一个源，没有"切"的对象；
// 2. 刷新按钮与 `?refresh=1` 一并删除——目录是编译期打包的常量，没有可刷新的
//    远端；网络失败态也就不再存在（取不到只可能是后端异常，文案照实说）；
// 3. **补上环境变量输入**：21 条里 13 条需要用户填自己的凭据，此前这里硬编码
//    `env: {}`，点安装必然 400 —— 它们其实一直是"看得见装不上"。凭据对话框
//    是这批条目能否使用的唯一前提，优先级高于任何目录美化。
import { useEffect, useMemo, useState } from "react";
import { Loader2, ShieldAlert, Terminal } from "lucide-react";
import * as api from "../../api/rest";
import type { CatalogEntry } from "../../api/types";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { Button, ConfirmDialog, Modal } from "../ui";
import { LOADING } from "../strings";
import { InitialIcon } from "../ui/InitialIcon";

/** 目录类目 → 图标色调。
 *
 *  2026-10-23：这里原本有一张**本地** CATEGORY_TONE 表，其中「开发」写的是
 *  violet。而 categoryTone.ts 的统一口径里「开发 / 编程 / software-development」
 *  一律是 blue —— 同义类目在两个地方给了两个色相，就是漂移本身。
 *  本地表已删除，改由 categoryTone 单点决定，四页 + 本页必然一致。
 *  这也顺带修掉「工具集卡片是蓝、MCP 目录同名词条是紫」的错位。 */

type EnvMap = Record<string, string>;

export function McpCatalogGrid({ onInstalled }: { onInstalled: () => void }) {
  const [items, setItems] = useState<CatalogEntry[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [category, setCategory] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  /** 需要二次确认的条目（把命令与风险摊开给用户看） */
  const [confirming, setConfirming] = useState<CatalogEntry | null>(null);
  /** 正在收集凭据的条目 */
  const [envTarget, setEnvTarget] = useState<CatalogEntry | null>(null);
  const [envDraft, setEnvDraft] = useState<EnvMap>({});
  /** 最近一次安装尝试用的 env —— 428 重发时必须原样带上，否则会因缺 env 被 400 挡回 */
  const [lastEnv, setLastEnv] = useState<EnvMap>({});

  const load = () => {
    setLoading(true);
    api
      .getMcpCatalog()
      .then((res) => {
        if (!res) { setFailed(true); return; }
        setItems(res.items);
      })
      .catch(() => setFailed(true))
      .finally(() => setLoading(false));
  };

  useEffect(() => { load(); }, []);

  const categories = useMemo(() => {
    const set = new Set<string>();
    for (const i of items ?? []) set.add(i.category);
    return Array.from(set);
  }, [items]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (items ?? []).filter((i) => {
      if (category && i.category !== category) return false;
      if (!q) return true;
      return [i.title, i.titleZh, i.description, i.descriptionZh, i.id]
        .filter(Boolean)
        .some((s) => String(s).toLowerCase().includes(q));
    });
  }, [items, category, query]);

  const doInstall = async (item: CatalogEntry, acknowledge: boolean, env: EnvMap) => {
    setBusyId(item.id);
    try {
      const res = await api.installMcpFromCatalog(item.id, env, acknowledge);
      if (res.ok) {
        toastSuccess(`已添加「${item.titleZh ?? item.title}」`);
        setConfirming(null);
        setEnvTarget(null);
        onInstalled();
      } else if (res.needAck && !acknowledge) {
        // 服务端 428：把命令与风险摊开给用户看过再带 acknowledge 重发。
        // env 必须原样带上——重发是一次全新的请求，不带就又缺 env 了。
        setLastEnv(env);
        setConfirming(item);
      } else {
        toastError(res.error);
      }
    } finally {
      setBusyId(null);
    }
  };

  /** 点「安装」：需凭据的先收集凭据，零凭据的直接装。 */
  const startInstall = (item: CatalogEntry) => {
    if (item.requiredEnv.length > 0) {
      setEnvTarget(item);
      setEnvDraft(lastEnv);
    } else {
      void doInstall(item, false, {});
    }
  };

  const envComplete = (item: CatalogEntry) =>
    item.requiredEnv.every((k) => (envDraft[k] ?? "").trim() !== "");

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-[length:var(--text-meta)] text-text-muted py-6 justify-center">
        <Loader2 className="icon-md animate-spin" />
        {LOADING}
      </div>
    );
  }
  if (failed || !items) {
    return (
      <div className="rounded-lg bg-bg-card p-4 text-[length:var(--text-meta)] text-text-secondary">
        读取内置目录失败。
        <div className="mt-1 text-text-muted">
          目录是打包在应用内的常量，不依赖网络；取不到通常是后端未就绪。
          重试无效时请查看日志，或先用下方的「手动添加」配置。
        </div>
        <button
          onClick={load}
          className="mt-2 h-7 px-2.5 rounded-md bg-bg-tertiary hover:bg-bg-hover"
        >
          重试
        </button>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">内置目录</span>
        <span className="text-[length:var(--text-tiny)] text-text-faint">{visible.length} 个</span>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索名称或描述"
          className="ml-auto h-7 px-2.5 rounded-md bg-bg-tertiary text-[length:var(--text-meta)] outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] w-44"
        />
      </div>

      {categories.length > 1 && (
        <div className="flex items-center gap-1.5 flex-wrap">
          <button
            onClick={() => setCategory(null)}
            aria-pressed={category === null}
            className={`chip-flat${category === null ? " chip-flat--on" : ""}`}
          >
            全部
          </button>
          {categories.map((c) => (
            <button
              key={c}
              onClick={() => setCategory(category === c ? null : c)}
              aria-pressed={category === c}
              className={`chip-flat${category === c ? " chip-flat--on" : ""}`}
            >
              {c}
            </button>
          ))}
        </div>
      )}

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-2 max-h-[420px] overflow-y-auto pr-1">
        {visible.map((item) => {
          const blocked = item.scan.verdict === "block";
          const name = item.titleZh ?? item.title;
          const desc = item.descriptionZh ?? item.description;
          return (
            <div
              key={item.id}
              className={`rounded-[var(--radius-lg)] p-2.5 ${
                blocked ? "bg-[color-mix(in_srgb,var(--color-bg-tertiary)_60%,transparent)]" : "bg-[var(--color-bg-card)]"
              }`}
            >
              <div className="flex items-center gap-2.5">
                {/* 与插件 / 技能 / 工具集三页共用 InitialIcon（首字 + 稳定色） */}
                <InitialIcon name={name} />
                <div className="min-w-0 flex-1">
                  <span className="truncate text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
                    {name}
                  </span>
                  {/* 描述恒定单行：旧版 line-clamp-2 让每张卡高度不一，
                      网格出现参差的锯齿边。完整信息在详情里。 */}
                  <p className="mt-0.5 truncate text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
                    {desc}
                  </p>
                </div>
              </div>

              {/* 元信息收成一行、11px、faint：三条提示（暂无中文 / 本机执行 /
                  需 N 项配置）此前并排塞在标题行里，把标题挤成一个词。
                  现在它们退到描述下方，用 · 连接，只看不抢。 */}
              {(!item.descriptionZh || item.install.kind === "npm" || item.requiredEnv.length > 0) && (
                <p className="mt-1 pl-[38px] text-[length:var(--text-tiny)] text-[var(--color-text-faint)]">
                  {[
                    !item.descriptionZh ? "暂无中文" : null,
                    item.install.kind === "npm" ? "需在本机执行" : null,
                    item.requiredEnv.length > 0 ? `需 ${item.requiredEnv.length} 项配置` : null,
                  ].filter(Boolean).join(" · ")}
                </p>
              )}

              {blocked && (
                <ul className="mt-2 space-y-0.5">
                  {item.scan.hits
                    .filter((h) => h.rule !== "executes_local_code" && h.rule !== "community_source" && h.rule !== "untraceable_source" && h.rule !== "unpinned_version" && h.rule !== "requires_secrets")
                    .map((h) => (
                      <li key={h.rule} className="text-[length:var(--text-tiny)] text-text-error leading-snug">
                        · {h.detail}
                      </li>
                    ))}
                </ul>
              )}

              <div className="mt-2 flex items-center gap-2">
                <span
                  className={`text-[length:var(--text-tiny)] ${
                    item.scan.verdict === "pass"
                      ? "text-text-success"
                      : item.scan.verdict === "warn"
                        ? "text-text-warning"
                        : "text-text-error"
                  }`}
                >
                  {item.scan.label}
                </span>
                <button
                  onClick={() => startInstall(item)}
                  disabled={blocked || busyId === item.id}
                  title={blocked ? "未通过安全扫描，无法安装" : "添加到已配置"}
                  className="ml-auto h-7 px-2.5 rounded-md text-[length:var(--text-meta)] bg-bg-tertiary text-text-secondary hover:text-text-primary hover:bg-bg-hover disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  {busyId === item.id ? "安装中…" : blocked ? "已拒绝" : "安装"}
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {/* 凭据对话框：MCP 是外部服务的接入层，绝大多数条目要用户自己的凭据。
          值只随 install 请求发往本机后端，写进 ~/.hermix/mcp_servers.json，
          不出网、不进 localStorage。
          弹窗壳收编到 ui/Modal：焦点陷阱 / Esc / 焦点归还 / role=dialog +
          aria-modal 一次性补齐；遮罩由 Modal 的 scrim 提供（此前是裸
          bg-black/30，black 不在令牌重定向清单里，是真裸黑）。 */}
      {envTarget && (
        <Modal
          open
          onClose={() => setEnvTarget(null)}
          width="base"
          scrim
          closeOnBackdrop={false} closeOnEscape={false}
          title={
            <span className="flex items-center gap-2">
              <Terminal className="icon-md text-text-muted" aria-hidden="true" />
              配置「{envTarget.titleZh ?? envTarget.title}」
            </span>
          }
          bodyClassName="overflow-y-auto px-5 pb-5"
          footer={
            <>
              <Button variant="secondary" size="base" onClick={() => setEnvTarget(null)}>
                取消
              </Button>
              <Button
                variant="primary"
                size="base"
                disabled={!envComplete(envTarget)}
                onClick={() => {
                  const t = envTarget;
                  setEnvTarget(null);
                  void doInstall(t, false, envDraft);
                }}
              >
                下一步
              </Button>
            </>
          }
        >
          <p className="text-[length:var(--text-meta)] text-text-secondary leading-snug">
            该 server 需要以下配置才能连上第三方服务。值保存在本机配置文件里，不会上传到任何地方。
          </p>
          <div className="mt-3 space-y-2">
            {envTarget.requiredEnv.map((k) => (
              <label key={k} className="block">
                <span className="text-[length:var(--text-tiny)] text-text-muted font-mono">{k}</span>
                <input
                  type="password"
                  value={envDraft[k] ?? ""}
                  onChange={(e) => setEnvDraft((d) => ({ ...d, [k]: e.target.value }))}
                  placeholder="粘贴你的密钥"
                  className="mt-0.5 w-full h-8 px-2.5 rounded-md bg-bg-tertiary text-[length:var(--text-meta)] outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] font-mono"
                />
              </label>
            ))}
          </div>
        </Modal>
      )}

      {/* 安装前确认：破坏性/有风险动作的唯一模式是 ui/ConfirmDialog
          （宪法 §九）。此前是手写弹窗：无 role=dialog、无 aria-modal、
          无焦点陷阱、无 Esc。 */}
      <ConfirmDialog
        open={confirming !== null}
        scrim
        title={
          <span className="flex items-center gap-2">
            <ShieldAlert className="icon-md text-warning" aria-hidden="true" />
            安装前请确认
          </span>
        }
        confirmLabel="我已知悉，继续安装"
        confirmVariant="primary"
        busy={busyId === confirming?.id}
        body={
          confirming ? (
            <>
              <p className="text-[length:var(--text-ui)] text-text-secondary">
                「{confirming.titleZh ?? confirming.title}」需要确认后才能安装。
              </p>
              {confirming.scan.commandPreview && (
                <pre className="mt-2 px-2.5 py-2 rounded-md bg-bg-tertiary text-[length:var(--text-tiny)] font-mono text-text-secondary whitespace-pre-wrap break-all">
                  {confirming.scan.commandPreview}
                </pre>
              )}
              <ul className="mt-2 space-y-1">
                {confirming.scan.hits.map((h) => (
                  <li key={h.rule} className="text-[length:var(--text-tiny)] text-text-muted leading-snug">
                    · {h.detail}
                  </li>
                ))}
              </ul>
            </>
          ) : null
        }
        onCancel={() => setConfirming(null)}
        onConfirm={() => {
          if (!confirming) return;
          void doInstall(confirming, true, lastEnv);
        }}
      />
    </div>
  );
}
