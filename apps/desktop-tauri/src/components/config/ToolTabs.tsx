import { ToolCardIcon } from "./ToolCardIcon";
import { MgmtTag } from "./MgmtTag";
import { ToggleSwitch } from "../ToggleSwitch";
import { MarkdownBody } from "../MarkdownBody";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Search,
  Plus,
  Sparkles,
  RefreshCw,
  Boxes,
  X,
  Network,
  Trash2,
  ArrowUpRight,
  Wand2,
} from "lucide-react";
import * as api from "../../api";
import type { PluginSettings, SkillInfo, ToolsetInfo } from "../../api";
import type { LucideIcon } from "lucide-react";
import { McpCatalogGrid } from "./McpCatalogGrid";
import { ConfirmDialog } from "../ui";
import {
  BUILTIN_PLUGINS, DEFAULT_PLUGIN_SETTINGS, CATEGORY_LABELS,
  parseMcpJson, pickIconByName,
} from "./toolTabsData";

// Plugin toggles map to the backend's DashboardPluginSettings (5 hard slots).
// The icon is purely cosmetic — the source of truth is the boolean on the server.
// （R2-4：常量表已抽至 ./toolTabsData.ts）

// ── Plugins tab: built-in toggles + discovered manifests ────────────

export function PluginsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [settings, setSettings] = useState<PluginSettings>(DEFAULT_PLUGIN_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [savingId, setSavingId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setSettings(await api.getPluginSettings());
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const togglePlugin = useCallback(
    async (plugin: (typeof BUILTIN_PLUGINS)[number], enabled: boolean) => {
      setSavingId(plugin.id);
      try {
        const next = { ...settings, [plugin.key]: enabled };
        setSettings(next);
        const saved = await api.updatePluginSettings(next);
        setSettings(saved);
      } finally {
        setSavingId(null);
      }
    },
    [settings],
  );

  const q = query.trim().toLowerCase();
  const filteredBuiltins = q
    ? BUILTIN_PLUGINS.filter((p) => `${p.name} ${p.description} ${p.category}`.toLowerCase().includes(q))
    : BUILTIN_PLUGINS;

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

      {!loading && BUILTIN_PLUGINS.length > 0 && (
        <SummaryBar
          total={BUILTIN_PLUGINS.length}
          enabled={BUILTIN_PLUGINS.filter((p) => settings[p.key]).length}
          enabledLabel="已启用"
          totalLabel="个插件"
        />
      )}

      <Section title="内置工具" hint="由后端持久化保存于 dashboard_plugins.json">
        <Grid empty={!loading && filteredBuiltins.length === 0} emptyText="没有匹配的内置工具。">
          {filteredBuiltins.map((plugin) => {
            const enabled = Boolean(settings[plugin.key]);
            return (
              <PluginCard
                key={plugin.id}
                icon={<plugin.icon className="icon-lg" />}
                name={plugin.name}
                description={plugin.description}
                badge={plugin.category}
                enabled={enabled}
                saving={savingId === plugin.id}
                disabled={loading}
                rightIcon={<ArrowUpRight className="icon-sm" />}
                onToggle={() => togglePlugin(plugin, !enabled)}
              />
            );
          })}
        </Grid>
      </Section>

    </>
  );
}

// ── Skills tab: GET /api/skills + toggle ────────────────────────────

