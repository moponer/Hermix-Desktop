import type { ChatMessage } from "../types";

export type ArtifactKind = "code" | "doc" | "data" | "image" | "office" | "file";

export interface Artifact {
  id: string;
  name: string;
  /**
   * Raw path exactly as the LLM produced it. May be absolute (preferred — that's
   * the post-fix path the backend's `[file: …]` header stamps) or relative
   * (LLM body mentions like "tmp/xx/方案.md"). The right sidebar / inline
   * preview pass `workspaceRoot` to the Tauri command so relative paths get
   * anchored automatically.
   *
   * NOTE: We intentionally do NOT cache the file body on the artifact.
   * The `extractArtifacts` input (`m.content`, `m.tool.output`, etc.) is
   * tool output / LLM reply text — NOT file content. Stamping that onto
   * the artifact would make PreviewPanel display the wrong text. File
   * bodies are always read fresh from disk via `readArtifactFile`.
   */
  path: string;
  kind: ArtifactKind;
  tool: string;
  timestamp: string;
  /**
   * `true` when the path looked like a full absolute path (Windows drive
   * letter or POSIX leading `/`). The front-end can prefer resolved paths
   * from the backend's `resolvedPath` field when reopening.
   */
  isAbsolute: boolean;
}

// Path-shaped strings. Three flavors are accepted:
//   1. Windows absolute: F:\a\b.c            — drive letter + ≥1 segment + filename
//   2. POSIX    absolute: /a/b/c.py          — leading slash   + ≥1 segment + filename
//   3. Multi-seg relative: foo\bar.py        — ≥2 segments (one separator) + filename
//
// We intentionally do NOT match single-segment relative paths like `\foo.py`
// or `foo.py` because they carry no directory information — common in LLM
// bash commands like `cat \loop_demo.py` where the working directory is
// implicit. Anchoring these against a workspace root would silently point
// at the wrong file, so we drop them as ambiguous.
//
// 2026-07-14: Switched from [\w.\-\u4e00-\u9fff] to [\p{L}\p{N}_\-.\s()（）【】\[\]]
// with the `u` flag so paths with spaces (e.g. "My Documents"), parentheses,
// and non-CJK Unicode (Korean, Japanese, accented Latin) are matched.
// The extension character class also switched from [a-z0-9] to [\p{L}\p{N}]
// for the same reason. This aligns with MarkdownBody.tsx's PATH_RE.
const PATH_RE = /(?:(?:[A-Za-z]:[\\/])|(?:[\\/]))(?:[\p{L}\p{N}_\-.\s()（）【】\[\]]+[\\/])+[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}|(?:[A-Za-z]:[\\/])[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}|(?:[\p{L}\p{N}_\-.\s()（）【】\[\]]+[\\/]){2,}[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}/giu;
// Same as PATH_RE but only matches absolute paths. Used to flag confidence.
const ABS_PATH_RE = /(?:(?:[A-Za-z]:[\\/])|(?:[\/]))(?:[\p{L}\p{N}_\-.\s()（）【】\[\]]+[\\/])+[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}/giu;

const CODE_EXTS = new Set([
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "java", "kt",
  "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "m", "mm", "swift",
  "rb", "php", "sh", "bash", "zsh", "ps1", "css", "scss", "sass", "less",
  "html", "vue", "svelte", "lua", "sql", "ino",
]);
const DOC_EXTS = new Set(["md", "mdx", "txt", "rst", "adoc", "tex", "pdf"]);
const DATA_EXTS = new Set(["json", "yaml", "yml", "toml", "xml", "csv", "tsv", "ini", "conf", "env"]);
const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "bmp", "tiff"]);
const OFFICE_EXTS = new Set(["docx", "doc", "xlsx", "xls", "pptx", "ppt", "odt", "ods", "odp"]);

function classify(name: string): ArtifactKind {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (DOC_EXTS.has(ext)) return "doc";
  if (CODE_EXTS.has(ext)) return "code";
  if (DATA_EXTS.has(ext)) return "data";
  if (IMAGE_EXTS.has(ext)) return "image";
  if (OFFICE_EXTS.has(ext)) return "office";
  return "file";
}

