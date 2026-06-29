/**
 * Shared read-with-root-retry helpers used by every preview surface
 * (InlinePathPreview, PreviewPanel, RightSidebar).
 *
 * Why this exists: the backend's `read_artifact_file` enforces an
 * allowlist of "allowed artifact roots". When the LLM writes to a
 * location outside the user's configured workspace (e.g. an
 * agent-temp dir on `D:\data\...` while the workspace is on `C:\`),
 * the first read attempt with `workspaceRoot` returns the dreaded
 * "outside the allowed artifact roots" error. Walking up the path's
 * parent directories and retrying with each ancestor as the root
 * almost always lands on a directory the backend will accept.
 *
 * The frontend fix avoids a backend rebuild — the allowlist data
 * stays where it is; we just hand the backend more candidate roots
 * to try.
 */

import {
  artifactPathExists,
  readArtifactFile,
  readArtifactFileBytes,
  renderOfficePreview,
} from "../desktopBridge";

/** Extract the drive root (`D:\`) from a Windows absolute path, or
 * `/` for POSIX absolute paths. Returns null for relative paths so
 * we never fabricate a root out of thin air. */
export function extractDriveRoot(p: string): string | null {
  if (!p) return null;
  const m = /^([A-Za-z]:[\\/])/.exec(p);
  if (m) return m[1].replace(/\//g, "\\");
  if (p.startsWith("/") && (p[1] === "" || p[1] === "/")) return "/";
  return null;
}

/** Walk parents and return drive root + each ancestor up to (but
 * not including) the drive root. Used as a fallback chain when the
 * caller's workspaceRoot isn't accepted by the backend. */
export function candidateRoots(p: string): string[] {
  const out: string[] = [];
  const drive = extractDriveRoot(p);
  if (drive) out.push(drive);
  const sep = p.includes("\\") ? "\\" : "/";
  let cur = p.replace(/[\\/]+$/, "");
  while (true) {
    const idx = cur.lastIndexOf(sep);
    if (idx <= 0) break;
    cur = cur.slice(0, idx);
    if (cur.length >= 3 || cur === "/") out.push(cur);
  }
  return out;
}

export type ArtifactReadResult =
  | { kind: "text"; content: string; resolvedPath: string }
  | { kind: "binary"; size: number; resolvedPath: string }
  | { kind: "empty"; resolvedPath: string }
  | { kind: "error"; message: string };

export type ArtifactBytesResult =
  | { kind: "bytes"; bytes: Uint8Array; size: number; resolvedPath: string }
  | { kind: "error"; message: string };

/** Try to read the artifact's raw bytes using the same fallback
 * chain as `tryReadArtifact`. Used for binary formats the
 * front-end wants to parse in-browser (xlsx, xls, csv via SheetJS,
 * etc.). The base64 string is decoded with `atob` + manual
 * `Uint8Array` fill, which works in any modern browser without
 * needing a polyfill. */
export async function tryReadArtifactBytes(
  path: string,
  workspaceRoot?: string | null,
): Promise<ArtifactBytesResult> {
  const tried = new Set<string | null>();
  const queue: Array<string | null> = [workspaceRoot ?? null];
  for (const cand of candidateRoots(path)) {
    if (!tried.has(cand)) queue.push(cand);
  }

  let lastRaw = "";
  for (const root of queue) {
    if (tried.has(root)) continue;
    tried.add(root);
    try {
      const res = await readArtifactFileBytes(path, root);
      const b64 = res.contentBase64 ?? "";
      // atob returns a "binary string" — one char per byte — which
      // we then turn into a real Uint8Array. charCodeAt(i) is the
      // fastest cross-browser way to read a single byte.
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) {
        bytes[i] = bin.charCodeAt(i);
      }
      return {
        kind: "bytes",
        bytes,
        size: res.size,
        resolvedPath: res.resolvedPath ?? path,
      };
    } catch (e) {
      lastRaw = String(e);
      if (!/outside the allowed artifact roots/i.test(lastRaw)) {
        return { kind: "error", message: lastRaw };
      }
    }
  }
  return { kind: "error", message: lastRaw };
}

