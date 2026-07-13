import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Search,
  Plus,
  Sparkles,
  RefreshCw,
  FileCode,
  Terminal,
  Globe,
  Database,
  Cpu,
  Boxes,
  Layers,
  Plug,
  AlertTriangle,
  X,
  Network,
  RotateCcw,
  Trash2,
  ArrowUpRight,
  Package,
  Wand2,
} from "lucide-react";
import * as api from "../api";
import type { PluginInspect, PluginManifest, PluginSettings, SkillInfo, ToolsetInfo } from "../api";
import type { LucideIcon } from "lucide-react";

type Tab = "plugins" | "skills" | "connectors" | "toolsets";

// Plugin toggles map to the backend's DashboardPluginSettings (5 hard slots).
// The icon is purely cosmetic — the source of truth is the boolean on the server.
const BUILTIN_PLUGINS: Array<{
  id: string;
  key: keyof PluginSettings;
  name: string;
  description: string;
  icon: LucideIcon;
  category: "MCP" | "Tools";
}> = [
  { id: "mcp-filesystem", key: "mcp_filesystem", name: "文件系统", description: "读写本地文件和文件夹", icon: FileCode, category: "MCP" },
  { id: "mcp-terminal", key: "mcp_terminal", name: "终端", description: "执行 Shell 命令", icon: Terminal, category: "MCP" },
  { id: "mcp-browser", key: "mcp_browser", name: "浏览器", description: "浏览和检索网页内容", icon: Globe, category: "MCP" },
  { id: "mcp-database", key: "mcp_database", name: "数据库", description: "执行 SQLite 和 PostgreSQL 查询", icon: Database, category: "MCP" },
  { id: "tool-code-exec", key: "tool_code_exec", name: "代码执行", description: "在沙箱中运行代码", icon: Cpu, category: "Tools" },
];

const DEFAULT_PLUGIN_SETTINGS: PluginSettings = {
  mcp_filesystem: true,
  mcp_terminal: true,
  mcp_browser: true,
  mcp_database: false,
  tool_code_exec: true,
};

export function PluginsView() {
  const [tab, setTab] = useState<Tab>("plugins");
  const [query, setQuery] = useState("");

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
      <div className="max-w-5xl mx-auto">
        <div className="flex items-center gap-4 mb-6 text-sm">
          <TabButton active={tab === "plugins"} onClick={() => setTab("plugins")} icon={Plug} label="插件" />
          <TabButton active={tab === "skills"} onClick={() => setTab("skills")} icon={Sparkles} label="技能" />
          <TabButton active={tab === "connectors"} onClick={() => setTab("connectors")} icon={Network} label="连接器" />
          <TabButton active={tab === "toolsets"} onClick={() => setTab("toolsets")} icon={Boxes} label="工具集" />
        </div>

        <div className="text-center mb-6">
          {/* PLAN_UI_ENHANCEMENT §5.1 (2026-06-22): 标题按 tab 区分 */}
          <h1 className="text-3xl font-semibold text-text-primary">
            {tab === "plugins" && "插件"}
            {tab === "skills" && "技能"}
            {tab === "connectors" && "连接器"}
            {tab === "toolsets" && "工具集"}
          </h1>
          <p className="text-sm text-text-muted mt-2">
            {tab === "plugins" && "启用插件，扩展 AI 助手能力。"}
            {tab === "skills" && "启用技能，让 AI 助手掌握特定场景的工作流。"}
            {tab === "connectors" && "启用连接器，让 AI 助手能访问外部数据源。"}
            {tab === "toolsets" && "启用工具集，组合多个工具形成完整能力。"}
          </p>
        </div>

        {tab === "plugins" && <PluginsTab query={query} onQuery={setQuery} />}
        {tab === "skills" && <SkillsTab query={query} onQuery={setQuery} />}
        {tab === "connectors" && <ConnectorsTab query={query} onQuery={setQuery} />}
        {tab === "toolsets" && <ToolsetsTab query={query} onQuery={setQuery} />}
      </div>
    </div>
  );
}

