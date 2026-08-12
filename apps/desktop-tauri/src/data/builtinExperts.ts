// Built-in expert catalog (front-end bundle).
//
// 数据源: 后端 `/api/experts` 返回完整目录 (useExpert.ts 优先用
// remote, 本文件仅作首帧兜底)。字段与 RemoteExpert → ExpertDef
// 映射 (useExpert.ts remoteToExpertDef) 一一对应。

import { registerExperts, type RemoteExpert } from "../api";

/** Expert category label. */
export type ExpertCategory = string;

/** A built-in or remote expert definition. */
export interface ExpertDef {
  id: string;
  name: string;
  avatar: string;
  role: string;
  category: ExpertCategory;
  description: string;
  systemPrompt: string;
  toolset?: string;
  isBuiltin: boolean;
}

/** Ordered category list for the sidebar filter. */
export const EXPERT_CATEGORIES: ExpertCategory[] = [];

/** Locally-bundled experts (first-paint fallback before remote fetch). */
export const BUILTIN_EXPERTS: ExpertDef[] = [];

/** Look up a built-in expert by id; null when absent. */
export function getBuiltinExpert(id: string): ExpertDef | null {
  return BUILTIN_EXPERTS.find((e) => e.id === id) ?? null;
}

/** Best-effort register of the local catalog into the backend ExpertRegistry. */
export async function registerBuiltinsToBackend(): Promise<unknown> {
  const remote: RemoteExpert[] = BUILTIN_EXPERTS.map((e) => ({
    id: e.id,
    name: e.name,
    avatar: e.avatar,
    role: e.role,
    category: e.category,
    description: e.description,
    system_prompt: e.systemPrompt,
    toolset: e.toolset ?? null,
    is_builtin: e.isBuiltin,
  }));
  if (remote.length === 0) {
    return { inserted: 0, total: 0 };
  }
  return registerExperts(remote);
}
