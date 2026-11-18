// Tool-name → display helpers, tool-icon mapping, and small pure
// formatting utilities previously colocated in MessageBubble.tsx.
// Pure functions only (no JSX) — safe to share across bubble files.
import {
  Wrench, Terminal, Eye, FilePlus, Edit3, Trash2, Search, FileSearch,
  FolderOpen, Globe, Database, Bot, Brain, HelpCircle, GitBranch, Hammer, FileText,
} from "lucide-react";
import { describeShellCommand } from "../lib/commandDescribe";
/**
 * 毫秒 → `29s` / `4m 12s` / `3h 18m` / `2d 4h`。
 *
 * 2026-10-01 用户修正：这个函数原先**只进位到分钟**，于是 8719 分钟被
 * 渲染成「8719m 55s」—— 一眼看过去像是个正常数字，实际是个 bug 的
 * 产物。耗时是「轮次」尺度的量（自治任务几小时很常见），不进位到小时
 * 就等于不给量级。粒度与参考界面的「3 时 18 分」对齐。
 */
export function formatDuration(ms: number | undefined): string {
  if (ms === undefined || !Number.isFinite(ms) || ms <= 0) return "";
  const totalSec = Math.round(ms / 1000);
  if (totalSec < 60) return `${totalSec}s`;
  if (totalSec < 3600) {
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return s === 0 ? `${m}m` : `${m}m ${s}s`;
  }
  if (totalSec < 86400) {
    const h = Math.floor(totalSec / 3600);
    const m = Math.floor((totalSec % 3600) / 60);
    // 秒在小时档是噪声（"3h 18m 7s" 读起来比 "3h 18m" 难读），
    // 只有整小时才省掉分钟位。
    return m === 0 ? `${h}h` : `${h}h ${m}m`;
  }
  const d = Math.floor(totalSec / 86400);
  const h = Math.floor((totalSec % 86400) / 3600);
  return h === 0 ? `${d}d` : `${d}d ${h}h`;
}
interface ToolCategory {
  keywords: string[];
  verb: string;
  kind: "shell" | "read" | "write" | "edit" | "delete" | "search-web" | "search-file" | "list" | "fetch" | "parse" | "image" | "code" | "git" | "build" | "test" | "install" | "deploy" | "db" | "desktop" | "reason" | "delegate" | "ask" | "clipboard" | "network" | "other";
}

