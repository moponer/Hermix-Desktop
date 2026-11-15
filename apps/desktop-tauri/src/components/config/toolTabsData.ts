// toolTabsData.ts —— ToolTabs 四个 Tab 的常量与纯函数（R2-4，2026-10-16）。
// 从 54KB 的 ToolTabs.tsx 抽出：无 React 依赖，可独立测试。
import type { LucideIcon } from "lucide-react";
import {
  FileCode, Terminal, Globe, Database, Cpu, Boxes, Layers, Plug, Network,
} from "lucide-react";
import * as api from "../../api";
import type { PluginSettings } from "../../api";

// Plugin toggles map to the backend's DashboardPluginSettings (4 hard slots).
// The icon is purely cosmetic — the source of truth is the boolean on the server.
// `mcp_browser` 不在其中：浏览器工具链已随 hermix-cua 一起移除（c6442032）,
// 留着这一行会让开关拨了不持久化（PUT 被 serde 丢掉，GET 不回该字段）。
export const BUILTIN_PLUGINS: Array<{
  id: string;
  key: keyof PluginSettings;
  name: string;
  description: string;
  icon: LucideIcon;
  category: "MCP" | "Tools";
}> = [
  { id: "mcp-filesystem", key: "mcp_filesystem", name: "文件系统", description: "读写本地文件和文件夹", icon: FileCode, category: "MCP" },
  { id: "mcp-terminal", key: "mcp_terminal", name: "终端", description: "执行 Shell 命令", icon: Terminal, category: "MCP" },
  { id: "mcp-database", key: "mcp_database", name: "数据库", description: "执行 SQLite 和 PostgreSQL 查询", icon: Database, category: "MCP" },
  { id: "tool-code-exec", key: "tool_code_exec", name: "代码执行", description: "在沙箱中运行代码", icon: Cpu, category: "Tools" },
];

export const DEFAULT_PLUGIN_SETTINGS: PluginSettings = {
  mcp_filesystem: true,
  mcp_terminal: true,
  mcp_database: false,
  tool_code_exec: true,
};

export const CATEGORY_LABELS: Record<string, string> = {
  "software-development": "软件开发",
  devops: "运维",
  productivity: "效率",
  finance: "金融",
  lifestyle: "生活",
  writing: "写作",
  office: "Office",
};

export type McpSpec = Omit<api.McpServerEntry, "id" | "createdAt">;

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
    // Claude/Cursor 风格的 headers.Authorization: "Bearer xxx" → token 字段。
    // 凭据不该散在 headers 里由前端透传，统一收敛到 token。
    let token = typeof c.token === "string" ? c.token : undefined;
    const headers = (typeof c.headers === "object" && c.headers ? c.headers : {}) as Record<string, unknown>;
    const authHeader = typeof headers.Authorization === "string" ? headers.Authorization : "";
    if (!token && authHeader.startsWith("Bearer ")) {
      token = authHeader.slice("Bearer ".length).trim() || undefined;
    }
    const base = {
      name,
      enabled: true,
      description: typeof c.description === "string" ? c.description : undefined,
      env: typeof c.env === "object" && c.env ? (c.env as Record<string, string>) : undefined,
    };
    if (typeof c.url === "string" && c.url) {
      specs.push({ ...base, transport: "http", url: c.url, token });
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

// 用一个稳定的字符串 hash 选择图标，保证同一名字总是同图标。
export const CONNECTOR_ICONS: LucideIcon[] = [Network, Globe, Database, Cpu, Boxes, FileCode, Layers, Plug];
export function pickIconByName(name: string): LucideIcon {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) | 0;
  const idx = Math.abs(h) % CONNECTOR_ICONS.length;
  return CONNECTOR_ICONS[idx];
}
