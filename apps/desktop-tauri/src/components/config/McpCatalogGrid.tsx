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
import {
  Blocks, Database, Globe, Loader2, Package, ShieldAlert, Terminal, Wrench,
} from "lucide-react";
import * as api from "../../api/rest";
import type { CatalogEntry } from "../../api/types";
import { toastError, toastSuccess } from "../../hooks/useToast";

const CATEGORY_ICON: Record<string, React.ReactNode> = {
  办公: <Blocks className="icon-md" />,
  开发: <Wrench className="icon-md" />,
  检索: <Globe className="icon-md" />,
  通讯: <Globe className="icon-md" />,
  数据: <Database className="icon-md" />,
  通用: <Package className="icon-md" />,
  其它: <Terminal className="icon-md" />,
};

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
        正在读取内置目录…
      </div>
    );
  }
  if (failed || !items) {
    return (
      <div className="rounded-lg border border-border-primary bg-bg-card p-4 text-[length:var(--text-meta)] text-text-secondary">
        读取内置目录失败。
        <div className="mt-1 text-text-muted">
          目录是打包在应用内的常量，不依赖网络；取不到通常是后端未就绪。
          重试无效时请查看日志，或先用下方的「手动添加」配置。
        </div>
        <button
          onClick={load}
          className="mt-2 h-7 px-2.5 rounded-md border border-border-secondary hover:bg-bg-hover"
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
          className="ml-auto h-7 px-2.5 rounded-md border border-border-secondary bg-bg-card text-[length:var(--text-meta)] outline-none focus:border-border-strong w-44"
        />
      </div>

      {categories.length > 1 && (
        <div className="flex items-center gap-1.5 flex-wrap">
          <button
            onClick={() => setCategory(null)}
            className={`h-6 px-2 rounded-full text-[length:var(--text-tiny)] border ${
              category === null
                ? "border-border-strong text-text-primary"
                : "border-border-secondary text-text-muted hover:text-text-secondary"
            }`}
          >
            全部
          </button>
          {categories.map((c) => (
            <button
              key={c}
              onClick={() => setCategory(category === c ? null : c)}
              className={`h-6 px-2 rounded-full text-[length:var(--text-tiny)] border ${
                category === c
                  ? "border-border-strong text-text-primary"
                  : "border-border-secondary text-text-muted hover:text-text-secondary"
              }`}
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
              className={`rounded-lg border p-3 ${
                blocked ? "border-border-primary bg-bg-tertiary/40" : "border-border-primary bg-bg-card"
              }`}
            >
              <div className="flex items-start gap-2.5">
                <span className="mt-0.5 shrink-0 text-text-muted">
                  {CATEGORY_ICON[item.category] ?? CATEGORY_ICON["其它"]}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5 flex-wrap">
                    <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">
                      {name}
                    </span>
                    {!item.descriptionZh && (
                      <span className="text-[length:var(--text-tiny)] text-text-faint">（暂无中文）</span>
                    )}
                    {item.install.kind === "npm" && (
                      <span className="text-[length:var(--text-tiny)] text-text-faint">需在本机执行</span>
                    )}
                    {item.requiredEnv.length > 0 && (
                      <span className="text-[length:var(--text-tiny)] text-text-faint">
                        需 {item.requiredEnv.length} 项配置
                      </span>
                    )}
                  </div>
                  <p className="mt-0.5 text-[length:var(--text-meta)] text-text-muted leading-snug line-clamp-2">
                    {desc}
                  </p>
                </div>
              </div>

              {blocked && (
                <ul className="mt-2 space-y-0.5">
                  {item.scan.hits
                    .filter((h) => h.rule !== "executes_local_code" && h.rule !== "community_source" && h.rule !== "untraceable_source" && h.rule !== "unpinned_version" && h.rule !== "requires_secrets")
                    .map((h) => (
                      <li key={h.rule} className="text-[length:var(--text-tiny)] text-error leading-snug">
                        · {h.detail}
                      </li>
                    ))}
                </ul>
              )}

              <div className="mt-2 flex items-center gap-2">
                <span
                  className={`text-[length:var(--text-tiny)] ${
                    item.scan.verdict === "pass"
                      ? "text-success"
                      : item.scan.verdict === "warn"
                        ? "text-warning"
                        : "text-error"
                  }`}
                >
                  {item.scan.label}
                </span>
                <button
                  onClick={() => startInstall(item)}
                  disabled={blocked || busyId === item.id}
                  title={blocked ? "未通过安全扫描，无法安装" : "添加到已配置"}
                  className="ml-auto h-7 px-2.5 rounded-md text-[length:var(--text-meta)] border border-border-secondary text-text-secondary hover:text-text-primary hover:bg-bg-hover disabled:opacity-40 disabled:cursor-not-allowed"
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
          不出网、不进 localStorage。 */}
      {envTarget && (
        <div
          className="fixed inset-0 z-50 grid place-items-center bg-black/30 p-4"
          onClick={() => setEnvTarget(null)}
        >
          <div
            className="w-full max-w-md rounded-xl border border-border-primary bg-bg-elevated p-4 shadow-pop"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center gap-2">
              <Terminal className="icon-md text-text-muted" />
              <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">
                配置「{envTarget.titleZh ?? envTarget.title}」
              </span>
            </div>
            <p className="mt-2 text-[length:var(--text-meta)] text-text-secondary leading-snug">
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
                    className="mt-0.5 w-full h-8 px-2.5 rounded-md border border-border-secondary bg-bg-card text-[length:var(--text-meta)] outline-none focus:border-border-strong font-mono"
                  />
                </label>
              ))}
            </div>
            <div className="mt-3 flex items-center gap-2 justify-end">
              <button
                onClick={() => setEnvTarget(null)}
                className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] border border-border-secondary text-text-secondary hover:bg-bg-hover"
              >
                取消
              </button>
              <button
                onClick={() => {
                  const t = envTarget;
                  setEnvTarget(null);
                  void doInstall(t, false, envDraft);
                }}
                disabled={!envComplete(envTarget)}
                className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] disabled:opacity-40"
              >
                下一步
              </button>
            </div>
          </div>
        </div>
      )}

      {confirming && (
        <div
          className="fixed inset-0 z-50 grid place-items-center bg-black/30 p-4"
          onClick={() => setConfirming(null)}
        >
          <div
            className="w-full max-w-md rounded-xl border border-border-primary bg-bg-elevated p-4 shadow-pop"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center gap-2">
              <ShieldAlert className="icon-md text-warning" />
              <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">
                安装前请确认
              </span>
            </div>
            <p className="mt-2 text-[length:var(--text-meta)] text-text-secondary">
              「{confirming.titleZh ?? confirming.title}」需要确认后才能安装。
            </p>
            {confirming.scan.commandPreview && (
              <pre className="mt-2 px-2.5 py-2 rounded-md bg-bg-tertiary border border-border-subtle text-[length:var(--text-tiny)] font-mono text-text-secondary whitespace-pre-wrap break-all">
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
            <div className="mt-3 flex items-center gap-2 justify-end">
              <button
                onClick={() => setConfirming(null)}
                className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] border border-border-secondary text-text-secondary hover:bg-bg-hover"
              >
                取消
              </button>
              <button
                onClick={() => void doInstall(confirming, true, lastEnv)}
                disabled={busyId === confirming.id}
                className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] disabled:opacity-50"
              >
                {busyId === confirming.id ? "安装中…" : "我已知悉，继续安装"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