export function SkillsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  /** 分类 chip 筛选。null = 全部。搜索框管关键词，这个管分组。 */
  const [catFilter, setCatFilter] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await api.getSkills();
      setSkills(list);
    } catch (e) {
      setError(e instanceof Error ? e.message : "加载技能失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const q = query.trim().toLowerCase();
  // 关键词与分类 chip 是两个独立维度：先按词筛，再按分类筛。
  // chip 上的计数取**分类筛之前**的结果，否则选中一个分类后其它 chip 全部归零。
  const byQuery = q
    ? skills.filter((s) =>
        // 带上 display_name：界面显示的是中文名，用户按看到的字搜才搜得到
        `${s.name} ${s.display_name ?? ""} ${s.description} ${s.category}`
          .toLowerCase()
          .includes(q),
      )
    : skills;
  const filtered = catFilter
    ? byQuery.filter((s) => (s.category || "uncategorized") === catFilter)
    : byQuery;

  // Group filtered skills by category
  const grouped = useMemo(() => {
    const map: Record<string, SkillInfo[]> = {};
    for (const s of filtered) {
      const key = s.category || "uncategorized";
      if (!map[key]) map[key] = [];
      map[key].push(s);
    }
    return map;
  }, [filtered]);

  const catLabel = useCallback((key: string) => CATEGORY_LABELS[key] || key, []);

  // chip 计数基于 byQuery（未按分类筛），否则选中任一分类后其余 chip 全部归零，
  // 用户会以为其它分类没有技能。
  const countsByCat = useMemo(() => {
    const m: Record<string, number> = {};
    for (const s of byQuery) {
      const k = s.category || "uncategorized";
      m[k] = (m[k] ?? 0) + 1;
    }
    return m;
  }, [byQuery]);

  // 分组顺序取自 countsByCat（未按分类筛）—— 否则选中一个分类后，
  // chip 行会只剩一个 chip，用户就没法切换到别的分类了。
  const categoryOrder = useMemo(() => {
    const priority = ["software-development", "devops", "productivity", "finance", "lifestyle"];
    const keys = Object.keys(countsByCat);
    return priority.filter((k) => keys.includes(k)).concat(
      keys.filter((k) => !priority.includes(k)).sort(),
    );
  }, [countsByCat]);

  const enabledCount = useMemo(() => skills.filter((s) => s.enabled).length, [skills]);

  return (
    <>
      <div className="flex items-center justify-between gap-2 mb-3">
        <div className="flex-1">
          <SearchBar query={query} onQuery={onQuery} />
        </div>
        <button
          onClick={refresh}
          disabled={loading}
          className="btn-secondary btn-sm flex-shrink-0"
          title="刷新技能列表"
          aria-label="刷新技能列表">
          <RefreshCw className={`icon-md ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* 分类 chip：搜索框只能筛关键词，分组再多时仍需一键跨组跳。
          无描边药丸——未选中=浅灰实心，选中=主色实心。 */}
      {!loading && categoryOrder.length > 1 && (
        <div className="flex items-center gap-1.5 flex-wrap mb-3">
          <button
            onClick={() => setCatFilter(null)}
            aria-pressed={catFilter === null}
            className={`h-6 px-2 rounded-full text-[length:var(--text-tiny)] font-semibold transition-colors ${
              catFilter === null
                ? "text-[var(--color-btn-primary-text)]"
                : "bg-[color-mix(in_srgb,var(--color-text-primary)_5%,transparent)] text-text-muted hover:text-text-primary"
            }`}
            style={catFilter === null ? { background: "var(--color-btn-primary-bg)" } : undefined}
          >
            全部 <span className="ml-0.5 tabular-nums opacity-65">{byQuery.length}</span>
          </button>
          {categoryOrder.map((c) => (
            <button
              key={c}
              onClick={() => setCatFilter(catFilter === c ? null : c)}
              aria-pressed={catFilter === c}
              className={`h-6 px-2 rounded-full text-[length:var(--text-tiny)] font-semibold transition-colors ${
                catFilter === c
                  ? "text-[var(--color-btn-primary-text)]"
                  : "bg-[color-mix(in_srgb,var(--color-text-primary)_5%,transparent)] text-text-muted hover:text-text-primary"
              }`}
              style={catFilter === c ? { background: "var(--color-btn-primary-bg)" } : undefined}
            >
              {catLabel(c)} <span className="ml-0.5 tabular-nums opacity-65">{countsByCat[c] ?? 0}</span>
            </button>
          ))}
        </div>
      )}

      {!loading && skills.length > 0 && (
        <SummaryBar
          total={skills.length}
          enabled={enabledCount}
          enabledLabel="已启用"
          totalLabel="个技能"
        />      )}

      {error && (
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-error">
          {error}
        </div>
      )}

      {loading ? (
        <SkeletonGrid count={4} />
      ) : filtered.length === 0 ? (
        <EmptyHint
          icon={Sparkles}
          text={
            skills.length === 0
              ? "后端还没有加载任何技能。把 SKILL.md 放到 skills 目录下后会自动出现。"
              : "没有匹配的技能。"
          }
        />
      ) : (
        categoryOrder.map((cat) => (
          <div key={cat} className="mb-5">
            <div className="flex items-center gap-2 mb-2 px-1">
              <h4 className="text-[length:var(--text-ui)] font-semibold text-text-primary">
                {catLabel(cat)}
              </h4>
              <MgmtTag tone="accent">{grouped[cat].length}</MgmtTag>
            </div>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-2">
              {grouped[cat].map((s) => (
                <SkillCard key={s.name} skill={s} catLabel={catLabel} onToggle={(name) => {
                  setSkills((prev) => prev.map((p) => p.name === name ? { ...p, enabled: !p.enabled } : p));
                }} />
              ))}
            </div>
          </div>
        ))
      )}

      <p className="text-[length:var(--text-meta)] text-text-muted pb-6">技能数据来自 hermix-skills 仓库，禁用会从文件系统中删除该技能。</p>
    </>
  );
}

// ── SkillCard: self-contained expand + toggle ───────────────────

function SkillCard({ skill, catLabel, onToggle }: {
  skill: SkillInfo;
  catLabel: (key: string) => string;
  onToggle?: (name: string) => void;
}) {
  const [showModal, setShowModal] = useState(false);
  const [content, setContent] = useState<string | null>(null);
  const [loadingContent, setLoadingContent] = useState(false);
  const [toggling, setToggling] = useState(false);
  const [pendingDisable, setPendingDisable] = useState(false);

  const openContent = useCallback(async () => {
    setShowModal(true);
    if (content === null) {
      setLoadingContent(true);
      try {
        const c = await api.getSkillContent(skill.name);
        if (c) setContent(c.content);
      } catch {
        // stay null
      } finally {
        setLoadingContent(false);
      }
    }
  }, [content, skill.name]);

  const closeContent = useCallback(() => setShowModal(false), []);

  const doToggle = useCallback(async () => {
    setToggling(true);
    try {
      await api.toggleSkill(skill.name, !skill.enabled);
      onToggle?.(skill.name);
    } catch {
      // silently fail
    } finally {
      setToggling(false);
    }
  }, [skill, onToggle]);

  const requestToggle = useCallback(() => {
    if (skill.enabled) {
      setPendingDisable(true);
    } else {
      void doToggle();
    }
  }, [skill.enabled, doToggle]);

  const confirmDisable = useCallback(() => {
    setPendingDisable(false);
    void doToggle();
  }, [doToggle]);

  return (
    <>
      <div className="group rounded-xl bg-bg-card hover:bg-bg-card-hover transition-colors">
        <div className="flex items-center gap-3 px-3 py-2.5">
          <ToolCardIcon onClick={openContent} title="查看技能内容" aria-label="查看技能内容">
            <Wand2 className="icon-md" />
          </ToolCardIcon>
          <div className="min-w-0 flex-1 cursor-pointer" onClick={openContent}>
            <div className="flex items-center gap-2">
              <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">
                {skill.display_name || skill.name}
              </span>
              <MgmtTag tone="neutral" className="shrink-0">
                {catLabel(skill.category) || "skill"}
              </MgmtTag>
            </div>
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">
              {skill.description || "（无描述）"}
            </p>
          </div>
          <ArrowUpRight className="icon-sm text-text-faint shrink-0" />
          <ToggleSwitch enabled={skill.enabled} onToggle={requestToggle} disabled={toggling} offLabel={`停用 ${skill.name}`} onLabel={`启用 ${skill.name}`} className="shrink-0" />
        </div>
      </div>

      {showModal && (
        <SkillContentModal
          skill={skill}
          catLabel={catLabel}
          content={content}
          loading={loadingContent}
          onClose={closeContent}
        />
      )}

      {pendingDisable && (
        <ConfirmDialog
          open
          title="禁用技能"
          body={
            <>
              即将从文件系统中删除技能 <span className="font-medium text-text-primary">{skill.name}</span>。
              该操作不可撤销，需要重新下载才能再次启用。
            </>
          }
          confirmLabel="禁用并删除"
          busy={toggling}
          onCancel={() => setPendingDisable(false)}
          onConfirm={confirmDisable}
        />
      )}
    </>
  );
}

// ── SkillContentModal: popup for reading skill markdown ─────────

function SkillContentModal({ skill, catLabel, content, loading, onClose }: {
  skill: SkillInfo;
  catLabel: (key: string) => string;
  content: string | null;
  loading: boolean;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card modal-card-lg"
        onClick={(e) => e.stopPropagation()}
        style={{ maxHeight: "80vh", display: "flex", flexDirection: "column" }}
      >
        {/* 头部自带一层底色，与正文分开 —— 层次靠底色差，不靠描边。
            标题与描述都限高：此前两者都不限，一段长描述会把整个头撑成十几行、
            把关闭按钮顶到视线之外（"过长的标题会改变显示"就是这个）。 */}
        <div
          className="modal-header"
          style={{ flexShrink: 0, background: "var(--color-bg-secondary)" }}
        >
          {/* 图标与列表卡共用 ToolCardIcon —— 此前这里用的是 .modal-icon-info
              的灰色块，与卡片里的主色图标对不上，"图标没有颜色"。那个组件存在的
              全部意义就是让四页图标必然一致，弹窗没走它就又漂了一处。 */}
          <ToolCardIcon>
            <Sparkles className="icon-lg" />
          </ToolCardIcon>
          <div className="modal-title-block">
            <h3 className="modal-title truncate" title={skill.display_name || skill.name}>
              {skill.display_name || skill.name}
            </h3>
            <p className="modal-subtitle line-clamp-2">
              {catLabel(skill.category)}
              {skill.description ? ` · ${skill.description}` : ""}
            </p>
          </div>
          <button onClick={onClose} className="modal-close shrink-0" aria-label="关闭">
            <X className="icon-lg" />
          </button>
        </div>
        {/* 正文走 MarkdownBody：SKILL.md 本来就是 markdown，用 <pre> 平铺等于
            把标题层级、有序列表、行内代码全部抹平——「界面没有层次」的一半在
            这里，另一半在头部那层底色。 */}
        <div className="flex-1 overflow-auto bg-bg-elevated px-5 py-4">
          {loading ? (
            <div className="text-[length:var(--text-meta)] text-text-muted animate-pulse">
              加载中…
            </div>
          ) : content ? (
            <MarkdownBody content={content} />
          ) : (
            <div className="text-[length:var(--text-meta)] text-text-muted">
              <p>无法加载技能内容</p>
              <p className="mt-2 opacity-60 break-all">
                存放路径：~/.hermix/skills/{skill.category}/{skill.name}/SKILL.md
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// R2-2（2026-10-16）：私有 ConfirmDialog 已删除，统一走 ui/ConfirmDialog（busy 态已收编）。
// R2-4：常量表与 MCP JSON 解析器已抽到 ./toolTabsData.ts。

// ── Connectors tab: user-configured MCP servers ──────────────────────

export function ConnectorsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [servers, setServers] = useState<api.McpServerEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [showAddForm, setShowAddForm] = useState(false);
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  const [addMode, setAddMode] = useState<"paste" | "manual">("paste");
  const [pasteJson, setPasteJson] = useState("");
  const [newServer, setNewServer] = useState({
    name: "",
    transport: "stdio" as api.McpTransport,
    command: "",
    args: "",
    url: "",
    token: "",
    env: "",
    description: "",
  });

  useEffect(() => {
    let cancelled = false;
    api.getMcpServers().then((list) => { if (!cancelled) { setServers(list); setLoading(false); } });
    return () => { cancelled = true; };
  }, []);

  const toggle = async (id: string, enabled: boolean) => {
    const updated = await api.updateMcpServer(id, { enabled });
    if (updated) setServers((prev) => prev.map((s) => (s.id === id ? updated : s)));
  };
  const remove = async (id: string) => {
    await api.deleteMcpServer(id);
    setServers((prev) => prev.filter((s) => s.id !== id));
  };
  const handleAdd = async () => {
    if (!newServer.name.trim()) { setAddError("名称不能为空"); return; }
    if (newServer.transport === "stdio" && !newServer.command.trim()) { setAddError("stdio 模式需要填写 command"); return; }
    if (newServer.transport === "http" && !newServer.url.trim()) { setAddError("http 模式需要填写 URL"); return; }
    setAdding(true);
    setAddError(null);
    try {
      const spec: Omit<api.McpServerEntry, "id" | "createdAt"> = {
        name: newServer.name.trim(),
        transport: newServer.transport,
        command: newServer.transport === "stdio" ? newServer.command.trim() : undefined,
        args: newServer.transport === "stdio" && newServer.args.trim() ? newServer.args.trim().split(/\s+/) : undefined,
        url: newServer.transport === "http" ? newServer.url.trim() : undefined,
        token: newServer.transport === "http" && newServer.token.trim() ? newServer.token.trim() : undefined,
        env: newServer.env.trim() ? Object.fromEntries(newServer.env.trim().split("\n").filter(Boolean).map((line) => { const idx = line.indexOf("="); return idx > 0 ? [line.slice(0, idx).trim(), line.slice(idx + 1).trim()] : null; }).filter(Boolean) as [string, string][]) : undefined,
        enabled: true,
        description: newServer.description.trim() || undefined,
      };
      const created = await api.addMcpServer(spec);
      setServers((prev) => [...prev, created]);
      setNewServer({ name: "", transport: "stdio", command: "", args: "", url: "", token: "", env: "", description: "" });
      setShowAddForm(false);
    } catch (e) {
      setAddError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  };

  const handleImportPaste = async () => {
    if (!pasteJson.trim()) { setAddError("请先粘贴配置"); return; }
    setAdding(true);
    setAddError(null);
    try {
      const specs = parseMcpJson(pasteJson);
      const created: api.McpServerEntry[] = [];
      for (const spec of specs) {
        created.push(await api.addMcpServer(spec));
      }
      setServers((prev) => [...prev, ...created]);
      setPasteJson("");
      setShowAddForm(false);
    } catch (e) {
      setAddError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  };

  const q = query.trim().toLowerCase();
  const filtered = q
    ? servers.filter((s) =>
        `${s.name} ${s.description ?? ""} ${s.command ?? ""} ${s.url ?? ""}`.toLowerCase().includes(q),
      )
    : servers;

  const enabledCount = useMemo(() => servers.filter((s) => s.enabled).length, [servers]);

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

      {!loading && servers.length > 0 && (
        <SummaryBar
          total={servers.length}
          enabled={enabledCount}
          enabledLabel="已启用"
          totalLabel="个服务"
        />
      )}

      <div className="flex items-center justify-between mb-3">
        <span className="text-[length:var(--text-meta)] text-text-muted">
          已配置的 MCP 服务：通过 stdio 启动本地子进程，或连接 HTTP 远程端点。
        </span>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowAddForm((v) => !v)}
            className="flex items-center gap-1 px-2 py-1 rounded-md text-[length:var(--text-meta)] text-accent hover:bg-bg-tertiary"
            title="添加自定义 MCP 服务"
          >
            <Plus className="icon-sm" /> 添加 MCP
          </button>
        </div>
      </div>

      {showAddForm && (
        <div className="rounded-xl bg-bg-card p-4 mb-3 space-y-3">
          <div className="text-sm font-medium text-text-primary">添加 MCP 服务</div>
          {addError && (
            <div className="text-[length:var(--text-meta)] text-error bg-error/10 rounded px-2 py-1">{addError}</div>
          )}

          <div className="flex gap-2 text-[length:var(--text-meta)]">
            <button
              onClick={() => setAddMode("paste")}
              className={addMode === "paste" ? "px-2 py-1 rounded-md bg-bg-tertiary text-text-primary" : "px-2 py-1 rounded-md text-text-muted hover:text-text-primary"}
            >粘贴导入</button>
            <button
              onClick={() => setAddMode("manual")}
              className={addMode === "manual" ? "px-2 py-1 rounded-md bg-bg-tertiary text-text-primary" : "px-2 py-1 rounded-md text-text-muted hover:text-text-primary"}
            >手动填写（高级）</button>
          </div>

          {addMode === "paste" ? (
            <>
              <div>
                <label className="text-[length:var(--text-meta)] text-text-muted">粘贴配置（Claude Desktop / Cursor 风格 mcpServers JSON）</label>
                <textarea
                  value={pasteJson}
                  onChange={(e) => setPasteJson(e.target.value)}
                  rows={7}
                  placeholder={'{"mcpServers":{"my-fs":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/tmp"]},"remote":{"url":"https://example.com/mcp"}}}'}
                  className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary resize-none font-mono"
                />
                <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">支持 mcpServers 对象或单条 server 配置，自动识别 stdio / http。</p>
              </div>
              <div className="flex items-center gap-2">
                <button onClick={handleImportPaste} disabled={adding} className="px-3 py-1.5 rounded-lg bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] text-[length:var(--text-meta)] font-medium hover:opacity-90 disabled:opacity-50">导入</button>
                <button onClick={() => { setShowAddForm(false); setAddError(null); }} className="px-3 py-1.5 rounded-lg text-[length:var(--text-meta)] text-text-muted hover:text-text-primary hover:bg-bg-tertiary">取消</button>
              </div>
            </>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="text-[length:var(--text-meta)] text-text-muted">名称</label>
                  <input value={newServer.name} onChange={(e) => setNewServer((p) => ({ ...p, name: e.target.value }))} placeholder="my-mcp-server" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary" />
                </div>
                <div>
                  <label className="text-[length:var(--text-meta)] text-text-muted">传输方式</label>
                  <select value={newServer.transport} onChange={(e) => setNewServer((p) => ({ ...p, transport: e.target.value as api.McpTransport }))} className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary">
                    <option value="stdio">stdio</option>
                    <option value="http">http</option>
                  </select>
                </div>
                {newServer.transport === "stdio" ? (
                  <>
                    <div>
                      <label className="text-[length:var(--text-meta)] text-text-muted">Command</label>
                      <input value={newServer.command} onChange={(e) => setNewServer((p) => ({ ...p, command: e.target.value }))} placeholder="npx" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary" />
                    </div>
                    <div>
                      <label className="text-[length:var(--text-meta)] text-text-muted">Args （空格分隔）</label>
                      <input value={newServer.args} onChange={(e) => setNewServer((p) => ({ ...p, args: e.target.value }))} placeholder="-y @modelcontextprotocol/server-filesystem /tmp" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary" />
                    </div>
                  </>
                ) : (
                  <div className="col-span-2">
                    <label className="text-[length:var(--text-meta)] text-text-muted">URL</label>
                    <input value={newServer.url} onChange={(e) => setNewServer((p) => ({ ...p, url: e.target.value }))} placeholder="https://example.com/mcp" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary" />
                  </div>
                )}
                {newServer.transport === "http" && (
                  <div className="col-span-2">
                    <label className="text-[length:var(--text-meta)] text-text-muted">Bearer Token （可选，远程服务鉴权）</label>
                    <input value={newServer.token} onChange={(e) => setNewServer((p) => ({ ...p, token: e.target.value }))} placeholder="homk_…" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary" />
                  </div>
                )}
                <div className="col-span-2">
                  <label className="text-[length:var(--text-meta)] text-text-muted">环境变量 (每行 KEY=value)</label>
                  <textarea value={newServer.env} onChange={(e) => setNewServer((p) => ({ ...p, env: e.target.value }))} placeholder={"API_KEY=xxx"} rows={2} className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary resize-none" />
                </div>
                <div className="col-span-2">
                  <label className="text-[length:var(--text-meta)] text-text-muted">描述 （可选）</label>
                  <input value={newServer.description} onChange={(e) => setNewServer((p) => ({ ...p, description: e.target.value }))} placeholder="自定义文件系统 MCP" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-[length:var(--text-meta)] text-text-primary" />
                </div>
              </div>
              <div className="flex items-center gap-2">
                <button onClick={handleAdd} disabled={adding} className="px-3 py-1.5 rounded-lg bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] text-[length:var(--text-meta)] font-medium hover:opacity-90 disabled:opacity-50">
                  {adding ? "添加中..." : "确认添加"}
                </button>
                <button onClick={() => { setShowAddForm(false); setAddError(null); }} className="px-3 py-1.5 rounded-lg text-[length:var(--text-meta)] text-text-muted hover:text-text-primary hover:bg-bg-tertiary">
                  取消
                </button>
              </div>
            </>
          )}
        </div>
      )}

      {loading ? (
        <SkeletonGrid count={3} />
      ) : filtered.length === 0 ? (
        <EmptyHint
          icon={Network}
          text={
            servers.length === 0
              ? "你还没有添加任何 MCP 服务，粘贴配置即可添加。"
              : "没有匹配的连接器。"
          }
        />
      ) : (
        <div className="space-y-1.5">
          {filtered.map((s) => (
            <ConnectorRow
              key={s.id}
              server={s}
              onToggle={(enabled) => toggle(s.id, enabled)}
              onRemove={() => remove(s.id)}
            />
          ))}
        </div>
      )}

      <p className="text-[length:var(--text-meta)] text-text-muted mt-4 pb-5">
        配置需要 {`npx`} / {`uvx`} 等运行时可用。密钥请填入 env 字段。
      </p>

      {/* 内置目录：先逛清单，再决定装不装。放在已配置列表之后——
          自己的 server 是主角，逛目录是第二幕。 */}
      <McpCatalogGrid onInstalled={() => { void api.getMcpServers().then(setServers); }} />
    </>
  );
}

function ConnectorRow({
  server,
  onToggle,
  onRemove,
}: {
  server: api.McpServerEntry;
  onToggle: (enabled: boolean) => void;
  onRemove: () => void;
}) {
  const Icon = pickIconByName(server.name);
  return (
    <div className="h-enter group flex items-start gap-3 px-3 py-2.5 rounded-xl bg-bg-card hover:bg-bg-card-hover transition-colors">
      <ToolCardIcon>
        <Icon className="icon-lg" />
      </ToolCardIcon>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">{server.name}</span>
          <MgmtTag tone="neutral">{server.transport}</MgmtTag>
          {server.enabled && (
            <MgmtTag tone="accent">启用中</MgmtTag>
          )}
        </div>
        {server.description && (
          <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">
            {server.description}
          </p>
        )}
        <p className="text-[length:var(--text-meta)] font-mono text-text-faint mt-1 truncate">
          {server.transport === "stdio"
            ? `${server.command ?? "?"} ${(server.args ?? []).join(" ")}`
            : server.url ?? "(no url)"}
        </p>
      </div>
      <div className="flex items-center gap-1.5 shrink-0">
        <ToggleSwitch enabled={server.enabled} onToggle={() => onToggle(!server.enabled)} offLabel="停用" onLabel="启用" />
        <button
          onClick={onRemove}
          className="h-7 w-7 rounded-md text-text-muted hover:text-error hover:bg-error/10 flex items-center justify-center"
          title="删除"
          aria-label="删除">
          <Trash2 className="icon-md" />
        </button>
      </div>
    </div>
  );
}

// ── Toolsets tab: backend tool registry (a.k.a. toolset groups) ──────

export function ToolsetsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [toolsets, setToolsets] = useState<ToolsetInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Local mirror of which toolsets are enabled, seeded from the
  // server's first response. We keep this as a `Set` for O(1)
  // membership checks; the server treats the canonical list as
  // the absolute state, so we always replace the whole set after
  // a successful PUT instead of merging.
  const [enabled, setEnabled] = useState<Set<string>>(() => new Set());
  // Per-toolset saving state. A toggle click is *optimistic* —
  // we flip the local `enabled` set immediately, mark the
  // toolset as `saving`, then on the PUT response we either
  // confirm (clear `saving`) or roll back (flip `enabled` back
  // and surface an error). The user can fire several toggles in
  // quick succession; the latest PUT's response wins.
  const [saving, setSaving] = useState<Set<string>>(() => new Set());
  // Warnings that are not fatal — e.g. the server reports
  // toolset names it did not recognise. We show them as a
  // dismissable banner above the grid.
  const [warning, setWarning] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .getToolsets()
      .then((list) => {
        if (cancelled) return;
        setToolsets(list);
        // Seed the local enabled set from the server's view of
        // reality. Toolsets that the server reports as
        // `enabled: false` (because the persisted
        // `enabled_toolsets.json` excluded them) start out off
        // in the UI as well.
        setEnabled(new Set(list.filter((t) => t.enabled).map((t) => t.name)));
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : "加载工具集失败");
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Optimistic toggle handler. Fires the PUT, then on success
  // reconciles the local set to whatever the server reports as
  // the canonical post-toggle state. On failure we roll the
  // local flip back and surface the error. We do *not* abort
  // earlier in-flight PUTs — the server is the source of
  // truth, and a late response simply overwrites the local
  // set with whatever the server has by then.
  const handleToggle = useCallback(
    async (name: string, next: boolean) => {
      const previous = enabled.has(name);
      if (previous === next) return; // no-op click

      // Optimistic flip + mark saving
      setEnabled((prev) => {
        const out = new Set(prev);
        if (next) out.add(name);
        else out.delete(name);
        return out;
      });
      setSaving((prev) => new Set(prev).add(name));
      setError(null);
      setWarning(null);

      // Build the next canonical set: start from current
      // local, apply this flip, then submit.
      const nextSet = new Set(enabled);
      if (next) nextSet.add(name);
      else nextSet.delete(name);

      try {
        const res = await api.updateEnabledToolsets(Array.from(nextSet));
        // Reconcile to the server's canonical answer. This
        // also picks up server-side changes to other toolsets
        // (shouldn't happen, but defends against drift).
        setEnabled(new Set(res.enabled));
        if (res.unknown.length > 0) {
          setWarning(
            `以下工具集名称未被识别: ${res.unknown.join(", ")}`,
          );
        }
      } catch (e) {
        // Roll back the optimistic flip
        setEnabled((prev) => {
          const out = new Set(prev);
          if (next) out.delete(name);
          else out.add(name);
          return out;
        });
        setError(
          e instanceof Error
            ? `保存失败: ${e.message}`
            : "保存失败：未知错误",
        );
      } finally {
        setSaving((prev) => {
          const out = new Set(prev);
          out.delete(name);
          return out;
        });
      }
    },
    [enabled],
  );

  const q = query.trim().toLowerCase();
  const filtered = q
    ? toolsets.filter((t) =>
        `${t.name} ${t.label} ${t.description} ${t.tools.join(" ")}`.toLowerCase().includes(q),
      )
    : toolsets;

  const enabledCount = useMemo(() => toolsets.filter((t) => t.enabled).length, [toolsets]);

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

      {!loading && toolsets.length > 0 && (
        <SummaryBar
          total={toolsets.length}
          enabled={enabledCount}
          enabledLabel="已启用"
          totalLabel="个工具集"
        />
      )}

      {error && (
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-error flex items-start justify-between gap-2">
          <span>{error}</span>
          <button
            type="button"
            onClick={() => setError(null)}
            className="text-text-muted hover:text-text-primary text-[length:var(--text-meta)]"
            aria-label="关闭错误提示"
          >
            <X className="icon-sm" />
          </button>
        </div>
      )}
      {warning && (
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-[length:var(--text-meta)] text-warning flex items-start justify-between gap-2">
          <span>{warning}</span>
          <button
            type="button"
            onClick={() => setWarning(null)}
            className="text-text-muted hover:text-text-primary text-[length:var(--text-meta)]"
            aria-label="关闭警告"
          >
            <X className="icon-sm" />
          </button>
        </div>
      )}

      {loading ? (
        <SkeletonGrid count={3} />
      ) : filtered.length === 0 ? (
        <EmptyHint
          icon={Boxes}
          text={
            toolsets.length === 0
              ? "后端 tool registry 还是空的。重启后端或在 main.rs 注册更多工具集后会自动显示。"
              : "没有匹配的工具集。"
          }
        />
      ) : (
        <Grid empty={false}>
          {filtered.map((t) => (
            <ToolsetCard
              key={t.name}
              toolset={t}
              enabled={enabled.has(t.name)}
              saving={saving.has(t.name)}
              onToggle={handleToggle}
            />
          ))}
        </Grid>
      )}

      <p className="text-[length:var(--text-meta)] text-text-muted pb-6">工具集由后端 ToolRegistry 自动注册；关闭后该组的所有工具会从 LLM 的工具列表里消失，下次 agent 调用就不会看到。</p>
    </>
  );
}

function ToolsetCard({
  toolset,
  enabled,
  saving,
  onToggle,
}: {
  toolset: ToolsetInfo;
  enabled: boolean;
  saving: boolean;
  onToggle: (name: string, next: boolean) => void;
}) {
  const Icon = pickIconByName(toolset.name);
  // 工具清单默认只露 4 个：8 个工具的工具集一屏塞满会把网格撑得很乱。
  // 「全部工具」展开是原型里确认过的交互，卡片自带这个能力，不需要另外加工具。
  const [expanded, setExpanded] = useState(false);
  const tools = toolset.tools;
  const shown = expanded ? tools : tools.slice(0, 4);
  const hidden = tools.length - shown.length;
  return (
    <div className="group flex flex-col gap-2.5 px-3 py-3 rounded-xl bg-bg-card hover:bg-bg-card-hover transition-colors">
      <div className="flex items-start gap-3">
        <ToolCardIcon>
          <Icon className="icon-lg" />
        </ToolCardIcon>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">{toolset.label}</span>
            <MgmtTag tone="neutral">{tools.length} 个工具</MgmtTag>
          </div>
          <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">
            {toolset.description || "（无描述）"}
          </p>
          {tools.length > 0 && (
            <div className="flex flex-wrap gap-1 mt-1.5">
              {shown.map((tool) => (
                <span
                  key={tool}
                  className="font-mono text-[length:var(--text-tiny)] text-text-secondary bg-[color-mix(in_srgb,var(--color-text-primary)_5%,transparent)] rounded px-1.5 py-0.5"
                >
                  {tool}
                </span>
              ))}
              {hidden > 0 && !expanded && (
                <span className="text-[length:var(--text-tiny)] text-text-muted self-center">+{hidden}</span>
              )}
            </div>
          )}
          {tools.length > 4 && (
            <button
              type="button"
              onClick={() => setExpanded((v) => !v)}
              className="mt-1.5 text-[length:var(--text-tiny)] font-medium text-text-muted hover:text-text-primary"
              aria-expanded={expanded}
            >
              {expanded ? "收起" : "全部工具"}
            </button>
          )}
        </div>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => onToggle(toolset.name, !enabled)}
          disabled={saving}
          offLabel={`关闭 ${toolset.label} 工具集`}
          onLabel={`启用 ${toolset.label} 工具集`}
          className="shrink-0 mt-0.5"
        />
      </div>
    </div>
  );
}

// ── Shared building blocks ──────────────────────────────────────────

export function SearchBar({
  query,
  onQuery,
  right,
}: {
  query: string;
  onQuery: (q: string) => void;
  right?: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 mb-5">
      <div className="relative flex-1">
        <Search className="icon-md absolute left-3 top-1/2 -translate-y-1/2 text-text-muted" />
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder="搜索名称、描述或类别"
          className="w-full h-9 bg-bg-tertiary border-0 rounded-lg pl-9 pr-3 text-[length:var(--text-ui)] text-text-primary placeholder-text-muted outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
        />
      </div>
      {right}
    </div>
  );
}

/**
 * 「共 N 个 · M 启用」统计带。
 *
 * 原先是一个 `border + bg-bg-card` 的方块，视觉上跟输入框同形，读起来像
 * 一个禁用的输入框而不是一句统计。改成：数字大、标签小、右侧一条进度计量。
 * 无描边（靠 bg-bg-card 浮出）。
 */
function SummaryBar({
  total,
  enabled,
  totalLabel,
  enabledLabel,
}: {
  total: number;
  enabled: number;
  totalLabel: string;
  enabledLabel: string;
}) {
  const pct = total > 0 ? Math.round((enabled / total) * 100) : 0;
  return (
    <div className="mb-3 flex items-center gap-5 px-4 py-3 rounded-[14px] bg-bg-card">
      <div className="flex flex-col shrink-0">
        <span className="text-xl font-bold leading-none tracking-tight text-text-primary">{total}</span>
        <span className="text-[length:var(--text-tiny)] text-text-muted mt-0.5">{totalLabel}</span>
      </div>
      <div className="flex flex-col shrink-0">
        <span
          className="text-xl font-bold leading-none tracking-tight"
          style={{ color: "var(--color-success)" }}
        >
          {enabled}
        </span>
        <span className="text-[length:var(--text-tiny)] text-text-muted mt-0.5">{enabledLabel}</span>
      </div>
      <div className="flex-1 min-w-20">
        <div className="flex justify-between text-[length:var(--text-tiny)] text-text-muted mb-1.5">
          <span>启用占比</span>
          <span className="tabular-nums">{pct}%</span>
        </div>
        <div className="h-1.5 rounded-full bg-bg-tertiary overflow-hidden">
          <div
            className="h-full rounded-full transition-[width] duration-300"
            style={{ width: `${pct}%`, background: "var(--color-btn-primary-bg)" }}
          />
        </div>
      </div>
    </div>
  );
}

function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mb-6">
      <div className="flex items-baseline justify-between mb-3">
        <h2 className="text-base font-medium text-text-primary">{title}</h2>
        {hint && <span className="text-[length:var(--text-meta)] text-text-muted">{hint}</span>}
      </div>
      {children}
    </section>
  );
}

function Grid({
  children,
  empty,
  emptyText,
}: {
  children: React.ReactNode;
  empty: boolean;
  emptyText?: string;
}) {
  if (empty) {
    return (
      <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
        <div className="col-span-full text-center text-sm text-text-muted py-6">{emptyText}</div>
      </div>
    );
  }
  return <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pb-2">{children}</div>;
}

function EmptyHint({ icon: Icon, text }: { icon: LucideIcon; text: string }) {
  return (
    <div className="flex flex-col items-center gap-2 text-text-muted py-10">
      <Icon className="icon-lg" />
      <p className="text-sm">{text}</p>
    </div>
  );
}

function SkeletonGrid({ count }: { count: number }) {
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pb-2">
      {Array.from({ length: count }).map((_, i) => (
        <div
          key={i}
          className="h-[68px] rounded-xl bg-bg-card/40 animate-pulse"
        />
      ))}
    </div>
  );
}

function PluginCard({
  icon,
  name,
  description,
  badge,
  enabled,
  saving,
  disabled,
  highlight = false,
  /** 右侧装饰图标（开关按钮之前）。默认无。 */
  rightIcon,
  onToggle,
}: {
  icon: React.ReactNode;
  name: string;
  description: string;
  badge: string;
  enabled: boolean;
  saving: boolean;
  disabled: boolean;
  highlight?: boolean;
  rightIcon?: React.ReactNode;
  onToggle: () => void;
}) {
  // 无描边：卡片靠 bg-bg-card 浮出（浅色主题下 bg-app 才是白的，卡片比底暗一档，
  // 夜墨则是 #1d1f22 压在 #171717 上）。描边在这种底色上只会画出一排灰线。
  // 状态不再叠「已启用/已停用」文字——开关本身已是状态，重复只会更吵；
  // 完整说明留在 ToggleSwitch 的 title/aria-label 里。
  //
  // 此前这里有一个 `noBorder` 分支走 `bg-bg-card/40`，全项目只有插件页一处调用。
  // 那是把**背景调淡**而不是去描边：`#f6f6f6` 按 40% 压在白底上只剩 `#fbfbfb`，
  // 卡片等于不存在——令牌怎么调都救不回来，也就是为什么唯独插件页调色阶毫无
  // 反应。已删除该 prop，插件卡片与其余所有卡片走同一条实色分支。
  return (
    <div
      className={`h-enter group flex items-center gap-3 px-3 py-2.5 rounded-xl transition-colors ${
        highlight ? "bg-bg-card-hover" : "bg-bg-card hover:bg-bg-card-hover"
      }`}
    >
      <ToolCardIcon>
        {icon}
      </ToolCardIcon>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">{name}</span>
          <MgmtTag tone="neutral" className="shrink-0">{badge}</MgmtTag>
        </div>
        <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">{description}</p>
      </div>
      {rightIcon && <span className="shrink-0 text-text-faint">{rightIcon}</span>}
      <ToggleSwitch enabled={enabled} onToggle={onToggle} disabled={saving || disabled} offLabel={`停用 ${name}`} onLabel={`启用 ${name}`} className="shrink-0" />
    </div>
  );
}
