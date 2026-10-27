// ConnectorsTab.tsx —— 连接器 Tab：用户配置的 MCP servers（R2-4 拆分第四步）。
import { useEffect, useMemo, useState } from "react";
import { Plus, Trash2, Network } from "lucide-react";
import * as api from "../../api";
import { parseMcpJson } from "./toolTabsData";
import { McpCatalogGrid } from "./McpCatalogGrid";
import { InitialIcon } from "../ui/InitialIcon";
import { ToolListRow } from "./ToolListRow";
import { ToggleSwitch } from "../ToggleSwitch";
import { ConfirmDialog, Input, SelectField, TextArea } from "../ui";
import { DELETE } from "../strings";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { SearchBar, EmptyHint, SkeletonGrid, SummaryBar } from "./toolTabsShared";

// ── Connectors tab: user-configured MCP servers ──────────────────────

export function ConnectorsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [servers, setServers] = useState<api.McpServerEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [showAddForm, setShowAddForm] = useState(false);
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  const [addMode, setAddMode] = useState<"paste" | "manual">("paste");
  const [confirmReset, setConfirmReset] = useState(false);
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

  /** 恢复官方清单：POST /api/mcp/servers/reset。后端丢弃当前清单、
   *  重新播种内置官方服务；成功后重拉列表。 */
  const doReset = async () => {
    try {
      await api.fetchJSON("/api/mcp/servers/reset", { method: "POST" });
      toastSuccess("已恢复官方 MCP 清单");
      setServers(await api.getMcpServers());
    } catch (e) {
      toastError(`恢复失败：${e instanceof Error ? e.message : String(e)}`);
    }
  };

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
          <button
            onClick={() => setConfirmReset(true)}
            disabled={servers.length === 0}
            className="flex items-center gap-1 px-2 py-1 rounded-md text-[length:var(--text-meta)] text-text-secondary hover:text-text-error hover:bg-error-soft disabled:opacity-40"
            title="恢复为内置官方 MCP 服务清单（丢弃全部自定义条目）"
          >
            恢复官方清单
          </button>
        </div>
      </div>

      <ConfirmDialog
        open={confirmReset}
        title="恢复官方 MCP 清单"
        body={`将删除全部 ${servers.length} 个已配置的 MCP 服务（含自定义条目），恢复为内置官方清单。`}
        confirmLabel="恢复官方清单"
        onCancel={() => setConfirmReset(false)}
        onConfirm={() => {
          setConfirmReset(false);
          void doReset();
        }}
      />

      {showAddForm && (
        <div className="rounded-xl bg-bg-card p-4 mb-3 space-y-3">
          <div className="text-sm font-medium text-text-primary">添加 MCP 服务</div>
          {addError && (
            <div className="text-[length:var(--text-meta)] text-text-error bg-error/10 rounded px-2 py-1">{addError}</div>
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
              <TextArea
                label="粘贴配置（Claude Desktop / Cursor 风格 mcpServers JSON）"
                hint="支持 mcpServers 对象或单条 server 配置，自动识别 stdio / http。"
                value={pasteJson}
                onChange={(e) => setPasteJson(e.target.value)}
                rows={7}
                placeholder={'{"mcpServers":{"my-fs":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/tmp"]},"remote":{"url":"https://example.com/mcp"}}}'}
                className="font-mono"
              />
              <div className="flex items-center gap-2">
                <button onClick={handleImportPaste} disabled={adding} className="px-3 py-1.5 rounded-lg bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] text-[length:var(--text-meta)] font-medium hover:opacity-90 disabled:opacity-50">导入</button>
                <button onClick={() => { setShowAddForm(false); setAddError(null); }} className="px-3 py-1.5 rounded-lg text-[length:var(--text-meta)] text-text-muted hover:text-text-primary hover:bg-bg-tertiary">取消</button>
              </div>
            </>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-3">
                <Input
                  size="md"
                  label="名称"
                  value={newServer.name}
                  onChange={(e) => setNewServer((p) => ({ ...p, name: e.target.value }))}
                  placeholder="my-mcp-server"
                />
                <SelectField
                  size="md"
                  label="传输方式"
                  value={newServer.transport}
                  onChange={(e) => setNewServer((p) => ({ ...p, transport: e.target.value as api.McpTransport }))}
                  options={[
                    { value: "stdio", label: "stdio" },
                    { value: "http", label: "http" },
                  ]}
                />
                {newServer.transport === "stdio" ? (
                  <>
                    <Input
                      size="md"
                      label="Command"
                      value={newServer.command}
                      onChange={(e) => setNewServer((p) => ({ ...p, command: e.target.value }))}
                      placeholder="npx"
                    />
                    <Input
                      size="md"
                      label="Args （空格分隔）"
                      value={newServer.args}
                      onChange={(e) => setNewServer((p) => ({ ...p, args: e.target.value }))}
                      placeholder="-y @modelcontextprotocol/server-filesystem /tmp"
                    />
                  </>
                ) : (
                  <div className="col-span-2">
                    <Input
                      size="md"
                      label="URL"
                      value={newServer.url}
                      onChange={(e) => setNewServer((p) => ({ ...p, url: e.target.value }))}
                      placeholder="https://example.com/mcp"
                    />
                  </div>
                )}
                {newServer.transport === "http" && (
                  <div className="col-span-2">
                    <Input
                      size="md"
                      label="Bearer Token （可选，远程服务鉴权）"
                      value={newServer.token}
                      onChange={(e) => setNewServer((p) => ({ ...p, token: e.target.value }))}
                      placeholder="homk_…"
                    />
                  </div>
                )}
                <div className="col-span-2">
                  <TextArea
                    label="环境变量 (每行 KEY=value)"
                    value={newServer.env}
                    onChange={(e) => setNewServer((p) => ({ ...p, env: e.target.value }))}
                    placeholder={"API_KEY=xxx"}
                    rows={2}
                  />
                </div>
                <div className="col-span-2">
                  <Input
                    size="md"
                    label="描述 （可选）"
                    value={newServer.description}
                    onChange={(e) => setNewServer((p) => ({ ...p, description: e.target.value }))}
                    placeholder="自定义文件系统 MCP"
                  />
                </div>
              </div>
              <div className="flex items-center gap-2">
                <button onClick={handleAdd} disabled={adding} className="px-3 py-1.5 rounded-lg bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] text-[length:var(--text-meta)] font-medium hover:opacity-90 disabled:opacity-50">
                  {adding ? "添加中…" : "确认添加"}
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
  return (
    <ToolListRow
      dimmed={!server.enabled}
      icon={<InitialIcon name={server.name} />}
      title={server.name}
      meta={server.transport}
      /* 只留一句能分辨 server 的信息：描述优先，没有就退回命令行。
         旧版描述与命令行上下叠两行，行高被撑到 80px，列表一长就散。 */
      description={server.description || (server.transport === "stdio"
        ? `${server.command ?? "?"} ${(server.args ?? []).join(" ")}`
        : server.url ?? "(no url)")}
      action={
        <>
          <ToggleSwitch
            enabled={server.enabled}
            onToggle={() => onToggle(!server.enabled)}
            offLabel="停用"
            onLabel="启用"
          />
          <button
            onClick={onRemove}
            className="flex h-7 w-7 items-center justify-center rounded-[var(--radius-sm)] text-[var(--color-text-muted)] hover:bg-[var(--color-error-soft)] hover:text-[var(--color-text-error)]"
            title={DELETE}
            aria-label={DELETE}>
            <Trash2 className="icon-md" />
          </button>
        </>
      }
    />
  );
}
