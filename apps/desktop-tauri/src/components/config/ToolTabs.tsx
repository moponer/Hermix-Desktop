import { ToolCardIcon, toneForCategory } from "./ToolCardIcon";
import { ToggleSwitch } from "../ToggleSwitch";
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
  Trash2,
  ArrowUpRight,
  Wand2,
} from "lucide-react";
import * as api from "../../api";
import type { PluginSettings, SkillInfo, ToolsetInfo } from "../../api";
import type { LucideIcon } from "lucide-react";
import { McpCatalogGrid } from "./McpCatalogGrid";

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
                noBorder
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

const CATEGORY_LABELS: Record<string, string> = {
  "software-development": "软件开发",
  devops: "运维",
  productivity: "效率",
  finance: "金融",
  lifestyle: "生活",
  writing: "写作",
  office: "Office",
};

export function SkillsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
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
          <RefreshCw className={`icon-md ${loading ? "animate-spin" : ""}`} />
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
      <div className="rounded-xl overflow-hidden">
        <div className="flex items-center gap-3 px-3 py-2.5">
          <button onClick={openContent} className="shrink-0 rounded-[var(--radius-md)]"
            title="查看技能内容"
            aria-label="查看技能内容">
            <ToolCardIcon tone={toneForCategory(catLabel(skill.category))} interactive>
              <Wand2 className="icon-md" />
            </ToolCardIcon>
          </button>
          <div className="min-w-0 flex-1 cursor-pointer" onClick={openContent}>
            <div className="flex items-center gap-2">
              <span className="text-sm text-text-primary truncate">{skill.name}</span>
              <span className="text-[length:var(--text-meta)] uppercase tracking-wider text-text-muted border border-border-primary rounded px-1.5 py-0.5 shrink-0">
                {catLabel(skill.category) || "skill"}
              </span>
            </div>
            <p className="text-xs text-text-muted mt-0.5 truncate">{skill.description || "（无描述）"}</p>
          </div>
          <ArrowUpRight className="icon-sm text-text-faint shrink-0" />
          <ToggleSwitch enabled={skill.enabled} onToggle={requestToggle} disabled={toggling} offLabel="禁用" onLabel="启用" className="shrink-0" />
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
          <div className="modal-icon modal-icon-info"><Sparkles className="icon-lg" /></div>
          <div className="modal-title-block">
            <h3 className="modal-title">{skill.name}</h3>
            <p className="modal-subtitle">
              {catLabel(skill.category)}{skill.description ? ` · ${skill.description}` : ""}
            </p>
          </div>
          <button onClick={onClose} className="modal-close" aria-label="关闭">
            <X className="icon-lg" />
          </button>
        </div>
        <div style={{ flex: 1, overflow: "auto", padding: "12px 16px" }}>
          {loading ? (
            <div className="text-xs text-text-muted animate-pulse">加载中…</div>
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
          <div className="modal-icon modal-icon-warn"><AlertTriangle className="icon-lg" /></div>
          <div className="modal-title-block">
            <h3 className="modal-title">{title}</h3>
            <p className="modal-subtitle">{message}</p>
          </div>
          <button
            onClick={onCancel}
            className="modal-close"
            aria-label="关闭"
          >
            <X className="icon-lg" />
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
            {busy ? "处理中…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Connectors tab: user-configured MCP servers ──────────────────────

type McpSpec = Omit<api.McpServerEntry, "id" | "createdAt">;

// 解析 Claude Desktop / Cursor 风格的 mcpServers JSON，自动拆成 stdio / http 条目。
export function parseMcpJson(raw: string): McpSpec[] {
  const data = JSON.parse(raw) as unknown;
  const root = (
    data && typeof data === "object" && "mcpServers" in (data as Record<string, unknown>)
      ? (data as { mcpServers: Record<string, unknown> }).mcpServers
      : data
  ) as Record<string, unknown>;
  if (!root || typeof root !== "object") {
    throw new Error("配置格式不正确：期望 mcpServers 对象");
  }
  const specs: McpSpec[] = [];
  for (const [name, cfgRaw] of Object.entries(root)) {
    const c = cfgRaw as Record<string, unknown>;
    const base = {
      name,
      enabled: true,
      description: typeof c.description === "string" ? c.description : undefined,
      env: typeof c.env === "object" && c.env ? (c.env as Record<string, string>) : undefined,
    };
    if (typeof c.url === "string" && c.url) {
      specs.push({ ...base, transport: "http", url: c.url });
    } else if (typeof c.command === "string" && c.command) {
      const args = Array.isArray(c.args)
        ? (c.args as unknown[]).filter((x): x is string => typeof x === "string")
        : typeof c.args === "string" && c.args
          ? c.args.split(/\s+/)
          : undefined;
      specs.push({ ...base, transport: "stdio", command: c.command, args });
    } else {
      throw new Error(`条目 "${name}" 缺少 command 或 url`);
    }
  }
  if (specs.length === 0) throw new Error("未解析到任何 MCP 服务");
  return specs;
}

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

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

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
        <div className="rounded-xl border border-border-primary bg-bg-card p-4 mb-3 space-y-3">
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
                  className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary resize-none font-mono"
                />
                <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">支持 mcpServers 对象或单条 server 配置，自动识别 stdio / http。</p>
              </div>
              <div className="flex items-center gap-2">
                <button onClick={handleImportPaste} disabled={adding} className="px-3 py-1.5 rounded-lg bg-accent text-accent-on-text text-xs font-medium hover:opacity-90 disabled:opacity-50">导入</button>
                <button onClick={() => { setShowAddForm(false); setAddError(null); }} className="px-3 py-1.5 rounded-lg text-xs text-text-muted hover:text-text-primary hover:bg-bg-tertiary">取消</button>
              </div>
            </>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="text-[length:var(--text-meta)] text-text-muted">名称</label>
                  <input value={newServer.name} onChange={(e) => setNewServer((p) => ({ ...p, name: e.target.value }))} placeholder="my-mcp-server" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                </div>
                <div>
                  <label className="text-[length:var(--text-meta)] text-text-muted">传输方式</label>
                  <select value={newServer.transport} onChange={(e) => setNewServer((p) => ({ ...p, transport: e.target.value as api.McpTransport }))} className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary">
                    <option value="stdio">stdio</option>
                    <option value="http">http</option>
                  </select>
                </div>
                {newServer.transport === "stdio" ? (
                  <>
                    <div>
                      <label className="text-[length:var(--text-meta)] text-text-muted">Command</label>
                      <input value={newServer.command} onChange={(e) => setNewServer((p) => ({ ...p, command: e.target.value }))} placeholder="npx" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                    </div>
                    <div>
                      <label className="text-[length:var(--text-meta)] text-text-muted">Args （空格分隔）</label>
                      <input value={newServer.args} onChange={(e) => setNewServer((p) => ({ ...p, args: e.target.value }))} placeholder="-y @modelcontextprotocol/server-filesystem /tmp" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                    </div>
                  </>
                ) : (
                  <div className="col-span-2">
                    <label className="text-[length:var(--text-meta)] text-text-muted">URL</label>
                    <input value={newServer.url} onChange={(e) => setNewServer((p) => ({ ...p, url: e.target.value }))} placeholder="https://example.com/mcp" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                  </div>
                )}
                <div className="col-span-2">
                  <label className="text-[length:var(--text-meta)] text-text-muted">环境变量 (每行 KEY=value)</label>
                  <textarea value={newServer.env} onChange={(e) => setNewServer((p) => ({ ...p, env: e.target.value }))} placeholder={"API_KEY=xxx"} rows={2} className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary resize-none" />
                </div>
                <div className="col-span-2">
                  <label className="text-[length:var(--text-meta)] text-text-muted">描述 （可选）</label>
                  <input value={newServer.description} onChange={(e) => setNewServer((p) => ({ ...p, description: e.target.value }))} placeholder="自定义文件系统 MCP" className="mt-0.5 w-full px-2.5 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary border border-border-secondary" />
                </div>
              </div>
              <div className="flex items-center gap-2">
                <button onClick={handleAdd} disabled={adding} className="px-3 py-1.5 rounded-lg bg-accent text-accent-on-text text-xs font-medium hover:opacity-90 disabled:opacity-50">
                  {adding ? "添加中..." : "确认添加"}
                </button>
                <button onClick={() => { setShowAddForm(false); setAddError(null); }} className="px-3 py-1.5 rounded-lg text-xs text-text-muted hover:text-text-primary hover:bg-bg-tertiary">
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

      <p className="text-xs text-text-muted mt-4 pb-5">
        配置需要 {`npx`} / {`uvx`} 等运行时可用。密钥请填入 env 字段。
      </p>

      {/* 内置目录：先逛清单，再决定装不装。放在已配置列表之后——
          自己的 server 是主角，逛目录是第二幕。 */}
      <McpCatalogGrid onInstalled={() => { void api.getMcpServers().then(setServers); }} />
    </>
  );
}

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
      <ToolCardIcon tone="info">
        <Icon className="icon-lg" />
      </ToolCardIcon>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-sm text-text-primary truncate">{server.name}</span>
          <span className="text-[length:var(--text-meta)] uppercase tracking-wider text-text-muted bg-bg-tertiary rounded px-1.5 py-0.5">
            {server.transport}
          </span>
          {server.enabled && (
            <span className="text-[length:var(--text-meta)] text-accent bg-accent-soft/40 rounded px-1.5 py-0.5">启用中</span>
          )}
        </div>
        {server.description && (
          <p className="text-xs text-text-muted mt-0.5 line-clamp-2">{server.description}</p>
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
            <X className="icon-sm" />
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
      <ToolCardIcon tone="accent">
        <Icon className="icon-lg" />
      </ToolCardIcon>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-sm text-text-primary truncate">{toolset.label}</span>
          <span className="text-[length:var(--text-meta)] uppercase tracking-wider text-text-muted border border-border-primary rounded px-1.5 py-0.5">
            {toolset.tools.length} 个工具
          </span>
        </div>
        <p className="text-xs text-text-muted mt-0.5 line-clamp-2">{toolset.description || "（无描述）"}</p>
        {toolset.tools.length > 0 && (
          <div className="flex flex-wrap gap-1 mt-1.5">
            {toolset.tools.slice(0, 6).map((tool) => (
              <span key={tool} className="text-[length:var(--text-meta)] text-text-secondary bg-bg-tertiary rounded px-1.5 py-0.5">
                {tool}
              </span>
            ))}
            {toolset.tools.length > 6 && (
              <span className="text-[length:var(--text-meta)] text-text-muted">+{toolset.tools.length - 6}</span>
            )}
          </div>
        )}
      </div>
      <div className="shrink-0 mt-1.5 flex flex-col items-end gap-1">
        <ToggleSwitch enabled={enabled} onToggle={() => onToggle(toolset.name, !enabled)} disabled={saving} offLabel={`关闭 ${toolset.label} 工具集`} onLabel={`启用 ${toolset.label} 工具集`} />
        <span
          className={`text-[length:var(--text-meta)] uppercase tracking-wider ${
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
      <ToolCardIcon tone={toneForCategory(badge)}>
        {icon}
      </ToolCardIcon>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-sm text-text-primary truncate">{name}</span>
          <span className="text-[length:var(--text-meta)] uppercase tracking-wider text-text-muted border border-border-primary rounded px-1.5 py-0.5 shrink-0">
            {badge}
          </span>
        </div>
        <p className="text-xs text-text-muted mt-0.5 truncate">{description}</p>
      </div>
      {rightIcon && <span className="shrink-0 text-text-faint">{rightIcon}</span>}
      <ToggleSwitch enabled={enabled} onToggle={onToggle} disabled={saving || disabled} offLabel="禁用" onLabel="启用" className="shrink-0" />
    </div>
  );
}