const TOOL_CATEGORIES: ToolCategory[] = [
  {
    keywords: ["terminal", "bash", "shell", "exec", "execute_command", "run_command", "run_script", "cmd", "powershell", "pwsh"],
    verb: "执行命令",
    kind: "shell",
  },
  {
    keywords: ["read_file", "read_pdf", "read_xls", "read_xlsx", "read_docx", "read_doc", "read_pptx", "read_image", "view_file", "cat", "open_file", "preview", "inspect"],
    verb: "读取文件",
    kind: "read",
  },
  {
    keywords: ["write_file", "create_file", "save_file", "add_file", "new_file", "output_file", "append_file"],
    verb: "写入文件",
    kind: "write",
  },
  {
    keywords: ["edit_file", "edit", "patch", "update_file", "modify_file", "rewrite", "replace", "change_file"],
    verb: "编辑文件",
    kind: "edit",
  },
  {
    keywords: ["delete_file", "remove_file", "unlink", "rm_file"],
    verb: "删除文件",
    kind: "delete",
  },
  {
    keywords: ["file_operation", "file_op", "move_file", "rename_file", "copy_file"],
    verb: "操作文件",
    kind: "edit",
  },
  {
    keywords: ["search_web", "web_search", "google", "bing", "duckduckgo", "search_internet", "search_query"],
    verb: "搜索网络",
    kind: "search-web",
  },
  {
    keywords: ["search_files", "file_search", "file_query", "grep", "find_file", "locate", "search", "search_directory"],
    verb: "搜索文件",
    kind: "search-file",
  },
  {
    keywords: ["list_files", "list_dir", "list_directory", "ls", "dir", "tree", "walk", "readdir", "glob"],
    verb: "查看目录",
    kind: "list",
  },
  {
    keywords: ["browser", "web_fetch", "fetch_url", "fetch_page", "http_get", "http_request", "curl", "wget", "download", "webpage"],
    verb: "获取网页",
    kind: "fetch",
  },
  {
    keywords: ["network", "ping", "dns", "socket", "traceroute", "nslookup"],
    verb: "网络诊断",
    kind: "network",
  },
  {
    keywords: ["parse_pdf", "extract_pdf", "pdf_to_text", "pdf_parse", "read_pdf"],
    verb: "解析 PDF",
    kind: "parse",
  },
  {
    keywords: ["image_edit", "edit_image", "resize_image", "convert_image", "process_image", "image_process"],
    verb: "处理图片",
    kind: "image",
  },
  {
    keywords: ["code", "generate_code", "create_code", "write_code", "implement"],
    verb: "生成代码",
    kind: "code",
  },
  {
    keywords: ["python", "py_run", "run_python", "node_run", "run_node", "run_js", "run_script", "script"],
    verb: "运行代码",
    kind: "shell",
  },
  {
    keywords: ["git", "commit", "push", "pull", "branch", "merge", "clone", "checkout", "diff", "log", "status", "stash", "rebase", "fetch"],
    verb: "Git 操作",
    kind: "git",
  },
  {
    keywords: ["build", "compile", "cargo", "npm_run", "pnpm_run", "yarn_run", "make", "cmake", "gradle", "mvn", "webpack", "vite", "tsc"],
    verb: "构建项目",
    kind: "build",
  },
  {
    keywords: ["test", "pytest", "jest", "vitest", "mocha", "spec", "run_test", "unit_test"],
    verb: "运行测试",
    kind: "test",
  },
  {
    keywords: ["install", "add_package", "add_dep", "npm_install", "pnpm_add", "pip_install", "cargo_add"],
    verb: "安装依赖",
    kind: "install",
  },
  {
    keywords: ["deploy", "publish", "release", "upload", "ship", "rollout"],
    verb: "部署发布",
    kind: "deploy",
  },
  {
    keywords: ["sql", "database", "db", "sqlite", "postgres", "mongo", "select", "insert_query", "update_db", "query_db", "db_query"],
    verb: "操作数据库",
    kind: "db",
  },
  {
    keywords: ["computer_use", "desktop", "screenshot", "mouse", "keyboard", "gui"],
    verb: "操作桌面",
    kind: "desktop",
  },
  {
    keywords: ["think", "reason", "reflect", "analyze", "plan_task", "plan"],
    verb: "正在思考",
    kind: "reason",
  },
  //    MUST come before "询问用户" — "delegate_task" contains
  //    the substring "ask" (t-a-s-k), so it would wrongly
  //    match the "ask" keyword in the ask-user category.
  {
    keywords: ["delegate_task", "delegate", "sub_agent", "subagent", "spawn_agent", "fan_out", "get_subagent_result", "get_sub_agent_result"],
    verb: "委派子agent",
    kind: "delegate",
  },
  //    NOTE: "ask" keyword is broad — it matches any tool name
  //    containing "ask" as a substring. The "delegate" category
  //    above must stay BEFORE this one to prevent
  //    "delegate_task" from being misclassified as "询问用户".
  {
    keywords: ["ask_user", "ask", "question", "confirm", "clarify"],
    verb: "询问用户",
    kind: "ask",
  },
  {
    keywords: ["clipboard", "paste", "copy_to_clipboard", "read_clipboard"],
    verb: "读取剪贴板",
    kind: "clipboard",
  },
  // 2026-08-29: custom app management — placed before the catch-all
  // so toolVerb returns a meaningful Chinese verb instead of the raw
  // English tool name (user feedback: tool rows and batch approval
  // cards showed "custom_app_manager" verbatim).
  {
    keywords: ["custom_app_manager", "custom_app", "app_manager"],
    verb: "自定义应用管理",
    kind: "other",
  },
  {
    keywords: ["lint", "format", "prettier", "eslint", "check", "validate", "audit"],
    verb: "代码检查",
    kind: "other",
  },
  {
    keywords: [],  // empty array = catch-all, never matches by keyword
    verb: "调用工具",
    kind: "other",
  },
];

