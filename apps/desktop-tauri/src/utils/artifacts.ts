import type { ChatMessage } from "../types";

export type ArtifactKind = "code" | "doc" | "data" | "image" | "office" | "file";

export interface Artifact {
  id: string;
  name: string;
  /**
   * Raw path as the LLM produced it. May be absolute (the post-fix path
   * the backend stamps in the `[file: …]` header) or relative (LLM
   * body mentions like `tmp/xx/方案.md`); callers pass `workspaceRoot`
   * to the Tauri read command so relative paths anchor automatically.
   *
   * Bodies are never cached on the artifact — `extractArtifacts` input
   * is tool output / LLM reply text, not file content. Stamping that
   * onto the artifact would make `PreviewPanel` show the wrong text;
   * file bodies are always re-read via `readArtifactFile`.
   */
  path: string;
  kind: ArtifactKind;
  tool: string;
  timestamp: string;
  /** True when the path matched the absolute-path regex (Windows drive
   * letter or POSIX leading `/`). UI uses this to colour relative
   * paths differently. */
  isAbsolute: boolean;
}

// Three flavors are accepted: Windows absolute (`F:\a\b.c`), POSIX
// absolute (`/a/b/c.py`), and multi-segment relative (`foo\bar.py`).
// Single-segment relative paths (`\foo.py`, `foo.py`) are intentionally
// rejected — they carry no directory information, common in LLM bash
// commands like `cat \loop_demo.py`, and anchoring them against a
// workspace root would silently point at the wrong file.
//
// Character class uses `[\p{L}\p{N}_-.\s()（）【】\[\]]` with the `u`
// flag so paths with spaces, parentheses, and non-CJK Unicode
// (Korean, Japanese, accented Latin) match; extension class uses
// `[\p{L}\p{N}]` for the same reason. Kept in sync with PATH_RE in
// MarkdownBody.tsx.
const PATH_RE = /(?:(?:[A-Za-z]:[\\/])|(?:[\\/]))(?:[\p{L}\p{N}_\-.\s()（）【】\[\]]+[\\/])+[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}|(?:[A-Za-z]:[\\/])[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}|(?:[\p{L}\p{N}_\-.\s()（）【】\[\]]+[\\/]){2,}[\p{L}\p{N}_\-.\s()（）【】\[\]]+\.[\p{L}\p{N}]{1,5}/giu;
// Same as PATH_RE but only matches absolute paths.
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
 * Maps a file name to its `ArtifactKind`. Check order is significant:
 * `doc` > `code` > `data` > `image` > `office` > `file` (e.g. `.json`
 * is `data`, never `code`; `.pdf` is `doc`, never `file`). All three
 * call sites (`classifyArtifactKind` callers in `InlinePathPreview.tsx`,
 * `App.tsx`, and this file) must route through here — past duplicates
 * drifted apart and produced inconsistent preview branches for the
 * same file.
 */
export function classifyArtifactKind(name: string): ArtifactKind {
  return classify(name);
}

function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

/**
 * 路径归一化 —— 「同一个文件」的**唯一**判定口径。
 *
 * 为什么需要：同一个文件在对话里会以多种写法出现 ——
 * `F:\a\b.md`（LLM 原样输出）、`F:/a/b.md`（正文里的链接）、
 * `\\?\F:\a\b.md`（Win32 长路径前缀）。它们指向同一个文件，字符串
 * 比较却全部不相等。历史上 `App.tsx` 用严格相等、`useArtifacts` 用
 * `endsWith`，两套口径各判各的：正文点 `F:/a/b.md` 时 `endsWith`
 * 匹配不上已提取的 `F:\a\b.md`，于是合成一个 `path:` 产物，而 App
 * 侧严格相等同样匹配不上 —— 同一文件开出两个预览标签。
 *
 * 只用于**比较**（去重 / 找邻居），不用于显示或 IO：展示仍用用户
 * 看到的原始路径，读文件仍走原始路径 + workspaceRoot 锚定。
 * Windows 路径大小写不敏感，故统一小写；顺带剥掉 `\\?\` 前缀。
 */
export function canonicalPath(p: string): string {
  return p.replace(/^\\\\\?\\/, "").replace(/\\/g, "/").toLowerCase();
}

// Writer tools (LLM intends to materialize a new file on disk). We
// watch their arguments for file paths so the right-sidebar artifact
// list shows the file the moment the LLM commits to writing it, not
// only after the tool reports success.
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

