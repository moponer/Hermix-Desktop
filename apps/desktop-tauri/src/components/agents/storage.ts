/* ════════════════════════════════════════════════════════════════
   Agents storage helpers — async API with localStorage fallback
   Used by AgentsView, AIGenerateWizard, TestDialog.
   ════════════════════════════════════════════════════════════════ */

import { apiUrl } from "../../api";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import type { AgentPersona } from "./types";
import { pickColor } from "./types";

export const STORAGE_KEY = STORAGE_KEYS.agents;

/** Load agents from the backend API, falling back to localStorage
 *  if the server is unreachable (e.g. running in standalone mode). */
export async function loadAgentsAsync(): Promise<AgentPersona[]> {
  try {
    const resp = await fetch(apiUrl("/api/agents/custom"));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json() as { agents: AgentPersona[] };
    if (!data.agents || !Array.isArray(data.agents)) return [];
    // Migrate old records to new shape
    return data.agents.map((a) => ({
      ...a,
      colorTheme: a.colorTheme || pickColor(a.name || a.id),
      usageCount: a.usageCount ?? 0,
      lastUsedAt: a.lastUsedAt ?? a.updatedAt ?? a.createdAt ?? Date.now(),
    }));
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
    const parsed = JSON.parse(raw) as AgentPersona[];
    return parsed.map((a) => ({
      ...a,
      colorTheme: a.colorTheme || pickColor(a.name || a.id),
      usageCount: a.usageCount ?? 0,
      lastUsedAt: a.lastUsedAt ?? a.updatedAt ?? a.createdAt ?? Date.now(),
    }));
  } catch {
    return [];
  }
}

/** Save a single agent to the backend (upsert). Falls back to
 *  localStorage batch-save if the server is unreachable. */
export async function saveAgentAsync(agent: AgentPersona): Promise<void> {
  try {
    const resp = await fetch(apiUrl("/api/agents/custom"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(agent),
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  } catch {
    // Fallback: merge into the localStorage cache (don't clobber other agents)
    upsertAgentLocal(agent);
  }
}

/** Delete a single agent from the backend. Falls back to
 *  localStorage batch-save if the server is unreachable. */
export async function deleteAgentAsync(id: string, remaining: AgentPersona[]): Promise<void> {
  try {
    const resp = await fetch(apiUrl(`/api/agents/custom/${encodeURIComponent(id)}`), {
      method: "DELETE",
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
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