export function classifyTool(tool: string): ToolCategory {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") return TOOL_CATEGORIES[TOOL_CATEGORIES.length - 1]; // catch-all
  for (const cat of TOOL_CATEGORIES) {
    if (cat.keywords.length === 0) continue; // skip catch-all in normal loop
    for (const kw of cat.keywords) {
      if (t.includes(kw)) return cat;
    }
  }
  return TOOL_CATEGORIES[TOOL_CATEGORIES.length - 1]; // catch-all
}

/**
 * Infer the tool verb from args shape when the tool name is a
 * placeholder ("tool"). This handles the common LLM pattern where
 * `tool="tool"` but carries real args in the payload.
 */
export function inferVerbFromArgs(argsJson?: string): string | null {
  if (!argsJson) return null;
  try {
    const parsed = JSON.parse(argsJson);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      if ("command" in parsed || "cmd" in parsed || "script" in parsed) return "执行命令";
      if ("path" in parsed || "file_path" in parsed || "directory" in parsed || "file" in parsed) return "操作文件";
      if ("url" in parsed || "uri" in parsed) return "获取网页";
      if ("query" in parsed || "pattern" in parsed || "q" in parsed) return "搜索";
      if ("code" in parsed) return "执行代码";
    }
  } catch { /* 流式中间态的 argsJson 必然是残缺的：解析失败是预期路径，降级即可 */ }
  return null;
}

/**
 * Chinese action phrase for a tool name.
 *
 * Resolution order:
 *   1. Look up tool name in TOOL_CATEGORIES → return mapped verb
 *   2. Placeholder "tool" → infer from args shape
 *   3. Empty / placeholder + empty args → tool name itself (no "未知操作")
 */
export function toolVerb(tool: string, argsJson?: string): string {
  const t = tool.toLowerCase().trim();
  // `tool` 是 LLM 的占位符；`tool_call` 是运行时的**桥接工具**（模型要调的
  // 工具不在 tools 数组里时，先经它转发，见 directions.rs 的桥接说明）。
  // 两者都不是用户认识的名字，动词退回通用说法，具体调的是谁由
  // `toolPrimaryArg` 从 args.name 里取（「调用工具 execute_code」）。
  if (!t || t === "tool" || t === "tool_call") {
    return inferVerbFromArgs(argsJson) ?? "调用工具";
  }
  // Memory tool: generic "记忆" verb; the concrete action
  // (写入/删除/替换记忆) is shown by toolPrimaryArg below.
  if (t.includes("memory")) return "记忆";
  for (const cat of TOOL_CATEGORIES) {
    if (cat.keywords.length === 0) continue;
    for (const kw of cat.keywords) {
      if (t.includes(kw)) return cat.verb;
    }
  }
  // Last resort — return the literal tool name
  return tool;
}

/**
 * Extract a human-readable one-line result summary from the tool's
 * output. Everything is a user-visible success/error summary — never
 * raw JSON or wrapper noise.
 *
 * Returns null when no meaningful result can be extracted (the row
 * simply doesn't show a result peek).
 */