function TabButton({
  active,
  onClick,
  icon: Icon,
  label,
}: {
  active: boolean;
  onClick: () => void;
  icon: LucideIcon;
  label: string;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1.5 ${
        active ? "text-text-primary font-medium" : "text-text-muted hover:text-text-secondary"
      }`}
    >
      <Icon size={14} />
      {label}
    </button>
  );
}

// ── Plugins tab: built-in toggles + discovered manifests ────────────

function PluginsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [settings, setSettings] = useState<PluginSettings>(DEFAULT_PLUGIN_SETTINGS);
  const [manifests, setManifests] = useState<PluginManifest[]>([]);
  const [inspectMap, setInspectMap] = useState<Record<string, PluginInspect>>({});
  const [loading, setLoading] = useState(true);
  const [savingId, setSavingId] = useState<string | null>(null);
  const [rescanning, setRescanning] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [s, m, im] = await Promise.all([
        api.getPluginSettings(),
        api.getPluginManifests(),
        api.inspectAllPlugins(),
      ]);
      setSettings(s);
      setManifests(m);
      setInspectMap(im);
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

  const handleRescan = useCallback(async () => {
    setRescanning(true);
    try {
      await api.rescanPlugins();
      const m = await api.getPluginManifests();
      setManifests(m);
    } finally {
      setRescanning(false);
    }
  }, []);

  const q = query.trim().toLowerCase();
  const filteredBuiltins = q
    ? BUILTIN_PLUGINS.filter((p) => `${p.name} ${p.description} ${p.category}`.toLowerCase().includes(q))
    : BUILTIN_PLUGINS;
  const filteredManifests = q
    ? manifests.filter((m) =>
        `${m.name} ${m.description ?? ""} ${m.category ?? ""}`.toLowerCase().includes(q),
      )
    : manifests;

  return (
    <>
      <SearchBar
        query={query}
        onQuery={onQuery}
        right={
          <button
            onClick={handleRescan}
            disabled={rescanning || loading}
            className="h-9 px-3 rounded-lg bg-bg-card border border-border-primary text-xs text-text-secondary hover:text-text-primary disabled:opacity-50 flex items-center gap-1.5"
          >
            <RefreshCw size={12} className={rescanning ? "animate-spin" : ""} />
            重新扫描
          </button>
        }
      />

      <Section title="内置工具" hint="由后端持久化保存于 dashboard_plugins.json">
        <Grid empty={!loading && filteredBuiltins.length === 0} emptyText="没有匹配的内置工具。">
          {filteredBuiltins.map((plugin) => {
            const enabled = Boolean(settings[plugin.key]);
            return (
              <PluginCard
                key={plugin.id}
                icon={<plugin.icon size={16} />}
                name={plugin.name}
                description={plugin.description}
                badge={plugin.category}
                enabled={enabled}
                saving={savingId === plugin.id}
                disabled={loading}
                noBorder
                rightIcon={<ArrowUpRight size={12} />}
                onToggle={() => togglePlugin(plugin, !enabled)}
              />
            );
          })}
        </Grid>
      </Section>

      <Section
        title="官方插件市场"
        hint={`来自 NousResearch/hermix-agent，首次启动自动 seed，共 ${manifests.length} 个`}
      >
        {loading ? (
          <SkeletonGrid count={3} />
        ) : filteredManifests.length === 0 ? (
          <EmptyHint
            icon={Layers}
            text={
              manifests.length === 0
                ? "还没有发现任何插件清单。点“重新扫描”会触发服务端 seed 18 个官方插件。"
                : "没有匹配的清单。"
            }
          />
        ) : (
          <Grid empty={false}>
            {filteredManifests.map((m, i) => {
              // PluginManifest is intentionally open (index signature is unknown),
              // so read the extra backend fields via `as unknown` first to keep
              // the cast intentional.
              const extra = m as unknown as {
                availability?: string;
                display_name?: string;
                tools?: unknown;
                tags?: unknown;
              };
              const availability = extra.availability ?? "seeded";
              const displayName = extra.display_name ?? m.name;
              const declaredTools = Array.isArray(extra.tools)
                ? (extra.tools as unknown[]).filter((x): x is string => typeof x === "string")
                : [];
              const pluginId = (m.id as string) ?? m.name;
              const runtime = inspectMap[pluginId];
              const runtimeTools = runtime?.tools ?? [];
              const isWired = availability === "ready" && runtimeTools.length > 0;
              const description = isWired
                ? `✓ 后端已挂接 ${runtimeTools.length} 个工具：${runtimeTools.slice(0, 3).join(", ")}${runtimeTools.length > 3 ? "…" : ""}`
                : (m.description as string) ||
                  (declaredTools.length > 0
                    ? `${declaredTools.length} 工具 · ${declaredTools.slice(0, 3).join(", ")}…`
                    : "（无描述）");
              return (
                <PluginCard
                  key={m.id ?? m.name ?? i}
                  icon={<Package size={16} />}
                  name={displayName}
                  description={description}
                  badge={
                    isWired
                      ? `${m.category ?? "plugin"} · 已挂接`
                      : availability === "ready"
                        ? `${m.category ?? "plugin"} · 可用`
                        : `${m.category ?? "plugin"} · 待实装`
                  }
                  highlight={isWired}
                  enabled
                  saving={false}
                  disabled
                  noBorder
                  rightIcon={<ArrowUpRight size={12} />}
                  onToggle={() => undefined}
                />
              );
            })}
          </Grid>
        )}
      </Section>
    </>
  );
}

// ── Skills tab: GET /api/skills + toggle ────────────────────────────

const CATEGORY_LABELS: Record<string, string> = {
  "software-development": "软件开发",
  devops: "运维",
  productivity: "效率",
  finance: "金融",
  lifestyle: "生活",
  writing: "写作",
  office: "Office",
};

function SkillsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

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
  const filtered = q
    ? skills.filter((s) => `${s.name} ${s.description} ${s.category}`.toLowerCase().includes(q))
    : skills;

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

  const categoryOrder = useMemo(() => {
    const priority = ["software-development", "devops", "productivity", "finance", "lifestyle"];
    const keys = Object.keys(grouped);
    return priority.filter((k) => keys.includes(k)).concat(
      keys.filter((k) => !priority.includes(k)).sort(),
    );
  }, [grouped]);

  const catLabel = useCallback((key: string) => CATEGORY_LABELS[key] || key, []);

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
          <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
        </button>
      </div>

      {!loading && skills.length > 0 && (
        <div className="mb-3 px-3 py-1.5 text-xs text-text-muted bg-bg-card rounded-lg border border-border-primary">
          共 {skills.length} 个技能，{enabledCount} 个启用
        </div>
      )}

      {error && (
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card border border-border-primary text-xs text-error">
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
          <div key={cat} className="mb-4">
            <h4 className="text-xs font-medium text-text-muted uppercase tracking-wider mb-2 px-1">
              {catLabel(cat)}
              <span className="ml-1.5 text-text-dimmed font-normal normal-case">
                ({grouped[cat].length})
              </span>
            </h4>
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

      <p className="text-xs text-text-muted pb-6">技能数据来自 hermix-skills 仓库，禁用会从文件系统中删除该技能。</p>
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
      {/* PLAN_UI_ENHANCEMENT §5.3: 技能列表项去边框 + 右侧加 ArrowUpRight 装饰。 */}
      <div className="rounded-xl overflow-hidden">
        <div className="flex items-center gap-3 px-3 py-2.5">
          <button onClick={openContent}
            className="w-9 h-9 rounded-lg bg-bg-tertiary flex items-center justify-center text-text-secondary shrink-0 hover:bg-bg-surface transition-colors"
            title="查看技能内容"
           aria-label="查看技能内容">
            <Wand2 size={14} />
          </button>
          <div className="min-w-0 flex-1 cursor-pointer" onClick={openContent}>
            <div className="flex items-center gap-2">
              <span className="text-sm text-text-primary truncate">{skill.name}</span>
              <span className="text-[10px] uppercase tracking-wider text-text-muted border border-border-primary rounded px-1.5 py-0.5 shrink-0">
                {catLabel(skill.category) || "skill"}
              </span>
            </div>
            <p className="text-xs text-text-muted mt-0.5 truncate">{skill.description || "（无描述）"}</p>
          </div>
          <ArrowUpRight size={12} className="text-text-faint shrink-0" />
          <button
            onClick={(e) => { e.stopPropagation(); requestToggle(); }}
            disabled={toggling}
            className="w-6 h-6 rounded-full border border-border-secondary text-text-muted hover:text-text-primary hover:border-text-muted disabled:opacity-50 shrink-0"
            title={skill.enabled ? "禁用" : "启用"}
          >
            {skill.enabled ? (
              <Sparkles size={12} className="mx-auto text-success" />
            ) : (
              <Plus size={12} className="mx-auto" />
            )}
          </button>
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
          title="禁用技能"
          message={
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
      <div className="modal-card modal-card-lg" onClick={(e) => e.stopPropagation()} style={{ maxHeight: "80vh", display: "flex", flexDirection: "column" }}>
        <div className="modal-header" style={{ flexShrink: 0 }}>
          <div className="modal-icon modal-icon-info"><Sparkles size={18} /></div>
          <div className="modal-title-block">
            <h3 className="modal-title">{skill.name}</h3>
            <p className="modal-subtitle">
              {catLabel(skill.category)}{skill.description ? ` · ${skill.description}` : ""}
            </p>
          </div>
          <button onClick={onClose} className="modal-close" aria-label="关闭">
            <X size={16} />
          </button>
        </div>
        <div style={{ flex: 1, overflow: "auto", padding: "12px 16px" }}>
          {loading ? (
            <div className="text-xs text-text-muted animate-pulse">加载中...</div>
          ) : content ? (
            <pre className="text-xs text-text-secondary leading-relaxed whitespace-pre-wrap">
              {content}
            </pre>
          ) : (
            <div className="text-xs text-text-muted">
              <p>无法加载技能内容</p>
              <p className="mt-2 opacity-60">存放路径：~/.hermix/skills/{skill.category}/{skill.name}/SKILL.md</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function ConfirmDialog({
  title,
  message,
  confirmLabel,
  busy,
  onCancel,
  onConfirm,
}: {
  title: string;
  message: React.ReactNode;
  confirmLabel: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  useEffect(() => {
    if (busy) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onCancel]);

  return (
    <div
      role="dialog"
      aria-modal="true"
      className="modal-backdrop"
      onClick={onCancel}
    >
      <div
        className="modal-card modal-card-sm"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <div className="modal-icon modal-icon-warn"><AlertTriangle size={18} /></div>
          <div className="modal-title-block">
            <h3 className="modal-title">{title}</h3>
            <p className="modal-subtitle">{message}</p>
          </div>
          <button
            onClick={onCancel}
            className="modal-close"
            aria-label="关闭"
          >
            <X size={16} />
          </button>
        </div>
        <div className="modal-footer">
          <button
            onClick={onCancel}
            disabled={busy}
            className="modal-btn modal-btn-secondary"
          >
            取消
          </button>
          <button
            onClick={onConfirm}
            disabled={busy}
            className="modal-btn modal-btn-danger"
          >
            {busy ? "处理中..." : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Connectors tab: user-configured MCP servers ──────────────────────

function ConnectorsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [servers, setServers] = useState<api.McpServerEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [showAddForm, setShowAddForm] = useState(false);
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  const [newServer, setNewServer] = useState({
    name: "",
    transport: "stdio" as api.McpTransport,
    command: "",
    args: "",
    url: "",
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
  const reset = async () => {
    const list = await api.resetMcpServersToOfficial();
    setServers(list);
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
        env: newServer.env.trim() ? Object.fromEntries(newServer.env.trim().split("\n").filter(Boolean).map((line) => { const idx = line.indexOf("="); return idx > 0 ? [line.slice(0, idx).trim(), line.slice(idx + 1).trim()] : null; }).filter(Boolean) as [string, string][]) : undefined,
        enabled: true,
        description: newServer.description.trim() || undefined,
      };
      const created = await api.addMcpServer(spec);
      setServers((prev) => [...prev, created]);
      setNewServer({ name: "", transport: "stdio", command: "", args: "", url: "", env: "", description: "" });
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

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

      <div className="flex items-center justify-between mb-3">
        <span className="text-[11.5px] text-text-muted">
          MCP (Model Context Protocol) 服务,通过 stdio 启动本地子进程或 HTTP/SSE 连接远程端点。
        </span>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowAddForm((v) => !v)}
            className="flex items-center gap-1 px-2 py-1 rounded-md text-[11px] text-accent hover:bg-bg-tertiary"
            title="添加自定义 MCP 服务"
          >
            <Plus size={11} /> 添加 MCP
          </button>
          <button
            onClick={reset}
            className="flex items-center gap-1 px-2 py-1 rounded-md text-[11px] text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
            title="恢复为官方示例 5 个 seed"
          >
            <RotateCcw size={11} /> 恢复官方示例
          </button>
        </div>
      </div>

      {showAddForm && (
        <div className="rounded-xl border border-border-primary bg-bg-card p-4 mb-3 space-y-3">
          <div className="text-sm font-medium text-text-primary">添加 MCP 服务</div>
          {addError && (
            <div className="text-[11px] text-error bg-error/10 rounded px-2 py-1">{addError}</div>
          )}
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="text-[10.5px] text-text-muted">名称</label>
              <input value={newServer.name} onChange={(e) => setNewServer((p) => ({ ...p, name: e.target.value }))} placeholder="my-mcp-server" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
            </div>
            <div>
              <label className="text-[10.5px] text-text-muted">传输方式</label>
              <select value={newServer.transport} onChange={(e) => setNewServer((p) => ({ ...p, transport: e.target.value as api.McpTransport }))} className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary">
                <option value="stdio">stdio</option>
                <option value="http">http</option>
              </select>
            </div>
            {newServer.transport === "stdio" ? (
              <>
                <div>
                  <label className="text-[10.5px] text-text-muted">Command</label>
                  <input value={newServer.command} onChange={(e) => setNewServer((p) => ({ ...p, command: e.target.value }))} placeholder="npx" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                </div>
                <div>
                  <label className="text-[10.5px] text-text-muted">Args (空格分隔)</label>
                  <input value={newServer.args} onChange={(e) => setNewServer((p) => ({ ...p, args: e.target.value }))} placeholder="-y @modelcontextprotocol/server-filesystem /tmp" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                </div>
              </>
            ) : (
              <div className="col-span-2">
                <label className="text-[10.5px] text-text-muted">URL</label>
                <input value={newServer.url} onChange={(e) => setNewServer((p) => ({ ...p, url: e.target.value }))} placeholder="https://example.com/mcp" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
              </div>
            )}
            <div className="col-span-2">
              <label className="text-[10.5px] text-text-muted">环境变量 (每行 KEY=value)</label>
              <textarea value={newServer.env} onChange={(e) => setNewServer((p) => ({ ...p, env: e.target.value }))} placeholder={"API_KEY=xxx"} rows={2} className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary resize-none" />
            </div>
            <div className="col-span-2">
              <label className="text-[10.5px] text-text-muted">描述 (可选)</label>
              <input value={newServer.description} onChange={(e) => setNewServer((p) => ({ ...p, description: e.target.value }))} placeholder="自定义文件系统 MCP" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
            </div>
          </div>
          <div className="flex items-center gap-2">
            <button onClick={handleAdd} disabled={adding} className="px-3 py-1.5 rounded-lg bg-accent text-white text-xs font-medium hover:opacity-90 disabled:opacity-50">
              {adding ? "添加中..." : "确认添加"}
            </button>
            <button onClick={() => { setShowAddForm(false); setAddError(null); }} className="px-3 py-1.5 rounded-lg text-xs text-text-muted hover:text-text-primary hover:bg-bg-tertiary">
              取消
            </button>
          </div>
        </div>
      )}

      {loading ? (
        <SkeletonGrid count={3} />
      ) : filtered.length === 0 ? (
        <EmptyHint
          icon={Network}
          text={
            servers.length === 0
              ? "没有 MCP 服务。点击「恢复官方示例」可以加载 5 个常用 seed。"
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

      <p className="text-xs text-text-muted pb-6 mt-4">
        配置需要 {`npx`} / {`uvx`} 等运行时可用。密钥请填入 env 字段。
      </p>
    </>
  );
}

// PLAN_UI_ENHANCEMENT §5.4: 每个连接器/工具集图标各不相同。
// 用一个稳定的字符串 hash 选择图标，保证同一名字总是同图标。
const CONNECTOR_ICONS: LucideIcon[] = [Network, Globe, Database, Cpu, Boxes, FileCode, Layers, Plug];
function pickIconByName(name: string): LucideIcon {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) | 0;
  const idx = Math.abs(h) % CONNECTOR_ICONS.length;
  return CONNECTOR_ICONS[idx];
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
    <div className="h-enter flex items-start gap-3 px-3 py-2.5 rounded-xl bg-bg-card/70 hover:bg-bg-card">
      <div className="w-9 h-9 rounded-lg bg-bg-tertiary flex items-center justify-center text-text-secondary shrink-0">
        <Icon size={16} />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-sm text-text-primary truncate">{server.name}</span>
          <span className="text-[10px] uppercase tracking-wider text-text-muted bg-bg-tertiary rounded px-1.5 py-0.5">
            {server.transport}
          </span>
          {server.enabled && (
            <span className="text-[10px] text-accent bg-accent-soft/40 rounded px-1.5 py-0.5">启用中</span>
          )}
        </div>
        {server.description && (
          <p className="text-xs text-text-muted mt-0.5 line-clamp-2">{server.description}</p>
        )}
        <p className="text-[10.5px] font-mono text-text-faint mt-1 truncate">
          {server.transport === "stdio"
            ? `${server.command ?? "?"} ${(server.args ?? []).join(" ")}`
            : server.url ?? "(no url)"}
        </p>
      </div>
      <div className="flex items-center gap-1.5 shrink-0">
        <button
          onClick={() => onToggle(!server.enabled)}
          className={`h-7 w-11 rounded-full ${
            server.enabled ? "bg-accent" : "bg-bg-tertiary"
          }`}
          title={server.enabled ? "停用" : "启用"}
        >
          <span
            className={`block w-5 h-5 rounded-full bg-white shadow transition-transform ${
              server.enabled ? "translate-x-5" : "translate-x-1"
            }`}
          />
        </button>
        <button
          onClick={onRemove}
          className="h-7 w-7 rounded-md text-text-muted hover:text-error hover:bg-error/10 flex items-center justify-center"
          title="删除"
         aria-label="删除">
          <Trash2 size={13} />
        </button>
      </div>
    </div>
  );
}

// ── Toolsets tab: backend tool registry (a.k.a. toolset groups) ──────

function ToolsetsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
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

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

      {error && (
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-xs text-error flex items-start justify-between gap-2">
          <span>{error}</span>
          <button
            type="button"
            onClick={() => setError(null)}
            className="text-text-muted hover:text-text-primary text-xs"
            aria-label="关闭错误提示"
          >
            ✕
          </button>
        </div>
      )}
      {warning && (
        <div className="mb-4 px-3 py-2 rounded-lg bg-bg-card text-xs text-warning flex items-start justify-between gap-2">
          <span>{warning}</span>
          <button
            type="button"
            onClick={() => setWarning(null)}
            className="text-text-muted hover:text-text-primary text-xs"
            aria-label="关闭警告"
          >
            ✕
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

      <p className="text-xs text-text-muted pb-6">工具集由后端 ToolRegistry 自动注册；关闭后该组的所有工具会从 LLM 的工具列表里消失，下次 agent 调用就不会看到。</p>
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
  return (
    <div className="group flex items-start gap-3 px-3 py-2.5 rounded-xl bg-bg-card/80 hover:bg-bg-card">
      <div className="w-9 h-9 rounded-lg bg-bg-tertiary flex items-center justify-center text-text-secondary shrink-0">
        <Icon size={16} />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-sm text-text-primary truncate">{toolset.label}</span>
          <span className="text-[10px] uppercase tracking-wider text-text-muted border border-border-primary rounded px-1.5 py-0.5">
            {toolset.tools.length} 个工具
          </span>
        </div>
        <p className="text-xs text-text-muted mt-0.5 line-clamp-2">{toolset.description || "（无描述）"}</p>
        {toolset.tools.length > 0 && (
          <div className="flex flex-wrap gap-1 mt-1.5">
            {toolset.tools.slice(0, 6).map((tool) => (
              <span key={tool} className="text-[10px] text-text-secondary bg-bg-tertiary rounded px-1.5 py-0.5">
                {tool}
              </span>
            ))}
            {toolset.tools.length > 6 && (
              <span className="text-[10px] text-text-muted">+{toolset.tools.length - 6}</span>
            )}
          </div>
        )}
      </div>
      <div className="shrink-0 mt-1.5 flex flex-col items-end gap-1">
        <button
          type="button"
          role="switch"
          aria-checked={enabled}
          aria-label={`${enabled ? "关闭" : "启用"} ${toolset.label} 工具集`}
          disabled={saving}
          onClick={() => onToggle(toolset.name, !enabled)}
          className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
            enabled ? "bg-accent" : "bg-bg-tertiary"
          } ${saving ? "opacity-60 cursor-wait" : "cursor-pointer"}`}
        >
          <span
            className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white shadow transition-transform ${
              enabled ? "translate-x-5" : "translate-x-1"
            }`}
          />
        </button>
        <span
          className={`text-[10px] uppercase tracking-wider ${
            enabled ? "text-success" : "text-text-muted"
          }`}
        >
          {saving ? "保存中…" : enabled ? "已启用" : "已停用"}
        </span>
      </div>
    </div>
  );
}

// ── Shared building blocks ──────────────────────────────────────────

function SearchBar({
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
        <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-text-muted" />
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder="搜索名称、描述或类别"
          className="w-full h-9 bg-bg-card border border-border-primary rounded-lg pl-9 pr-3 text-sm text-text-primary placeholder-text-muted outline-none"
        />
      </div>
      {right}
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
        {hint && <span className="text-[11px] text-text-muted">{hint}</span>}
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
      <Icon size={20} />
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
          className="h-[68px] rounded-xl border border-border-primary bg-bg-card/40 animate-pulse"
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
  /** PLAN_UI_ENHANCEMENT §5.2 (2026-06-22): 关闭边框。默认 false。 */
  noBorder = false,
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
  noBorder?: boolean;
  rightIcon?: React.ReactNode;
  onToggle: () => void;
}) {
  return (
    <div className={`h-enter group flex items-center gap-3 px-3 py-2.5 rounded-xl ${noBorder ? "" : "border"} ${highlight ? "border-accent/40 bg-accent-soft/20" : (noBorder ? "bg-bg-card/40 hover:bg-bg-card/70" : "border-border-primary bg-bg-card/80 hover:bg-bg-card hover:border-border-secondary")}`}>
      <div className="w-9 h-9 rounded-lg bg-bg-tertiary flex items-center justify-center text-text-secondary shrink-0">
        {icon}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-sm text-text-primary truncate">{name}</span>
          <span className="text-[10px] uppercase tracking-wider text-text-muted border border-border-primary rounded px-1.5 py-0.5 shrink-0">
            {badge}
          </span>
        </div>
        <p className="text-xs text-text-muted mt-0.5 truncate">{description}</p>
      </div>
      {rightIcon && <span className="shrink-0 text-text-faint">{rightIcon}</span>}
      <button
        onClick={onToggle}
        disabled={saving || disabled}
        className="w-6 h-6 rounded-full border border-border-secondary text-text-muted hover:text-text-primary hover:border-text-muted disabled:opacity-50 shrink-0"
        title={disabled ? "只读" : enabled ? "禁用" : "启用"}
      >
        {enabled ? (
          <Sparkles size={12} className="mx-auto text-success" />
        ) : (
          <Plus size={12} className="mx-auto" />
        )}
      </button>
    </div>
  );
}
