// Shared parser for the Hermix `memory` tool's structured JSON result.
//
// The memory backend (crates/hermix-tools/src/backends/memory.rs)
// returns a JSON envelope on EVERY outcome:
//
//   { success, status, action, target, path, message, entries, entry_count, ... }
//
// `status` is one of:
//   - "success"   — entry written / replaced / removed / read OK
//   - "not_found" — remove/replace couldn't locate old_text (NOT a failure)
//   - "ambiguous" — remove/replace matched multiple entries (be more specific)
//   - "error"     — genuine execution error (disk write failed, lock, etc.)
//
// The chat UI renders these as a single row: <动作> <记忆文件路径> <状态>,
// and the path is clickable to open a preview. no-match / ambiguous are
// soft outcomes (returned as Ok, never as a tool error) so they must NOT
// show a red "失败" badge — they render "未查到对应内容" / "匹配到多条".
//
// Both the inline `ToolMessageCard` (MessageBubble.tsx) and the ACUI
// timeline `ToolEventCard` (ToolEventCard.tsx) import from here so the
// two renderers stay in sync.

export type MemoryAction = "add" | "replace" | "remove" | "read";
export type MemoryStatus = "success" | "not_found" | "ambiguous" | "error";

export interface MemoryToolResult {
  success: boolean;
  status: MemoryStatus;
  action: MemoryAction;
  target: "memory" | "user";
  path: string;
  message: string;
  entry_count: number;
}

/**
 * Parse a memory tool result from a raw tool-output string.
 * Returns null when the output isn't a memory envelope (so callers
 * can fall back to generic tool rendering). Tolerates the case where
 * a threat-scan JSON is appended on a second line (success path) by
 * parsing only the first line.
 */
export function parseMemoryResult(output?: string | null): MemoryToolResult | null {
  if (!output) return null;
  const trimmed = output.trim();
  if (!trimmed.startsWith("{")) return null;

  const tryParse = (s: string): MemoryToolResult | null => {
    try {
      const p = JSON.parse(s) as Record<string, unknown>;
      if (
        p &&
        typeof p === "object" &&
        typeof p.status === "string" &&
        typeof p.action === "string" &&
        typeof p.path === "string"
      ) {
        return {
          success: p.success === true,
          status: p.status as MemoryStatus,
          action: p.action as MemoryAction,
          target: p.target === "user" ? "user" : "memory",
          path: p.path as string,
          message: typeof p.message === "string" ? p.message : "",
          entry_count: typeof p.entry_count === "number" ? p.entry_count : 0,
        };
      }
    } catch {
      /* not JSON — fall through */
    }
    return null;
  };

  return tryParse(trimmed) ?? tryParse(trimmed.split(/\r?\n/)[0] ?? "");
}

/** Chinese action label shown as the verb slot of the card row. */
export function memoryActionLabel(action?: string): string {
  switch (action) {
    case "add":
      return "编辑记忆";
    case "replace":
      return "更新记忆";
    case "remove":
      return "删除记忆";
    case "read":
      return "查找记忆";
    default:
      return "记忆";
  }
}

/** One-line status label for the card's right-side badge. */
export function memoryStatusLabel(status?: string): string {
  switch (status) {
    case "success":
      return "成功";
    case "not_found":
      return "未查到对应内容";
    case "ambiguous":
      return "匹配到多条";
    case "error":
      return "失败";
    default:
      return "已完成";
  }
}

/** Tailwind text-color class for the status badge (no blue/purple —
 *  per the project's gray/zinc + amber/red palette). */
export function memoryStatusColor(status?: string): string {
  switch (status) {
    case "success":
      return "text-emerald-400";
    case "not_found":
      return "text-zinc-400";
    case "ambiguous":
      return "text-amber-400";
    case "error":
      return "text-red-400";
    default:
      return "text-zinc-400";
  }
}
