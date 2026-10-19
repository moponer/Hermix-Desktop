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
} from "../desktopBridge";
import { decodeTextBytes } from "@hermix/preview";

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

// `atob` + manual `Uint8Array` fill (cross-browser, no polyfill).
function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return bytes;
}

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
      const bytes = base64ToBytes(res.contentBase64 ?? "");
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
      if (res.kind === "empty" || !res.dataBase64) {
        return { kind: "empty", resolvedPath: resolved };
      }
      // 后端送的是原始字节；文本 / 二进制判定与编码解码统一在共享层做
      // （BOM / UTF-16 / GBK），规则只有一份。
      const decoded = decodeTextBytes(base64ToBytes(res.dataBase64));
      if (decoded.kind === "empty") {
        return { kind: "empty", resolvedPath: resolved };
      }
      if (decoded.kind === "binary") {
        return { kind: "binary", size: res.size, resolvedPath: resolved };
      }
      return { kind: "text", content: decoded.text, resolvedPath: resolved };
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
// chain as the other artifact readers so cross-drive workspace paths resolve.
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

