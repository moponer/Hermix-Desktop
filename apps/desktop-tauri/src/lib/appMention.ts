// appMention — parse, highlight and serialise @app mentions.
//
// A mention is the literal token "@app" followed by a single (whitespace-
// delimited) app name, e.g. "@app 本地音乐". The name must match a known
// custom app so we can resolve it to a stable id at send time.

import type { CustomAppSummary } from "./customAppApi";

export const MENTION_TRIGGER = "@app";

// Dark-blue (still legible on both light and dark themes — a true navy would
// be invisible on the dark composer background). Swap for a deeper navy if the
// app is ever forced to a light-only theme.
export const MENTION_COLOR = "#3b82f6";

export interface ActiveMention {
  /** index of the '@' in "@app" */
  start: number;
  /** caret position (end of the currently-typed query) */
  caret: number;
  /** text typed after "@app " used to filter the app list */
  query: string;
}

// Matches a *resolved* mention anywhere in the text: "@app <name>" where name
// is a non-space, non-@ token. Used both for highlighting and for send-time
// id resolution.
const RESOLVED_RE = /@app\s+([^\s@]+)/g;

/**
 * Detect an *active* (unresolved) mention near the caret — i.e. the user just
 * typed "@app" (optionally followed by a filter query) and hasn't committed it
 * yet. Returns null once the mention is "closed" (a trailing space after a name
 * or a newline), so the popup stays open only while the user is mid-typing.
 */
export function detectActiveMention(text: string, caret: number): ActiveMention | null {
  const triggerLen = MENTION_TRIGGER.length;
  // Latest "@app" occurrence whose end does not exceed the caret.
  let idx = text.lastIndexOf(MENTION_TRIGGER, caret);
  while (idx !== -1 && idx + triggerLen > caret) {
    idx = text.lastIndexOf(MENTION_TRIGGER, idx - 1);
  }
  if (idx === -1) return null;
  // Trigger must start the string or be preceded by whitespace.
  const before = idx === 0 ? "" : text[idx - 1];
  if (before && !/\s/.test(before)) return null;
  const after = text.slice(idx + triggerLen, caret);
  // A newline means the mention was abandoned on a previous line.
  if (after.includes("\n")) return null;
  // accept "" (just typed @app) or " <query>"; a trailing space closes it.
  const m = after.match(/^\s*([^\s]*)$/);
  if (!m) return null;
  return { start: idx, caret, query: m[1] };
}

/**
 * Replace the active mention span (from its start to the caret) with a resolved
 * "@app <name> " token (trailing space so the user keeps typing naturally).
 */
export function applyMention(
  text: string,
  active: ActiveMention,
  appName: string,
): { text: string; caret: number } {
  const insert = `${MENTION_TRIGGER} ${appName} `;
  const next = text.slice(0, active.start) + insert + text.slice(active.caret);
  return { text: next, caret: active.start + insert.length };
}

export interface ResolvedMention {
  name: string;
  id: string;
}

/** Resolve every "@app <name>" in the text to its app id (deduped by id). */
export function extractMentions(text: string, apps: CustomAppSummary[]): ResolvedMention[] {
  const byName = new Map(apps.map((a) => [a.name, a]));
  const found: ResolvedMention[] = [];
  const seen = new Set<string>();
  let m: RegExpExecArray | null;
  RESOLVED_RE.lastIndex = 0;
  while ((m = RESOLVED_RE.exec(text)) !== null) {
    const app = byName.get(m[1]);
    if (app && !seen.has(app.id)) {
      seen.add(app.id);
      found.push({ name: m[1], id: app.id });
    }
  }
  return found;
}

/**
 * Prepend a structured target-app header carrying each mentioned app's id so the
 * LLM unambiguously knows which custom app the user is referring to. When no
 * mention is present the text is returned unchanged (ongoing conversations that
 * already resolved the target need no extra hint).
 */
export function buildAppPrefix(text: string, apps: CustomAppSummary[]): string {
  const mentions = extractMentions(text, apps);
  if (mentions.length === 0) return text;
  const lines = mentions.map((mt) => `- ${mt.name}（app_id: ${mt.id}）`);
  const header = `【目标自定义应用】\n${lines.join("\n")}`;
  return `${header}\n\n${text}`;
}

/** Split text into plain + mention segments for the highlight overlay. */
export function splitMentions(text: string): Array<{ text: string; mention: boolean }> {
  const segments: Array<{ text: string; mention: boolean }> = [];
  let last = 0;
  let m: RegExpExecArray | null;
  RESOLVED_RE.lastIndex = 0;
  while ((m = RESOLVED_RE.exec(text)) !== null) {
    if (m.index > last) segments.push({ text: text.slice(last, m.index), mention: false });
    segments.push({ text: m[0], mention: true });
    last = m.index + m[0].length;
  }
  if (last < text.length) segments.push({ text: text.slice(last), mention: false });
  return segments;
}
