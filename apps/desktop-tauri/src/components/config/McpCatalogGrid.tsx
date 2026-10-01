// MCP 社区目录：可浏览 + 一键安装。
//
// 设计对齐参考项目的市场面板：分类 → 卡片（图标 + 名称 + 一句描述 + 安装按钮），
// 但**把安全结论摆在安装按钮旁边**——这是我们和普通市场最大的区别：来自社区的
// 东西要执行在用户机器上，"能不能装"必须是看得见的判断，不是藏进日志。
//
// 三档（后端 mcp_scan 判定，前端只负责如实呈现）：
// - pass  可直接使用      → 直接装
// - warn  需确认后使用    → 展示将执行的命令 + 逐条风险，用户确认后装
// - block 已拒绝          → 按钮禁用，列出命中的规则
import { useEffect, useMemo, useState } from "react";
import {
  Blocks, Database, Globe, Loader2, Package, RefreshCw, ShieldAlert, Terminal, Wrench,
} from "lucide-react";
import * as api from "../../api/rest";
import type { CatalogEntry } from "../../api/types";
import { CatalogSourceBar } from "./CatalogSourceBar";
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

export function McpCatalogGrid({ onInstalled }: { onInstalled: () => void }) {
  const [items, setItems] = useState<CatalogEntry[] | null>(null);
  const [stale, setStale] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [failed, setFailed] = useState(false);
  const [category, setCategory] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  /** 需要二次确认的条目（把命令与风险摊开给用户看） */
  const [confirming, setConfirming] = useState<CatalogEntry | null>(null);

  const load = (refresh: boolean) => {
    if (refresh) setRefreshing(true); else setLoading(true);
    api
      .getMcpCatalog(refresh)
      .then((res) => {
        if (!res) { setFailed(true); return; }
        setItems(res.items);
        setStale(res.staleReason ?? null);
      })
      .catch(() => setFailed(true))
      .finally(() => { setLoading(false); setRefreshing(false); });
  };

  useEffect(() => { load(false); }, []);

  // 换源后必须重拉目录：缓存键没有按源隔离，旧源数据会误导用户。
  const reloadAfterSourceChange = () => load(true);

  const installedIds = useMemo(() => new Set<string>(), []);
  void installedIds; // 已安装判定交给父组件（它有列表），这里只管目录

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

  const doInstall = async (item: CatalogEntry, acknowledge: boolean) => {
    setBusyId(item.id);
    try {
      const res = await api.installMcpFromCatalog(item.id, {}, acknowledge);
      if (res.ok) {
        toastSuccess(`已添加「${item.titleZh ?? item.title}」`);
        setConfirming(null);
        onInstalled();
      } else if (res.needAck && !acknowledge) {
        setConfirming(item);
      } else {
        toastError(res.error);
      }
    } finally {
      setBusyId(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-[length:var(--text-meta)] text-text-muted py-6 justify-center">
        <Loader2 className="icon-md animate-spin" />
        正在拉取社区目录…
      </div>
    );
  }
  if (failed) {
    return (
      <div className="rounded-lg border border-border-primary bg-bg-card p-4 text-[length:var(--text-meta)] text-text-secondary">
        拉取目录失败：连不上 MCP 官方 registry。
        <div className="mt-1 text-text-muted">
          先检查网络。官方 registry 在国内一般可以直连；若你的网络策略（公司网 / 校园网）拦截了它，
          可开启系统代理的「环境变量」开关后重试，或先在下方「手动添加」里配置。
        </div>
        <button
          onClick={() => load(true)}
          className="mt-2 h-7 px-2.5 rounded-md flex items-center gap-1 border border-border-secondary hover:bg-bg-hover"
        >
          <RefreshCw className="icon-sm" /> 重试
        </button>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <CatalogSourceBar onChanged={reloadAfterSourceChange} />
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">社区目录</span>
        <span className="text-[length:var(--text-tiny)] text-text-faint">{visible.length} 个</span>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索名称或描述"
          className="ml-auto h-7 px-2.5 rounded-md border border-border-secondary bg-bg-card text-[length:var(--text-meta)] outline-none focus:border-border-strong w-44"
        />
        <button
          onClick={() => load(true)}
          disabled={refreshing}
          className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary disabled:opacity-50"
          title="从官方 registry 重新拉取"
        >
          <RefreshCw className={`icon-sm ${refreshing ? "animate-spin" : ""}`} />
          刷新
        </button>
      </div>

      {stale && (
        <div className="flex items-start gap-2 text-[length:var(--text-tiny)] text-text-muted">
          <ShieldAlert className="icon-sm mt-0.5 shrink-0 text-warning" />
          {stale}
        </div>
      )}

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
                      <span className="text-[length:var(--text-tiny)] text-text-faint">（社区暂无中文）</span>
                    )}
                    {item.install.kind === "npm" && (
                      <span className="text-[length:var(--text-tiny)] text-text-faint">需在本机执行</span>
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
                  onClick={() => void doInstall(item, false)}
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
                onClick={() => void doInstall(confirming, true)}
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