export function toolResultPeek(output?: string, argsJson?: string): string | null {
  if (!output) return null;

  // Peel JSON wrapper
  let text = output;
  if (text.trimStart().startsWith("{")) {
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed === "object") {
        // Error field first (tool errored)
        if (typeof parsed.error === "string" && parsed.error.length > 0) {
          return parsed.error.length > 120 ? parsed.error.slice(0, 117) + "…" : parsed.error;
        }
        // Stderr with content (actual execution failure)
        if (typeof parsed.stderr === "string" && parsed.stderr.trim().length > 0) {
          const first = parsed.stderr.trim().split(/\r?\n/)[0];
          return first.length > 120 ? first.slice(0, 117) + "…" : first;
        }
        // Stdout / result
        const inner = (typeof parsed.stdout === "string" && parsed.stdout) ||
                      (typeof parsed.result === "string" && parsed.result) ||
                      (typeof parsed.output === "string" && parsed.output) ||
                      (typeof parsed.message === "string" && parsed.message) ||
                      "";
        if (inner) {
          text = inner;
          // Recurse in case the inner text is still wrapped
          const recursed = toolResultPeek(text, argsJson);
          if (recursed) return recursed;
          return null;
        }
        // Exit code != 0 without stderr
        if (typeof parsed.exit_code === "number" && parsed.exit_code !== 0) {
          return `退出码 ${parsed.exit_code}`;
        }
        // Success with no stdout
        if (typeof parsed.exit_code === "number" && parsed.exit_code === 0) {
          const std = typeof parsed.stdout === "string" ? parsed.stdout.trim() : "";
          if (std && std.length > 0) {
            text = std;
          } else {
            return null; // no output, nothing to show
          }
        }
      }
    } catch { /* 流式中间态的 argsJson 必然是残缺的：解析失败是预期路径，降级即可 */ }
  }

  const trimmed = text.trim();
  if (!trimmed) return null;

  // "OK -> D:\path\to\file.pptx | size: 33596 bytes | slides: 3"
  const okArrow = trimmed.match(/^OK\s*->\s*(\S+)/);
  if (okArrow) {
    // Also try to extract "slides: N" or "size: N" suffix
    const detail = trimmed.match(/\|\s*(slides|pages|size):\s*(\d+)/i);
    if (detail) {
      return detail[1].toLowerCase() === "slides" || detail[1].toLowerCase() === "pages"
        ? `已生成 (${detail[2]} ${detail[1] === "slides" ? "页" : "页"})`
        : `已生成 (${detail[2]} ${detail[1].toLowerCase()})`;
    }
    return "已完成";
  }

  // "Wrote 823 bytes to D:\data\report.csv"
  const wroteBytes = trimmed.match(/^(?:Wrote|Written|Saved|Created|Generated)\s+(\d+)\s+bytes?\s+(?:to\s+)?/i);
  if (wroteBytes) return `已写入 ${wroteBytes[1]} bytes`;

  // "Saved D:\path\to\file"  (no byte count)
  const saved = trimmed.match(/^(?:Saved|Wrote|Created|Generated)\s+(?:to\s+)?[`"']?(\S+)/i);
  if (saved) {
    const path = saved[1];
    const filename = path.split(/[\\/]/).pop() ?? path;
    return `已保存 ${filename}`;
  }

  // "saved: D:\path\to\file.xlsx 5632 bytes"
  const savedColon = trimmed.match(/^saved:\s*(\S+)\s+(\d+)\s+bytes/i);
  if (savedColon) return `已写入 ${savedColon[2]} bytes`;

  // First non-empty line, capped at 100 chars
  const firstLine = trimmed.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0);
  if (!firstLine) return null;
  // Don't show raw JSON as result
  if (firstLine.startsWith("{") || firstLine.startsWith("[")) return null;

  return firstLine.length > 100 ? firstLine.slice(0, 97) + "…" : firstLine;
}

/**
 * Primary argument extracted for the row's secondary line. Order:
 *   1. Tool-kind-specific keys (path / command / url / query / …)
 *   2. Stdout-derived hint for tools that don't carry their input in
 *      `args` (e.g. python-pptx / py-runner wrappers that stash the
 *      real command in stdout as JSON)
 *   3. First non-empty string-valued arg
 *
 * `stdoutHint` is the tool's accumulated output. When `args` is empty
 * or has no recognisable command field (the python-pptx case in the
 * "毕业纪念.pptx" screenshot, where args was `{}` and stdout began
 * with `OK -> D:\data\slides.pptx`), we pull a friendly hint from
 * stdout so the row doesn't read as "已执行命令" + blank.
 */
export function toolPrimaryArg(tool: string, argsJson?: string, stdoutHint?: string): string | null {
  let parsed: Record<string, unknown> | null = null;
  if (argsJson) {
    try {
      const v = JSON.parse(argsJson);
      if (v && typeof v === "object" && !Array.isArray(v)) parsed = v as Record<string, unknown>;
    } catch { /* 流式中间态的 argsJson 必然是残缺的：解析失败是预期路径，降级即可 */ }
  }
  const str = (k: string): string | null => {
    if (!parsed) return null;
    const v = parsed[k];
    return typeof v === "string" && v.length > 0 ? v : null;
  };
  const trim = (s: string, n: number) => s.length > n ? s.slice(0, n - 1) + "…" : s;
  const t = tool.toLowerCase();
  // Memory tool: show the action (写入/删除/替换记忆) instead of a
  // generic label. "整理记忆" (auto-archive) is surfaced as a chip
  // from the tool result in the render layer.
  if (t.includes("memory")) {
    const action = str("action");
    const c = str("content");
    const old = str("old_text");
    if (action === "add") return "写入记忆: " + (c ? trim(c, 28) : "");
    if (action === "replace") return "替换记忆: " + (old ? trim(old, 28) : "");
    if (action === "remove") return "删除记忆: " + (old ? trim(old, 28) : "");
    return "记忆操作";
  }
  // Path-based tools
  if (t.includes("read_file") || t.includes("write_file") || t.includes("patch") || t.includes("edit") || t.includes("delete_file") || t.includes("list_files")) {
    return trim(str("path") ?? str("file_path") ?? str("directory") ?? "", 80) || stdoutFallback(t, stdoutHint);
  }
  if (t.includes("terminal") || t.includes("bash") || t.includes("exec") || t.includes("command")) {
    return trim(str("command") ?? str("cmd") ?? str("script") ?? "", 80) || stdoutFallback(t, stdoutHint);
  }
  if (t.includes("python") || t.includes("py_run") || t.includes("run_python")) {
    // Python-style tools usually carry the script under `code` /
    // `script`; if not, the stdout often begins with the interpreter
    // banner or a one-line result.
    const code = str("code") ?? str("script") ?? str("command");
    if (code) {
      // Don't dump a 50-line script — show its first non-empty line.
      const firstLine = code.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0) ?? "";
      return trim(firstLine, 80) || null;
    }
    return stdoutFallback(t, stdoutHint);
  }
  if (t.includes("search_files") || t.includes("search")) {
    const p = str("path") ?? str("directory");
    const q = str("query") ?? str("pattern") ?? str("q");
    if (p && q) return `${trim(p, 40)}  ·  "${trim(q, 30)}"`;
    return trim(p ?? q ?? "", 80) || null;
  }
  if (t.includes("browser") || t.includes("fetch") || t.includes("web") || t.includes("http")) {
    return trim(str("url") ?? str("uri") ?? "", 80) || null;
  }
  if (t.includes("computer_use")) {
    return trim(str("action") ?? "", 40) || null;
  }
  // 字面 `tool` 是 LLM 的占位符——args 实际可能是任何东西。
  // 按 args 形状推断是什么工具，按"command → 终端；path → 文件路径；
  // query → 搜索"等分支走，让"调用工具 + (空白)"这种无法动作的
  // 行彻底消失。
  if (t === "tool" && parsed) {
    const cmd = str("command") ?? str("cmd") ?? str("script");
    if (cmd) return trim(cmd, 80);
    const path = str("path") ?? str("file_path") ?? str("directory") ?? str("file");
    if (path) return trim(path, 80);
    const q = str("query") ?? str("pattern") ?? str("q");
    if (q) return trim(q, 60);
    const url = str("url") ?? str("uri");
    if (url) return trim(url, 80);
    const code = str("code");
    if (code) return trim(code.split(/\r?\n/)[0] ?? "", 80) || null;
  }
  // Fallback: first non-empty string value
  if (parsed) {
    for (const v of Object.values(parsed)) {
      if (typeof v === "string" && v.length > 0) return trim(v, 60);
    }
  }
  return stdoutFallback(t, stdoutHint);
}

/**
 * See `memoryArchiveChip` in ToolEventCard.tsx — same semantics. When a
 * memory operation triggers the backend's auto-archive, the tool result
 * embeds `archived_count` (or `archived_to`); surface that as a chip
 * ("已归档N条" / "整理记忆") so the user sees the "整理记忆" action.
 */
export function memoryArchiveChip(tool: string | undefined, output?: string): string | null {
  if (!tool || !tool.toLowerCase().includes("memory")) return null;
  if (!output) return null;
  const re = /\{[^{}]*\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(output))) {
    try {
      const obj = JSON.parse(m[0]) as Record<string, unknown>;
      if (typeof obj.archived_count === "number") {
        return `已归档${obj.archived_count}条`;
      }
      if (obj.archived_to) return "整理记忆";
    } catch {
      // not JSON — keep scanning
    }
  }
  return null;
}

/**
 * Try to lift a one-line hint from the tool's accumulated output when
 * the args payload is opaque (no recognisable command / path / url
 * field). We recognise two common wrapper patterns:
 *
 *   python-pptx style:  `OK -> D:\path\to\file.pptx | size: 33596 bytes | slides: 3`
 *   json tool wrapper:  `{"result":"…","exit_code":0}`  (already unwrapped earlier)
 *   generic tail:       first 80 chars of the first non-empty stdout line
 *
 * Returns null if stdout is empty or starts with no recognisable signal —
 * the caller falls back to a tool-kind-agnostic label.
 */
export function stdoutFallback(toolLower: string, stdout?: string): string | null {
  if (!stdout) return null;
  // First, peel off common JSON wrappers the backend uses for tool
  // results (`{"exit_code":0,"interpreter":"python","language":"python",
  // "stderr":"","stdout":"saved: D:\\qcc\\发票汇总.xls 5632 bytes\r\n"}`).
  // Without this, the row reads as "已执行 {"exit_code":0,…}" —
  // pure JSON noise that the user cannot act on. We pull the
  // human-readable `stdout` (or `result`) out and recurse so
  // `OK -> path` / `Saved path` patterns still match.
  if (stdout.trimStart().startsWith("{")) {
    try {
      const parsed = JSON.parse(stdout);
      if (parsed && typeof parsed === "object") {
        const inner =
          (typeof parsed.stdout === "string" && parsed.stdout) ||
          (typeof parsed.result === "string" && parsed.result) ||
          (typeof parsed.output === "string" && parsed.output) ||
          (typeof parsed.message === "string" && parsed.message) ||
          "";
        if (inner) {
          const recursed = stdoutFallback(toolLower, inner);
          if (recursed) return recursed;
        }
      }
    } catch {
      // not JSON, fall through
    }
  }
  const trimmed = stdout.trim();
  if (!trimmed) return null;
  // python-pptx / generic "OK ->" preamble
  const okArrow = trimmed.match(/^OK\s*->\s*(\S+)/);
  if (okArrow && okArrow[1]) return okArrow[1];
  // Wrote / Saved / Created / Generated <filepath>
  const verbArrow = trimmed.match(/^(?:Wrote|Saved|Created|Generated|Written)\s+(?:to\s+)?[`"']?(\S+?)[`"']?\s*(?:\||$)/i);
  if (verbArrow && verbArrow[1]) return verbArrow[1];
  // "saved: D:\data\report.xls 5632 bytes"  — the python-runner
  // wrapper used by the qcc/invoice-aggregation flow. Lift the path.
  const saved = trimmed.match(/^saved:\s*(\S+?\.xlsx?)\b/i);
  if (saved && saved[1]) return saved[1];
  // First non-empty line, capped at 80 chars. We deliberately don't
  // try to parse JSON here — `LiveTerminal` already unwraps the
  // {"result":"…"} wrapper for the body, so by the time we'd see it
  // it's either clean prose or the original JSON dump.
  const firstLine = trimmed.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0);
  if (!firstLine) return null;
  // Last-resort: if the first non-empty line is itself a JSON
  // object, don't surface the raw JSON to the user. Render a short
  // summary instead. This is the "out of unwraps" case.
  if (firstLine.startsWith("{") || firstLine.startsWith("[")) {
    // 第一行还是 JSON wrapper（peel 没解开）——返回 null 而不是
    // 占位"已执行"，否则会出现"调用工具 已执行"这种啥也没说
    // 的行。让 verb slot 单独显示，由 verb 自己处理 fallback。
    return null;
  }
  return firstLine.length > 80 ? firstLine.slice(0, 79) + "…" : firstLine;
}

/**
 * WorkBuddy-style tool row:
 *
 *   [icon] 已读取 F:\<workspace>\...  ✅
 *   [icon] 已搜索 .tsx · "query"  ✅
 *   [icon] 已执行命令 cargo build  🔄
 *
 * Single compact line per tool, status badge on right.
 * Click to expand/collapse the terminal output body.
 */
/**
 * Extract a human-readable one-line summary from a tool error payload.
 * Handles common wrapper formats so the user never sees raw JSON like
 * `{"error":"Tool execution failed: Failed to connect to Chrome CDP..."}`
 * in the tool row.
 */
export function summarizeError(output?: string): string | null {
  if (!output) return null;
  const trimmed = output.trim();
  // Already plain text (no JSON wrapper)
  if (!trimmed.startsWith("{")) return trimmed.length > 120 ? trimmed.slice(0, 117) + "…" : trimmed;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.error === "string") return parsed.error;
    if (typeof parsed.message === "string") return parsed.message;
    if (typeof parsed.stderr === "string") return parsed.stderr;
    // Nested wrapper: {"result":"...","exit_code":1} — not really an error shape
    if (typeof parsed.result === "string") return null;
    return null;
  } catch {
    return null;
  }
}
export function unwrapToolOutput(raw: string): string {
  if (!raw) return raw;
  const trimmed = raw.trim();
  if (!trimmed.startsWith("{")) return raw;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.result === "string") return parsed.result;
    if (typeof parsed.error === "string") return parsed.error;
    if (typeof parsed.output === "string") return parsed.output;
    // Fall back to raw if no known field
    return raw;
  } catch {
    return raw;
  }
}
export function toolIcon(tool: string) {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") return Wrench;

  const cat = classifyTool(t);
  switch (cat.kind) {
    case "shell":
    case "install":
    case "test":
      return Terminal;
    case "read":
    case "parse":
      return Eye;
    case "write":
      return FilePlus;
    case "edit":
      return Edit3;
    case "delete":
      return Trash2;
    case "search-web":
      return Search;
    case "search-file":
      return FileSearch;
    case "list":
      return FolderOpen;
    case "fetch":
    case "network":
      return Globe;
    case "image":
      return Eye;
    case "code":
      return Wrench;
    case "git":
      return GitBranch;
    case "build":
    case "deploy":
      return Hammer;
    case "db":
      return Database;
    case "desktop":
      return Bot;
    case "reason":
      return Brain;
    case "ask":
      return HelpCircle;
    case "clipboard":
      return FileText;
    default:
      return Wrench;
  }
}

