// Read-with-root-retry helpers shared by every preview surface
// (InlinePathPreview, PreviewPanel, RightSidebar). The backend's
// `read_artifact_file` enforces an allowlist of "allowed artifact
// roots", and rejects reads when the LLM writes outside the user's
// workspace (e.g. agent-temp on `D:\data\…` with workspace on `C:\`).
// We walk ancestors and retry — usually one of them lands inside
// an accepted root, with no backend rebuild required.

import {
  artifactPathExists,
  convertDocumentToMarkdown,
  readArtifactFile,
  readArtifactFileBytes,
  renderOfficePreview,
} from "../desktopBridge";

// Returns `D:\` for Windows absolute paths, `/` for POSIX absolute
// paths, or null for relative paths (so we never fabricate a root).
export function extractDriveRoot(p: string): string | null {
  if (!p) return null;
  const m = /^([A-Za-z]:[\\/])/.exec(p);
  if (m) return m[1].replace(/\//g, "\\");
  if (p.startsWith("/") && (p[1] === "" || p[1] === "/")) return "/";
  return null;
}

// Drive root + each ancestor (up to but not including the root).
// Used as a fallback chain when the caller's workspaceRoot isn't
// accepted by the backend.
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

// Same fallback chain as `tryReadArtifact` but returns raw bytes —
// used for binary formats the front-end parses in-browser (xlsx,
// xls, csv via SheetJS, etc.). The base64 payload is decoded with
// `atob` + manual `Uint8Array` fill (cross-browser, no polyfill).
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

// Reads via workspaceRoot, then walks up the path's ancestors until
// one is accepted. Returns the first successful read or an error
// carrying the last failed attempt's message.
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
      if (!/outside the allowed artifact roots/i.test(lastRaw)) {
        return { kind: "error", message: lastRaw };
      }
    }
  }
  return { kind: "error", message: lastRaw };
}

// Same retry chain as `tryReadArtifact` but for the cheap existence
// probe used by the artifact panel — without it the panel can stay
// empty while a preview that uses the text-reader chain still opens.
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
      // Existence probe tends to silently return false rather than
      // throw the allowlist error — keep walking the chain either way.
    }
  }
  return false;
}

export type DocumentMarkdownPayload = {
  markdown: string;
  fileName: string;
  truncated: boolean;
  resolvedPath: string;
};

// Document → Markdown via the `markitdown` sidecar. Same ancestor-retry
// chain as the Office renderer so cross-drive workspace paths resolve.
export async function tryConvertDocumentToMarkdown(
  path: string,
  workspaceRoot?: string | null,
): Promise<DocumentMarkdownPayload | { error: string }> {
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
      const res = await convertDocumentToMarkdown(path, root);
      return {
        markdown: res.markdown,
        fileName: res.fileName,
        truncated: res.truncated,
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

export type OfficePreviewResult = {
  htmlPath: string;
  htmlContent: string;
  fileName: string;
  resolvedPath: string;
};

// Office variants don't go through the text-reader chain — they
// invoke a separate `render_office_preview` Tauri command (which
// shells out to `officecli`). Same ancestor-retry logic applies so
// `D:\` paths still work when workspaceRoot is on `C:\`.
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
