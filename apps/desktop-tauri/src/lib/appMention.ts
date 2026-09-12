// mention — parse, highlight and serialise @app / @work mentions.
//
// A mention is a literal trigger token ("@app" / "@work") followed by a single
// (whitespace-delimited) item name, e.g. "@app 本地音乐" or "@work 车辆台账".
// The name must match a known custom app / workspace so we can resolve it to a
// stable id at send time.

/** 自定义应用触发词。 */
export const MENTION_TRIGGER_APP = "@app";
/** 工作台触发词。 */
export const MENTION_TRIGGER_WORK = "@work";

// Dark-blue (still legible on both light and dark themes — a true navy would
// be invisible on the dark composer background). Swap for a deeper navy if the
// app is ever forced to a light-only theme.
export const MENTION_COLOR = "#3b82f6";
// 工作台 mention 高亮色 — 与 @app 区分，用项目暖 accent（陶土棕）。
export const MENTION_COLOR_WORK = "#c96442";

export interface ActiveMention {
  /** index of the '@' in the trigger */
  start: number;
  /** caret position (end of the currently-typed query) */
  caret: number;
  /** text typed after "<trigger> " used to filter the item list */
  query: string;
}

/** A mention source item — anything with a display name and a stable id. */
export interface MentionItem {
  name: string;
  id: string;
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Matches a *resolved* mention anywhere in the text: "<trigger> <name>" where
// name is a non-space, non-@ token. Used both for highlighting and for
// send-time id resolution.
function resolvedRe(trigger: string): RegExp {
  return new RegExp(escapeRe(trigger) + "\\s+([^\\s@]+)", "g");
}

/**
 * Detect an *active* (unresolved) mention near the caret — i.e. the user just
 * typed "<trigger>" (optionally followed by a filter query) and hasn't
 * committed it yet. Returns null once the mention is "closed" (a trailing
 * space after a name or a newline), so the popup stays open only while the
 * user is mid-typing.
 */
export function detectActiveMention(
  text: string,
  caret: number,
  trigger: string,
): ActiveMention | null {
  const triggerLen = trigger.length;
  // Latest "<trigger>" occurrence whose end does not exceed the caret.
  let idx = text.lastIndexOf(trigger, caret);
  while (idx !== -1 && idx + triggerLen > caret) {
    idx = text.lastIndexOf(trigger, idx - 1);
  }
  if (idx === -1) return null;
  // Trigger must start the string or be preceded by whitespace.
  const before = idx === 0 ? "" : text[idx - 1];
  if (before && !/\s/.test(before)) return null;
  const after = text.slice(idx + triggerLen, caret);
  // A newline means the mention was abandoned on a previous line.
  if (after.includes("\n")) return null;
  // accept "" (just typed trigger) or " <query>"; a trailing space closes it.
  const m = after.match(/^\s*([^\s]*)$/);
  if (!m) return null;
  return { start: idx, caret, query: m[1] };
}

/**
 * Replace the active mention span (from its start to the caret) with a
 * resolved "<trigger> <name> " token (trailing space so the user keeps typing
 * naturally).
 */
export function applyMention(
  text: string,
  active: ActiveMention,
  trigger: string,
  name: string,
): { text: string; caret: number } {
  const insert = `${trigger} ${name} `;
  const next = text.slice(0, active.start) + insert + text.slice(active.caret);
  return { text: next, caret: active.start + insert.length };
}

export interface ResolvedMention {
  name: string;
  id: string;
}

/** Resolve every "<trigger> <name>" in the text to its item id (deduped by id). */
export function extractMentions(
  text: string,
  items: MentionItem[],
  trigger: string,
): ResolvedMention[] {
  const byName = new Map(items.map((a) => [a.name, a]));
  const found: ResolvedMention[] = [];
  const seen = new Set<string>();
  const re = resolvedRe(trigger);
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const item = byName.get(m[1]);
    if (item && !seen.has(item.id)) {
      seen.add(item.id);
      found.push({ name: m[1], id: item.id });
    }
  }
  return found;
}

/**
 * Prepend a structured target header carrying each mentioned item's id so the
 * LLM unambiguously knows which custom app / workspace the user is referring
 * to. When no mention is present the text is returned unchanged (ongoing
 * conversations that already resolved the target need no extra hint).
 *
 * @param label   展示标签，如 "自定义应用" / "工作台"
 * @param idKey   注入的 id 字段名，如 "app_id" / "workspace_id"
 */
export function buildMentionPrefix(
  text: string,
  items: MentionItem[],
  trigger: string,
  label: string,
  idKey: string,
): string {
  const mentions = extractMentions(text, items, trigger);
  if (mentions.length === 0) return text;
  const lines = mentions.map((mt) => `- ${mt.name}（${idKey}: ${mt.id}）`);
  const header = `【目标${label}】\n${lines.join("\n")}`;
  return `${header}\n\n${text}`;
}

/** Split text into plain + mention segments for the highlight overlay. */
export function splitMentions(
  text: string,
  triggers: string[],
): Array<{ text: string; mention: boolean; trigger?: string }> {
  const segments: Array<{ text: string; mention: boolean; trigger?: string }> = [];
  const re = new RegExp(
    triggers.map((t) => `(${escapeRe(t)}\\s+[^\\s@]+)`).join("|"),
    "g",
  );
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) segments.push({ text: text.slice(last, m.index), mention: false });
    const matched = m[0];
    const trigger = triggers.find((t) => matched.startsWith(t));
    segments.push({ text: matched, mention: true, trigger });
    last = m.index + matched.length;
  }
  if (last < text.length) segments.push({ text: text.slice(last), mention: false });
  return segments;
}
