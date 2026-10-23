/* ════════════════════════════════════════════════════════════════
   Agents storage helpers — async API with localStorage fallback
   Used by AgentsView, AIGenerateWizard, TestDialog.
   ════════════════════════════════════════════════════════════════ */

import { fetchJSON } from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import type { AgentPersona } from "./types";
import { pickColor } from "./types";

export const STORAGE_KEY = STORAGE_KEYS.agents;

/* ── Wire mapping ────────────────────────────────────────────────
   后端 `dashboard/agents.rs` 的 AgentPersonaJson/SaveAgentRequest 是
   snake_case 且无 deny_unknown_fields：前端直发 camelCase 会被静默
   忽略、字段走 default 蒸发。决策（2026-10-18）：前端迁就后端，
   在此做双向映射，wire 格式只此一处。 */
interface WireAgent {
  id: string;
  name: string;
  avatar: string;
  role: string;
  identity: string;
  soul: string;
  skills: string[];
  specialty: string;
  dialog_style: string;
  background: string;
  personality: string[];
  system_prompt: string;
  model?: string;
  color_theme: string;
  usage_count: number;
  last_used_at: number;
  created_at: number;
  updated_at: number;
  sample_dialog?: { q: string; a: string };
}

export function toWireAgent(a: AgentPersona): WireAgent {
  return {
    id: a.id,
    name: a.name,
    avatar: a.avatar,
    role: a.role,
    identity: a.identity,
    soul: a.soul,
    skills: a.skills,
    specialty: a.specialty,
    dialog_style: a.dialogStyle,
    background: a.background,
    personality: a.personality,
    system_prompt: a.systemPrompt,
    model: a.model,
    color_theme: a.colorTheme,
    usage_count: a.usageCount,
    last_used_at: a.lastUsedAt,
    created_at: a.createdAt,
    updated_at: a.updatedAt,
    sample_dialog: a.sampleDialog,
  };
}

/** Accept both wire shapes (snake_case from backend, camelCase from
 *  legacy localStorage records) so old caches keep loading. */
export function fromWireAgent(raw: Record<string, unknown>): AgentPersona {
  const a = raw as Partial<AgentPersona> & Partial<WireAgent>;
  return {
    id: String(a.id ?? ""),
    name: String(a.name ?? ""),
    /* 头像已改由姓名首字派生，不再需要 emoji 兜底（旧数据里的 emoji 原样保留读取）。 */
    avatar: String(a.avatar ?? ""),
    role: String(a.role ?? ""),
    identity: String(a.identity ?? ""),
    soul: String(a.soul ?? ""),
    skills: a.skills ?? [],
    specialty: String(a.specialty ?? ""),
    dialogStyle: String(a.dialogStyle ?? a.dialog_style ?? ""),
    background: String(a.background ?? ""),
    personality: a.personality ?? [],
    systemPrompt: String(a.systemPrompt ?? a.system_prompt ?? ""),
    model: a.model ?? undefined,
    colorTheme: (a.colorTheme ?? a.color_theme ?? pickColor(String(a.name ?? a.id ?? ""))) as AgentPersona["colorTheme"],
    usageCount: Number(a.usageCount ?? a.usage_count ?? 0),
    lastUsedAt: Number(a.lastUsedAt ?? a.last_used_at ?? a.updatedAt ?? a.updated_at ?? Date.now()),
    createdAt: Number(a.createdAt ?? a.created_at ?? Date.now()),
    updatedAt: Number(a.updatedAt ?? a.updated_at ?? Date.now()),
    sampleDialog: a.sampleDialog ?? a.sample_dialog ?? undefined,
  };
}

/** Load agents from the backend API, falling back to localStorage
 *  if the server is unreachable (e.g. running in standalone mode).
 *  The server call goes through the stdio bridge's `http` channel —
 *  same axum route, just inside a JSON Lines envelope. */
export async function loadAgentsAsync(): Promise<AgentPersona[]> {
  try {
    const data = await fetchJSON<{ agents: Array<Record<string, unknown>> }>("/api/agents/custom");
    if (!data.agents || !Array.isArray(data.agents)) return [];
    // Migrate old records to new shape
    return data.agents.map((a) => {
      const persona = fromWireAgent(a);
      return {
        ...persona,
        colorTheme: persona.colorTheme || pickColor(persona.name || persona.id),
      };
    });
  } catch {
    // Fallback to localStorage
    return loadAgentsLocal();
  }
}

/** Synchronous localStorage-only loader (used as fallback and for initial state). */
export function loadAgentsLocal(): AgentPersona[] {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as Array<Record<string, unknown>>;
    return parsed.map((a) => {
      const persona = fromWireAgent(a);
      return {
        ...persona,
        colorTheme: persona.colorTheme || pickColor(persona.name || persona.id),
      };
    });
  } catch {
    return [];
  }
}

/** Save a single agent to the backend (upsert). Falls back to
 *  localStorage batch-save if the server is unreachable. */
export async function saveAgentAsync(agent: AgentPersona): Promise<void> {
  try {
    await fetchJSON("/api/agents/custom", {
      method: "POST",
      body: JSON.stringify(toWireAgent(agent)),
    });
  } catch {
    // Fallback: merge into the localStorage cache (don't clobber other agents)
    upsertAgentLocal(agent);
  }
}

/** Delete a single agent from the backend. Falls back to
 *  localStorage batch-save if the server is unreachable. */
export async function deleteAgentAsync(id: string, remaining: AgentPersona[]): Promise<void> {
  try {
    await fetchJSON(`/api/agents/custom/${encodeURIComponent(id)}`, { method: "DELETE" });
  } catch {
    saveAgentsLocal(remaining);
  }
}

/** Batch-save to localStorage (fallback path). */
export function saveAgentsLocal(agents: AgentPersona[]) {
  try { storage.setItem(STORAGE_KEY, JSON.stringify(agents)); } catch {}
}

/** Upsert a single agent into the localStorage fallback cache without
 *  clobbering the rest of the list (the wrong `saveAgentsLocal([agent])`
 *  would overwrite the whole cache with one entry on backend-down). */
export function upsertAgentLocal(agent: AgentPersona) {
  try {
    const existing = loadAgentsLocal();
    const idx = existing.findIndex((a) => a.id === agent.id);
    if (idx >= 0) existing[idx] = agent;
    else existing.push(agent);
    storage.setItem(STORAGE_KEY, JSON.stringify(existing));
  } catch {}
}

export function buildSystemPrompt(agent: Partial<AgentPersona>): string {
  const parts: string[] = [];
  if (agent.name) parts.push(`你叫「${agent.name}」`);
  if (agent.role) parts.push(`角色:${agent.role}`);
  if (agent.identity) parts.push(`身份定位:${agent.identity}`);
  if (agent.soul) parts.push(`核心理念:${agent.soul}`);
  if (agent.skills?.length) parts.push(`擅长:${agent.skills.join("、")}`);
  if (agent.specialty) parts.push(`专长领域:${agent.specialty}`);
  if (agent.dialogStyle) parts.push(`对话风格:${agent.dialogStyle}`);
  if (agent.background) parts.push(`背景:${agent.background}`);
  if (agent.personality?.length) parts.push(`性格特点:${agent.personality.join("、")}`);
  return parts.join("\n");
}