/**
 * Public version of [`classify`] — maps a file name (with or without path)
 * to its [`ArtifactKind`]. Order of checks is significant: `doc` > `code`
 * > `data` > `image` > `office` > `file` (e.g. a `.json` is `data`, never
 * `code`; a `.pdf` is `doc`, never `file`).
 *
 * 2026-07-17: extracted from `InlinePathPreview.tsx` and the inline
 * classifier in `App.tsx`. Both duplicates had drifted extension sets
 * (one missed `.sql`/`.pdf`/`.ico`/`.odt`/`.bmp`/`.tiff`), causing
 * different preview branches and icons for the same file depending on
 * which side of the app showed it. Keep all three call sites pointing
 * here so the mapping stays canonical.
 */
export function classifyArtifactKind(name: string): ArtifactKind {
  return classify(name);
}

function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

/**
 * Tools that the LLM invokes when it intends to materialize a new
 * file on disk.  We watch their arguments for file paths so the
 * right-sidebar artifact list shows the file the moment the LLM
 * commits to writing it (not just after the tool reports success).
 */
const WRITER_TOOLS = new Set([
  "write_file",
  "create_file",
  "edit_file",
  "patch_file",
  "append_file",
  "save_file",
  "write",
  "Write",
  "writefile",
  "createFile",
  "editFile",
  "patchFile",
  "appendFile",
  "saveFile",
]);

function isWriterTool(name: string | undefined | null): boolean {
  if (!name) return false;
  return WRITER_TOOLS.has(name);
}

function isAbsolutePath(p: string): boolean {
  ABS_PATH_RE.lastIndex = 0;
  return ABS_PATH_RE.test(p);
}

// Exclude files that are not meaningful user-facing artifacts:
// dependency dirs, build output, version-control internals,
// lock files, compiled binaries, cache, etc.
// All directory patterns use case-insensitive flag (Windows paths are
// case-insensitive; tool output may vary in casing).
const IGNORE_PATTERNS = [
  // ── Dependency & package manager dirs ──
  /node_modules[/\\]/i,
  /\.npm[/\\]/i,
  /\.yarn[/\\]/i,
  /\.pnpm[/\\]/i,
  /\.cargo[/\\]/i,         // Cargo registry / build artifacts

  // ── Version control ──
  /\.git[/\\]/i,

  // ── Language / framework build output ──
  /target[/\\]/i,          // Rust build output
  /dist[/\\]/i,            // Bundler output (webpack, vite, tsc — outDir)
  /build[/\\]/i,           // Generic build output
  /out[/\\]/i,             // Next.js / TypeScript outDir
  /\.next[/\\]/i,
  /\.nuxt[/\\]/i,
  /\.svelte-kit[/\\]/i,
  /\.output[/\\]/i,        // Nuxt / Nitro server output
  /\.expo[/\\]/i,
  /storybook-static[/\\]/i,

  // ── Tool internal dirs ──
  /\.workbuddy[/\\]/i,     // WorkBuddy binaries, memories, skills…

  // ── Python runtime + packages ──
  /site-packages[/\\]/i,   // Python third-party packages
  /dist-packages[/\\]/i,   // Python dist-packages

  // ── Cache dirs ──
  /\.cache[/\\]/i,
  /\.turbo[/\\]/i,
  /\.nx[/\\]/i,
  /\.parcel-cache[/\\]/i,
  /\.esbuild[/\\]/i,

  // ── Python ──
  /__pycache__[/\\]/i,
  /\.venv[/\\]/i,
  /venv[/\\]/i,
  /\.pytest_cache[/\\]/i,

  // ── IDE / editor ──
  /\.idea[/\\]/i,
  /\.vscode[/\\]/i,

  // ── Test coverage ──
  /coverage[/\\]/i,

  // ── Config / lock files (filename only) ──
  /package-lock\.json$/i,
  /yarn\.lock$/i,
  /pnpm-lock\.yaml$/i,
  /next\.config\./i,

  // ── Compiled binary / bytecode extensions ──
  /\.exe$/i,
  /\.dll$/i,
  /\.so$/i,
  /\.dylib$/i,
  /\.wasm$/i,
  /\.class$/i,
  /\.o$/i,
  /\.obj$/i,
  /\.pyc$/i,
  /\.pyo$/i,
];