/** Try reading via workspaceRoot, then walk up the path's ancestors
 * until one is accepted. Returns the first successful read or an
 * error carrying the last failed attempt's message. */
export async function tryReadArtifact(
  path: string,
  workspaceRoot?: string | null,
): Promise<ArtifactReadResult> {
  const tried = new Set<string | null>();
  const queue: Array<string | null> = [workspaceRoot ?? null];
  for (const cand of candidateRoots(path)) {
    if (!tried.has(cand)) queue.push(cand);
  }

  let lastRaw = "";
  for (const root of queue) {
    if (tried.has(root)) continue;
    tried.add(root);
    try {
      const res = await readArtifactFile(path, root);
      const resolved = res.resolvedPath ?? path;
      if (res.kind === "text") {
        return { kind: "text", content: res.content ?? "", resolvedPath: resolved };
      }
      if (res.kind === "binary") {
        return { kind: "binary", size: res.size, resolvedPath: resolved };
      }
      return { kind: "empty", resolvedPath: resolved };
    } catch (e) {
      lastRaw = String(e);
      // Only keep retrying on the "outside the allowed artifact roots"
      // error — anything else (404, IO error, …) is a hard fail and
      // would just delay surfacing the real message.
      if (!/outside the allowed artifact roots/i.test(lastRaw)) {
        return { kind: "error", message: lastRaw };
      }
    }
  }
  return { kind: "error", message: lastRaw };
}

export type OfficePreviewResult = {
  htmlPath: string;
  htmlContent: string;
  fileName: string;
  resolvedPath: string;
};

/** Same retry chain as tryReadArtifact but for the cheap "does the
 * file exist?" probe used by the artifact panel. Without this the
 * panel can stay empty even though the user can still click into a
 * preview that does retry — because the panel uses one-shot
 * `artifact_path_exists` and the preview uses `tryReadArtifact`. */
export async function tryArtifactPathExists(
  path: string,
  workspaceRoot?: string | null,
): Promise<boolean> {
  const tried = new Set<string | null>();
  const queue: Array<string | null> = [workspaceRoot ?? null];
  for (const cand of candidateRoots(path)) {
    if (!tried.has(cand)) queue.push(cand);
  }
  for (const root of queue) {
    if (tried.has(root)) continue;
    tried.add(root);
    try {
      if (await artifactPathExists(path, root)) return true;
    } catch {
      // Backend rejects with "outside the allowed artifact roots" only
      // on actual reads; the existence probe tends to silently return
      // false. Either way, just keep walking the chain.
    }
  }
  return false;
}

/** Office variants don't go through the text-reader retry — they
 * invoke a separate `render_office_preview` Tauri command that
 * internally shells out to `officecli`. Apply the same fallback
 * chain so a path on `D:\` doesn't fail just because workspaceRoot
 * is on `C:\`. */
export async function tryRenderOfficePreview(
  path: string,
  workspaceRoot?: string | null,
): Promise<OfficePreviewResult | { error: string }> {
  const tried = new Set<string | null>();
  const queue: Array<string | null> = [workspaceRoot ?? null];
  for (const cand of candidateRoots(path)) {
    if (!tried.has(cand)) queue.push(cand);
  }

  let lastRaw = "";
  for (const root of queue) {
    if (tried.has(root)) continue;
    tried.add(root);
    try {
      const res = await renderOfficePreview(path, root);
      return {
        htmlPath: res.htmlPath,
        htmlContent: res.htmlContent,
        fileName: res.fileName,
        resolvedPath: path,
      };
    } catch (e) {
      lastRaw = String(e);
      if (!/outside the allowed artifact roots/i.test(lastRaw)) {
        return { error: lastRaw };
      }
    }
  }
  return { error: lastRaw };
}