// Paths excluded from the artifact list: dependency dirs, build output,
// VCS internals, lock files, compiled binaries, caches. All directory
// patterns use the case-insensitive flag (Windows paths are
// case-insensitive and tool output may vary in casing).
const IGNORE_PATTERNS = [
  // Dependency & package manager dirs
  /node_modules[/\\]/i,
  /\.npm[/\\]/i,
  /\.yarn[/\\]/i,
  /\.pnpm[/\\]/i,
  /\.cargo[/\\]/i,         // Cargo registry + build artifacts

  // Version control
  /\.git[/\\]/i,

  // Language / framework build output
  /target[/\\]/i,          // Rust build output
  /dist[/\\]/i,            // Bundler output (webpack, vite, tsc outDir)
  /build[/\\]/i,
  /out[/\\]/i,             // Next.js / TypeScript outDir
  /\.next[/\\]/i,
  /\.nuxt[/\\]/i,
  /\.svelte-kit[/\\]/i,
  /\.output[/\\]/i,        // Nuxt / Nitro server output
  /\.expo[/\\]/i,
  /storybook-static[/\\]/i,

  // Tool internal dirs
  /\.workbuddy[/\\]/i,     // WorkBuddy binaries, memories, skills

  // Python runtime + packages
  /site-packages[/\\]/i,
  /dist-packages[/\\]/i,
  /__pycache__[/\\]/i,
  /\.venv[/\\]/i,
  /venv[/\\]/i,
  /\.pytest_cache[/\\]/i,

  // Caches
  /\.cache[/\\]/i,
  /\.turbo[/\\]/i,
  /\.nx[/\\]/i,
  /\.parcel-cache[/\\]/i,
  /\.esbuild[/\\]/i,

  // IDE / editor
  /\.idea[/\\]/i,
  /\.vscode[/\\]/i,

  // Test coverage
  /coverage[/\\]/i,

  // Config / lock files (filename patterns)
  /package-lock\.json$/i,
  /yarn\.lock$/i,
  /pnpm-lock\.yaml$/i,
  /next\.config\./i,

  // Compiled binary / bytecode extensions
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

// Patterns that mark a path-shaped string as noise from context
// (Python tracebacks `File "C:\foo.py", line 5`, Python frozen
// importlib `<frozen importlib._bootstrap>`, Rust panic locations
// `at C:\foo.rs:42:5`, and "hypothetical" example paths the LLM
// invents in prose). Two windows are checked:
//   • 40 chars BEFORE the match — for `File "`, `<frozen …`, `at ` markers
//   • 12 chars AFTER the match — for `:42:5` line/col tail (panic only)
const NOISE_BEFORE_PATTERNS = [
  /File\s*["<][^"<>]*$/i,        // ends with `File "…`
  /<frozen\s+\w*$/i,             // ends with `<frozen importlib`
];
const NOISE_AFTER_PATTERNS = [
  /:\d+:\d+$/,                   // `:42:5` Rust panic tail
  /['"],\s*line\s+\d+/i,         // `", line 5` Python traceback tail
];

// Returns true when the matched path looks like a real file reference
// (the user/agent wrote or read it) rather than a noise occurrence
// inside a traceback, panic, or LLM-invented example.
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
 * Scans chat messages for paths that look like file references and turns
 * them into `Artifact` entries. De-duplicates by canonical path
 * (slash-normalized, case-preserved). First occurrence wins. The
 * displayed path is preserved verbatim (we want to show the user
 * exactly what the LLM said) while `isAbsolute` lets the UI colour
 * relative paths differently.
 */
export function extractArtifacts(
  messages: ChatMessage[],
  _workspaceRoot?: string | null,
): Artifact[] {
  const byPath = new Map<string, Artifact>();
  let n = 0;

  const harvest = (raw: string, source: { tool: string; timestamp: string }) => {
    if (!raw) return;
    // Use exec loop (not matchAll) so we can inspect each match's
    // surrounding text and reject traceback / panic occurrences.
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
      if (!isArtifactPath(path)) { rejected++; continue; }
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

  // Direct path extraction from file_change events. Bypasses PATH_RE
  // (which can miss paths with spaces or unusual Unicode) by using
  // the backend-stamped absolute path verbatim, ensuring every file
  // the LLM writes shows up in the artifact panel.
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
        // file_change events carry the absolute path in `file_path`;
        // extract it directly because the regex may miss spaces or
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
        const raw = parts.join("\n");
        if (raw) harvest(raw, {
          tool: ev.tool ?? "tool",
          timestamp: ev.created_at ?? m.timestamp,
        });
      }
      harvest(m.content, { tool: "mention", timestamp: m.timestamp });
    }

    // tool-result messages (role: "tool") carry the full stdout/stderr
    // output of each call, often with file paths — they need to be
    // scanned separately because execution_timeline isn't persisted
    // across page reloads.
    if (m.role === "tool") {
      harvest(m.content, {
        tool: m.tool?.tool ?? "tool",
        timestamp: m.timestamp,
      });
      if (m.tool?.args) {
        harvest(m.tool.args, {
          tool: m.tool.tool,
          timestamp: m.timestamp,
        });
      }
      // tool.output accumulates onToolChunk stdout during streaming —
      // covers edge cases where m.content is empty/minimal but the
      // streamed output still mentions paths.
      if (m.tool?.output) {
        harvest(m.tool.output, {
          tool: m.tool.tool,
          timestamp: m.timestamp,
        });
      }
    }

    // User messages — scan for "show me path/to/file.md" style
    // references so user-mentioned paths stay tracked.
    if (m.role === "user") {
      harvest(m.content, { tool: "user", timestamp: m.timestamp });
    }
  }

  // Tool-call write targets: `m.tool_calls[*]` is the source of truth for
  // what the LLM asked the backend to do. Even if the reply text doesn't
  // echo the path, and even if execution_timeline / tool-result hasn't
  // landed yet, the tool_call is the canonical "I intend to write this
  // file" intent. We harvest a synthetic string for each known writer
  // tool so users see the artifact immediately — not only after the
  // tool finishes.
  for (const m of messages) {
    if (m.role !== "assistant") continue;
    for (const tc of m.tool_calls ?? []) {
      if (!isWriterTool(tc.name)) continue;
      // ToolCall has `name` + `output`; the actual arguments live on
      // the matching execution_timeline event (`ev.arguments` is a
      // stringified JSON blob). Find it by tool name — there is a 1:1
      // between (assistant, tool_start tool=name) and the tool_call.
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
      harvest(tc.output ?? "", { tool: tc.name, timestamp: m.timestamp });
    }
  }

  return Array.from(byPath.values());
}