// Patterns that mark a path-shaped string as noise from context:
//   • Python traceback lines:    `File "C:\foo.py", line 5`
//   • Python frozen importlib:   `<frozen importlib._bootstrap>`
//   • Rust panic locations:      `at C:\foo.rs:42:5` (and `:42:5` line/col)
//   • "hypothetical" example paths the LLM invents in prose
//
// Two windows are checked:
//   • 40 chars BEFORE the match — for `File "`, `<frozen …`, `at ` markers
//   • 12 chars AFTER the match — for `:42:5` line/col tail (panic only)
const NOISE_BEFORE_PATTERNS = [
  /File\s*["<][^"<>]*$/i,        // ends with `File "…`
  /<frozen\s+\w*$/i,             // ends with `<frozen importlib`
];
// Path-suffix patterns that only appear in error messages:
//   • `:42:5`  — Rust panic (file:line:col)
//   • `", line 5` — Python traceback (file", line N)
const NOISE_AFTER_PATTERNS = [
  /:\d+:\d+$/,                   // `:42:5` Rust panic tail
  /['"],\s*line\s+\d+/i,         // `", line 5` Python traceback tail
];

/**
 * Given the raw text being harvested and a match, decide whether the
 * match is real (a file the user/agent actually wrote or read) or
 * noise (a path that just happened to appear inside a traceback,
 * panic, or LLM example).
 */
function isContextuallyReal(
  raw: string,
  matchIndex: number,
  matchLength: number,
): boolean {
  const beforeStart = Math.max(0, matchIndex - 40);
  const beforeWindow = raw.slice(beforeStart, matchIndex);
  if (NOISE_BEFORE_PATTERNS.some((re) => re.test(beforeWindow))) return false;
  const afterEnd = Math.min(raw.length, matchIndex + matchLength + 12);
  const afterWindow = raw.slice(matchIndex + matchLength, afterEnd);
  if (NOISE_AFTER_PATTERNS.some((re) => re.test(afterWindow))) return false;
  return true;
}

function isArtifactPath(path: string): boolean {
  const norm = path.replace(/\\/g, "/");
  return !IGNORE_PATTERNS.some((re) => re.test(norm));
}

/**
 * Scan an assistant message for paths that look like file references and
 * turn them into Artifact entries. De-duplicates by canonical path
 * (slash-normalized, case-preserved). First occurrence wins.
 *
 * `workspaceRoot` is stored on each artifact so the Tauri read command can
 * anchor relative paths. We don't transform the displayed path here — we
 * want to show the user exactly what the LLM said, but tag confidence via
 * `isAbsolute` so the UI can color relative paths differently.
 */
export function extractArtifacts(
  messages: ChatMessage[],
  _workspaceRoot?: string | null,
): Artifact[] {
  const byPath = new Map<string, Artifact>();
  let n = 0;

  const harvest = (raw: string, source: { tool: string; timestamp: string }) => {
    if (!raw) return;
    // Use matchAll so we can inspect each match's index and decide
    // whether the surrounding text looks like a real reference or
    // a noise path (e.g. a path inside a Python traceback line).
    const re = new RegExp(PATH_RE.source, PATH_RE.flags);
    let m: RegExpExecArray | null;
    const debug = typeof window !== "undefined"
      && window.localStorage?.getItem("hermixDebugArtifacts") === "1";
    let kept = 0;
    let rejected = 0;
    while ((m = re.exec(raw)) !== null) {
      const path = m[0];
      const norm = path.replace(/\\/g, "/");
      if (byPath.has(norm)) continue;
      // Skip non-artifact paths (dependencies, build output, binaries…)
      if (!isArtifactPath(path)) { rejected++; continue; }
      // Skip paths that are part of an error message / traceback
      // (e.g. Python: `File "C:\foo.py", line 5`,
      //       Rust:   `at C:\foo.rs:42:5`).
      if (!isContextuallyReal(raw, m.index, m[0].length)) { rejected++; continue; }
      kept++;
      const name = basename(path);
      byPath.set(norm, {
        id: `art-${n++}`,
        name,
        path,
        kind: classify(name),
        tool: source.tool,
        timestamp: source.timestamp,
        isAbsolute: isAbsolutePath(path),
      });
    }
    if (debug && (kept > 0 || rejected > 0)) {
      // eslint-disable-next-line no-console
      console.debug("[harvest] tool=%s kept=%d rejected=%d", source.tool, kept, rejected);
    }
  };

  // 2026-07-14: Direct path extraction from file_change events.
  // Instead of relying on PATH_RE to match the file_path inside a
  // concatenated string (which fails for paths with spaces or
  // unusual Unicode), we directly create an artifact from the
  // backend-stamped absolute path. This ensures every file the
  // LLM writes shows up in the artifact panel.
  const harvestDirectPath = (
    rawPath: string,
    source: { tool: string; timestamp: string },
  ) => {
    if (!rawPath) return;
    const path = rawPath.trim();
    if (!path) return;
    const norm = path.replace(/\\/g, "/");
    if (byPath.has(norm)) return;
    if (!isArtifactPath(path)) return;
    const name = basename(path);
    byPath.set(norm, {
      id: `art-${n++}`,
      name,
      path,
      kind: classify(name),
      tool: source.tool,
      timestamp: source.timestamp,
      isAbsolute: isAbsolutePath(path),
    });
  };

  for (const m of messages) {
    if (m.role === "assistant") {
      for (const tc of m.tool_calls ?? []) {
        const raw = (tc.output ?? "") + "\n";
        harvest(raw, {
          tool: tc.name,
          timestamp: m.timestamp,
        });
      }
      for (const ev of m.execution_timeline ?? []) {
        // file_change events are special: the server stamps the
        // absolute path in `file_path`. Extract it directly instead
        // of relying on PATH_RE to match it inside a concatenated
        // string — the regex may not match paths with spaces or
        // unusual Unicode characters.
        if (ev.type === "file_change" && ev.file_path) {
          harvestDirectPath(ev.file_path, {
            tool: ev.tool ?? "file_change",
            timestamp: ev.created_at ?? m.timestamp,
          });
        }
        // Most events also carry paths in arguments/content (tool_start
        // args, tool stdout/completion text). Harvest those too.
        const parts: string[] = [];
        if (ev.arguments) parts.push(ev.arguments);
        if (ev.content) parts.push(ev.content);
        // Don't re-harvest file_path via the regex path — already done above.
        const raw = parts.join("\n");
        if (raw) harvest(raw, {
          tool: ev.tool ?? "tool",
          timestamp: ev.created_at ?? m.timestamp,
        });
      }
      harvest(m.content, { tool: "mention", timestamp: m.timestamp });
    }

    // Also scan tool-result messages (role: "tool"). These are
    // persisted by the backend (see Message::tool_result) and
    // contain the full stdout/stderr output of each tool call,
    // which often mentions file paths. Without this, artifacts
    // from tool output are invisible after a page reload because
    // execution_timeline is not persisted.
    if (m.role === "tool") {
      // The tool's main output text.
      harvest(m.content, {
        tool: m.tool?.tool ?? "tool",
        timestamp: m.timestamp,
      });
      // Also scan tool arguments for paths embedded in JSON
      // (e.g. {"path":"F:\\spiker\\test.txt"}).
      if (m.tool?.args) {
        harvest(m.tool.args, {
          tool: m.tool.tool,
          timestamp: m.timestamp,
        });
      }
      // Scan tool.output (accumulated from onToolChunk during
      // streaming) — covers edge cases where m.content is
      // empty/minimal but tool.output still has full stdout.
      if (m.tool?.output) {
        harvest(m.tool.output, {
          tool: m.tool.tool,
          timestamp: m.timestamp,
        });
      }
    }

    // User messages — scan for "show me path/to/file.md" style
    // references so artifacts mentioned by the user are still tracked.
    if (m.role === "user") {
      harvest(m.content, { tool: "user", timestamp: m.timestamp });
    }
  }

  // ── Tool-call write targets (any tool where the LLM invoked a
  //    "write/append/create a file" action) ──────────────────────
  //    `m.tool_calls[*]` is the source of truth for what the LLM asked
  //    the backend to do. Even if the assistant's reply text doesn't
  //    echo the path, and even if execution_timeline / tool-result
  //    message hasn't landed yet, the tool_call is the canonical
  //    "I intend to write this file" intent. We harvest a synthetic
  //    string for each known writer tool so users see the artifact
  //    *immediately* in the panel as soon as the assistant decides
  //    to write it — not only after the tool finishes.
  for (const m of messages) {
    if (m.role !== "assistant") continue;
    for (const tc of m.tool_calls ?? []) {
      if (!isWriterTool(tc.name)) continue;
      // ToolCall struct has `name` + `output`; the actual arguments live
      // on the matching execution_timeline event (`ev.arguments` is a
      // stringified JSON blob).  Find that event by tool name — there is
      // a 1:1 between (assistant, tool_start tool=name) and the tool_call.
      const argBlob =
        m.execution_timeline
          ?.find((ev) => ev.tool === tc.name && typeof ev.arguments === "string")
          ?.arguments ?? "";
      if (argBlob) {
        harvest(argBlob, {
          tool: tc.name,
          timestamp: m.timestamp,
        });
      }
      // Also pull path info from the tool's own output / file_change events.
      harvest(tc.output ?? "", { tool: tc.name, timestamp: m.timestamp });
    }
  }

  return Array.from(byPath.values());
}
