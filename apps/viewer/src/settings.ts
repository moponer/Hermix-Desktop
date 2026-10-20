// 查看器设置：与 Rust 侧 `src-tauri/src/settings.rs` 一一对应（camelCase 键名）。
//
// 设置存在 `$hermix_home/viewer-settings.json`；**API Key 不在这里**——它写进
// 同一个密钥库 `.env`（主程序也用它），设置里只有「配没配」的标记。
// 理由：配置目录会被备份/同步/贴进工单，密钥不该跟着走。

import { invoke } from "@tauri-apps/api/core";

export type SearchMode = "live" | "enter";
export type SearchScope = "auto" | "global" | "local";
export type AiMode = "inherit" | "custom";

export interface SearchSettings {
  /** live = 输入即搜；enter = 回车才搜。 */
  mode: SearchMode;
  /** live 模式的去抖毫秒数。 */
  debounceMs: number;
  /** auto = 全盘优先、失败退回当前目录；global = 只全盘；local = 只当前目录。 */
  scope: SearchScope;
  /** 最多返回条数。 */
  limit: number;
  /** 搜索时是否包含隐藏文件。 */
  includeHidden: boolean;
}

export interface AiSettings {
  /** inherit = 继承主程序（读主程序的 config.yaml）；custom = 独立模型。 */
  mode: AiMode;
  /** 独立模式的端点（OpenAI 兼容，含 /v1）。 */
  baseUrl: string;
  /** 独立模式的模型 id。 */
  model: string;
  /** 追加给模型的系统提示。 */
  systemPrompt: string;
  temperature: number | null;
  maxTokens: number | null;
  /** 独立模式的密钥是否已配置（在 .env 里）——不存明文。 */
  hasKey: boolean;
}

export interface McpSettings {
  /** 启动后是否拉起本地 MCP 服务。 */
  enabled: boolean;
  /** 本地 MCP 服务的端口（只监听回环）。 */
  port: number;
  /** 是否允许非本机连接（默认否——开它等于把文件操作能力开到网上）。 */
  allowRemote: boolean;
}

export interface GeneralSettings {
  /** 启动时进的目录（空 = 上次目录 → 主目录 → 首个盘符）。 */
  defaultDir: string;
  showHiddenByDefault: boolean;
  confirmCloseTab: boolean;
}

export interface ViewerSettings {
  search: SearchSettings;
  ai: AiSettings;
  mcp: McpSettings;
  general: GeneralSettings;
}

/** 后端实际生效的 AI 端点（设置界面显示「现在到底在用什么」）。 */
export interface AiStatus {
  source: string;
  provider: string;
  baseUrl: string;
  model: string;
  hasKey: boolean;
  ready: boolean;
  problem: string | null;
}

export const DEFAULT_SETTINGS: ViewerSettings = {
  search: {
    mode: "live",
    debounceMs: 250,
    scope: "auto",
    limit: 300,
    includeHidden: false,
  },
  ai: {
    mode: "inherit",
    baseUrl: "",
    model: "",
    systemPrompt:
      "你是 Hermix 文件查看器的助手，基于用户当前打开的文件回答问题。回答用中文，简洁准确；涉及文件内容时以实际读取到的内容为准。",
    temperature: 0.3,
    maxTokens: 4096,
    hasKey: false,
  },
  mcp: { enabled: false, port: 3911, allowRemote: false },
  general: {
    defaultDir: "",
    showHiddenByDefault: false,
    confirmCloseTab: false,
  },
};

export async function loadSettings(): Promise<ViewerSettings> {
  try {
    const s = await invoke<ViewerSettings>("viewer_get_settings");
    return { ...DEFAULT_SETTINGS, ...s, search: { ...DEFAULT_SETTINGS.search, ...s.search }, ai: { ...DEFAULT_SETTINGS.ai, ...s.ai }, mcp: { ...DEFAULT_SETTINGS.mcp, ...s.mcp }, general: { ...DEFAULT_SETTINGS.general, ...s.general } };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export async function saveSettings(s: ViewerSettings): Promise<ViewerSettings> {
  return invoke<ViewerSettings>("viewer_set_settings", { settings: s });
}

/** 写 / 清独立模型的 API Key（落 .env）。传 null 或空串 = 清除。 */
export async function setAiKey(key: string | null): Promise<boolean> {
  return invoke<boolean>("viewer_set_ai_key", { key: key?.trim() || null });
}

export async function loadAiStatus(): Promise<AiStatus | null> {
  try {
    return await invoke<AiStatus>("viewer_ai_status");
  } catch {
    return null;
  }
}

/** 本地 MCP 端点的状态。 */
export interface McpStatus {
  running: boolean;
  endpoint: string | null;
  port: number | null;
}

export async function loadMcpStatus(): Promise<McpStatus | null> {
  try {
    return await invoke<McpStatus>("viewer_mcp_status");
  } catch {
    return null;
  }
}

export async function startMcp(): Promise<McpStatus> {
  return invoke<McpStatus>("viewer_mcp_start");
}

export async function stopMcp(): Promise<McpStatus> {
  return invoke<McpStatus>("viewer_mcp_stop");
}

/** 把 MCP 客户端配置拼成可直接粘贴的 JSON（各客户端的 mcpServers 形状一致）。 */
export function mcpClientConfig(port: number): string {
  return JSON.stringify(
    {
      mcpServers: {
        "hermix-viewer": {
          type: "http",
          url: `http://127.0.0.1:${port}/mcp`,
        },
      },
    },
    null,
    2,
  );
}