/**
 * Tool "kind" — drives the row's `data-tool-kind` attribute.
 * Uses the comprehensive TOOL_CATEGORIES system.
 */
export function toolKind(tool: string): string {
  const t = tool.toLowerCase().trim();
  if (!t || t === "tool") return "tool";
  const cat = classifyTool(t);
  return cat.kind;
}
export function summarizeArgs(args: unknown): string {
  if (args == null) return "（无参数）";
  if (typeof args === "string") {
    return args.length > 600 ? args.slice(0, 600) + "\n…" : args;
  }
  try {
    const s = JSON.stringify(args, null, 2);
    return s.length > 600 ? s.slice(0, 600) + "\n…" : s;
  } catch {
    return String(args);
  }
}

/** Format a flat object as human-readable "key: value, key: value" brief.
 *  Nested objects / arrays truncate to JSON.stringify single-line.
 *  Used by approval batch items to avoid raw JSON dump in the UI. */
export function formatArgsBrief(args: unknown): string {
  if (args == null) return "";
  if (typeof args !== "object" || Array.isArray(args)) {
    return String(args).length > 120 ? String(args).slice(0, 120) + "…" : String(args);
  }
  const a = args as Record<string, unknown>;
  const parts: string[] = [];
  for (const [k, v] of Object.entries(a)) {
    if (v == null) continue;
    const sv = typeof v === "object" ? JSON.stringify(v) : String(v);
    const trimmed = sv.length > 80 ? sv.slice(0, 80) + "…" : sv;
    parts.push(`${k}: ${trimmed}`);
    if (parts.length >= 6) { parts.push("…"); break; } // cap entries
  }
  return parts.join(",  ");
}

/**
 * Render tool args as a clean key→value list instead of a raw
 * JSON code block.  Objects become aligned rows; primitives /
 * arrays fall back to `summarizeArgs` inside a muted pre block.
 */
/**
 * Generate a short human-readable description from tool args so the user
 * immediately understands what the pending operation will do — no need to
 * decode raw JSON.
 *
 * Examples:
 *   computer_use { action:"key", chord:"win" }  →  "按下 Win 键"
 *   computer_use { action:"type", text:"hello" } → "输入文本 "hello""
 *   bash { command:"ls -la" }                →  "运行命令: ls -la"
 *   write_file { path:"/foo/bar.rs" }        →  "写入文件: /foo/bar.rs"
 */
export function describeToolArgs(tool: string, args: unknown): string {
  if (args == null) return "（无参数）";
  if (typeof args !== "object" || Array.isArray(args)) {
    return summarizeArgs(args);
  }
  const a = args as Record<string, unknown>;

  // storage_clean: 清理临时文件
  if (tool === "storage_clean") {
    const paths = Array.isArray(a.paths) ? a.paths : [];
    const mode = typeof a.mode === "string" ? a.mode : "trash";
    const modeText = mode === "trash" ? "移回收站" : "永久删除";
    if (paths.length === 0) return `${modeText}文件`;
    if (paths.length === 1) {
      const p = String(paths[0]);
      const name = p.split(/[/\\]/).pop() || p;
      return `${modeText} "${name}"`;
    }
    return `${modeText} ${paths.length} 个文件/文件夹`;
  }

  // Tool-kind-specific snippets
  if (tool === "computer_use" || tool === "computer") {
    const action = a.action;
    if (action === "key" && typeof a.chord === "string") return `按下 ${a.chord} 键`;
    if (action === "type" && typeof a.text === "string") return `输入文本 "${a.text}"`;
    if (action === "click" && typeof a.x === "number" && typeof a.y === "number") return `点击坐标 (${a.x}, ${a.y})`;
  }
  if (tool === "terminal" || tool === "bash") {
    const cmd = a.command;
    if (typeof cmd === "string") return describeShellCommand(cmd).text;
  }
  if (tool === "write_file" || tool === "patch") {
    const p = a.path;
    if (typeof p === "string") {
      const name = p.split(/[/\\]/).pop() || p;
      return `${tool === "patch" ? "修改文件" : "写入文件"} "${name}"`;
    }
  }
  if (tool === "read_file") {
    const p = a.path;
    if (typeof p === "string") {
      const name = p.split(/[/\\]/).pop() || p;
      return `读取文件 "${name}"`;
    }
  }
  if (tool === "web_search") {
    const q = a.query;
    if (typeof q === "string") return `搜索: ${q.slice(0, 40)}`;
  }
  if (tool === "memory") {
    const action = typeof a.action === "string" ? a.action : "";
    const target = typeof a.target === "string" ? a.target : "";
    const targetText = target === "user" ? "用户偏好" : "工作记忆";
    if (action === "add") return `写入${targetText}`;
    if (action === "replace") return `编辑${targetText}`;
    if (action === "remove") return `删除${targetText}`;
    return `记忆操作: ${action || "未知"}`;
  }
  // 2026-08-29: custom app management — the batch approval card and
  // tool rows previously dumped raw args JSON here (user feedback:
  // "审批卡片中也要中文显示大概要运行什么"). Map the action verb to
  // Chinese and surface the app id so the user can tell WHICH app the
  // call touches — critical when a batch contains two same-tool items
  // that only differ by target app.
  if (tool.includes("custom_app")) {
    const action = typeof a.action === "string" ? a.action : "";
    const appId =
      (typeof a.app_id === "string" && a.app_id) ||
      (typeof a.id === "string" && a.id) ||
      (typeof a.name === "string" && a.name) ||
      "";
    const appLabel = appId ? ` "${appId}"` : "";
    const ACTION_VERBS: Record<string, string> = {
      list: "列出已安装应用",
      get: "查看应用",
      create: "创建应用",
      update: "更新应用",
      patch: "修改应用",
      package: "打包应用",
      install: "安装应用",
      uninstall: "卸载应用",
      delete: "删除应用",
      run: "运行应用",
      dev: "开发模式运行应用",
    };
    const verb = ACTION_VERBS[action];
    if (verb) return `${verb}${appLabel}`;
    return `应用管理操作 (${action || "未知"})${appLabel}`;
  }
  return summarizeArgs(args);
}
