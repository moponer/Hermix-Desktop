// ToolOutputPreviews — self-contained inline previews for cua_* tool
// results (screenshot / window list / UI tree / OCR / etc). Extracted
// prop-driven leaf components: they parse a JSON `output` string and
// render; they depend only on React hooks + DOM, no MessageBubble internals.
import { useMemo } from "react";
import { openImageViewer } from "../imageViewer";

function ScreenshotPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.png_base64 === "string" && v.png_base64.length > 0
      ) {
        return v as {
          png_base64: string;
          width?: number;
          height?: number;
          bytes?: number;
          monitor_name?: string;
        };
      }
    } catch {
      // not JSON, or wrong shape — fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const w = typeof parsed.width === "number" ? parsed.width : 0;
  const h = typeof parsed.height === "number" ? parsed.height : 0;
  const bytes = typeof parsed.bytes === "number" ? parsed.bytes : 0;
  const kb = (bytes / 1024).toFixed(0);
  const monitorLabel = parsed.monitor_name ? ` · ${parsed.monitor_name}` : "";
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <img
        src={`data:image/png;base64,${parsed.png_base64}`}
        alt={w && h ? `桌面截图 ${w}×${h}` : "桌面截图"}
        className="w-full h-auto max-h-[480px] object-contain bg-black/40"
        draggable={false}
        onClick={() =>
          openImageViewer({
            src: `data:image/png;base64,${parsed.png_base64}`,
            name: w && h ? `桌面截图 ${w}×${h}` : "桌面截图",
          })
        }
        style={{ cursor: "pointer" }}
      />
      <div className="flex items-center justify-between gap-2 px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-t border-border-primary/30">
        <span className="font-mono">{w}×{h}{monitorLabel}</span>
        <span>{kb} KB</span>
      </div>
    </div>
  );
}

/**
 * Inline preview for `cua_list_windows` tool results.
 *
 * Backend returns:
 *   { matched, truncated, windows: [{hwnd, title, class_name,
 *     process_id, is_visible, rect:{x,y,width,height}}, ...] }
 *
 * The list is the primary deliverable — like the screenshot, we
 * always render it inline (no expand-click required). The
 * truncated/matched count sits in the footer so the user can
 * tell at a glance whether the agent got back a full census
 * or hit the cap.
 */
function WindowListPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        Array.isArray(v.windows)
      ) {
        return v as {
          matched: number;
          truncated: boolean;
          windows: Array<{
            hwnd: number;
            title: string;
            class_name: string;
            process_id: number;
            is_visible: boolean;
            rect: { x: number; y: number; width: number; height: number };
          }>;
        };
      }
    } catch {
      // not JSON, or wrong shape — fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { matched, truncated, windows } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>顶层窗口</span>
        <span className="font-mono">
          {matched} 个{truncated ? "（已截断）" : ""}
        </span>
      </div>
      {windows.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          没有匹配的窗口
        </div>
      ) : (
        <ul className="divide-y divide-border-primary/30 max-h-72 overflow-auto">
          {windows.map((w, i) => (
            <li
              key={`${w.hwnd}-${i}`}
              className="px-2.5 py-1.5 flex items-center gap-2 text-[12px] hover:bg-bg-tertiary/40"
              data-visible={w.is_visible ? "true" : "false"}
            >
              {/* Visibility dot */}
              <span
                className={`shrink-0 w-1.5 h-1.5 rounded-full ${
                  w.is_visible ? "bg-success" : "bg-text-faint/50"
                }`}
                title={w.is_visible ? "可见" : "隐藏"}
              />
              {/* Title + class */}
              <div className="flex-1 min-w-0">
                <div
                  className="truncate text-text-primary"
                  title={w.title || "(无标题)"}
                >
                  {w.title || <span className="italic text-text-faint">（无标题）</span>}
                </div>
                <div className="flex items-center gap-1.5 text-[10.5px] text-text-faint mt-0.5">
                  <span className="font-mono">{w.class_name || "—"}</span>
                  <span>·</span>
                  <span className="font-mono">pid {w.process_id}</span>
                </div>
              </div>
              {/* HWND + rect */}
              <div className="shrink-0 text-right font-mono text-[10.5px] text-text-faint leading-tight">
                <div title="窗口句柄（可传给 cua_click_window）">
                  hwnd <span className="text-text-secondary">0x{w.hwnd.toString(16)}</span>
                </div>
                <div title="GetWindowRect">
                  {w.rect.width}×{w.rect.height}
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * Inline preview for `cua_window_at_point` tool results.
 *
 * Backend returns:  { x, y, window: WindowInfo | null }
 *
 * The hit-window gets a compact two-line summary so the user can
 * see "the LLM asked about pixel (x,y) and learned it was on app
 * Z" without expanding the row.
 */
function WindowAtPointPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.x === "number" && typeof v.y === "number"
      ) {
        return v as {
          x: number;
          y: number;
          window: null | {
            hwnd: number;
            title: string;
            class_name: string;
            process_id: number;
            is_visible: boolean;
            rect: { x: number; y: number; width: number; height: number };
          };
        };
      }
    } catch {
      // not JSON, or wrong shape — fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { x, y, window: w } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2.5 py-1.5 text-[12px] flex items-center gap-2">
        <span className="text-text-faint">点</span>
        <span className="font-mono text-text-secondary">({x}, {y})</span>
        <span className="text-text-faint">·</span>
        {w ? (
          <span className="truncate text-text-primary" title={w.title || w.class_name}>
            {w.title || <span className="italic text-text-faint">（无标题）</span>}
            <span className="ml-1.5 text-text-faint font-mono text-[10.5px]">
              {w.class_name} · pid {w.process_id}
            </span>
          </span>
        ) : (
          <span className="italic text-text-faint">没有窗口</span>
        )}
      </div>
    </div>
  );
}

/**
 * Inline tree preview for `cua_ui_tree` tool results.
 *
 * Backend returns:
 *   { max_depth, element_count, elements: [UiElement, ...] }
 *
 * Each UiElement carries `name`, `control_type`, `class_name`,
 * `automation_id`, `rect`, `depth`, `index`, etc. We render the
 * flat Vec<UiElement> as an indented tree using `depth` as the
 * nesting cue — the backend walks the control-view depth-first
 * so siblings are contiguous and `depth` jumps by exactly 1.
 *
 * Why this is always inline (no expand-click): the whole point
 * of `cua_ui_tree` is to let the user eyeball what UIA sees. A
 * collapsed pre block would defeat the purpose.
 */
function UiTreePreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        Array.isArray(v.elements)
      ) {
        return v as {
          max_depth: number;
          element_count: number;
          elements: Array<{
            name: string;
            class_name: string;
            automation_id: string;
            control_type: string;
            localized_control_type: string;
            rect: { x: number; y: number; width: number; height: number };
            is_enabled: boolean;
            is_offscreen: boolean;
            has_keyboard_focus: boolean;
            depth: number;
            index: number;
            process_id: number;
            framework_id: string;
          }>;
        };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { max_depth, elements, element_count } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>UI 树（UIA control view）</span>
        <span className="font-mono">
          {element_count} 个 / depth ≤ {max_depth}
        </span>
      </div>
      {elements.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          空树
        </div>
      ) : (
        <ul className="divide-y divide-border-primary/30 max-h-80 overflow-auto font-mono text-[11.5px]">
          {elements.map((el) => {
            const indentPx = Math.min(el.depth, 8) * 12;
            const disabledCls = el.is_enabled ? "" : "text-text-faint italic";
            const focusMarker = el.has_keyboard_focus ? (
              <span className="ml-1.5 text-text-secondary" title="当前键盘焦点">●</span>
            ) : null;
            const offscreenMarker = el.is_offscreen ? (
              <span className="ml-1 text-text-faint/60" title="离屏">⊘</span>
            ) : null;
            const rect = el.rect;
            const rectLabel = rect.width > 0 || rect.height > 0
              ? `${rect.width}×${rect.height} @ (${rect.x},${rect.y})`
              : "—";
            return (
              <li
                key={`${el.index}-${el.depth}-${el.name}`}
                className={`px-2 py-1 hover:bg-bg-tertiary/40 ${disabledCls}`}
                style={{ paddingLeft: 8 + indentPx }}
              >
                <div className="flex items-baseline gap-1.5 truncate">
                  <span className="shrink-0 text-text-faint text-[10px]">
                    [{el.control_type}]
                  </span>
                  <span className="truncate text-text-primary" title={el.name || "(未命名)"}>
                    {el.name || <span className="italic text-text-faint">（未命名）</span>}
                    {focusMarker}
                    {offscreenMarker}
                  </span>
                </div>
                <div className="flex items-center gap-2 mt-0.5 text-[10px] text-text-faint">
                  {el.automation_id && (
                    <span title="automation_id">
                      id=<span className="text-text-secondary">{el.automation_id}</span>
                    </span>
                  )}
                  {el.class_name && (
                    <span title="class_name">{el.class_name}</span>
                  )}
                  {el.framework_id && (
                    <span title="framework_id">{el.framework_id}</span>
                  )}
                  <span className="ml-auto" title="BoundingRectangle">{rectLabel}</span>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/**
 * Inline flat-list preview for `cua_ui_find` tool results.
 *
 * Backend returns:
 *   { count, truncated, elements: [UiElement, ...] }
 *
 * `cua_ui_find` returns a flat list (search hit list, not a
 * tree) so we use the same row layout as `UiTreePreview` but
 * without the depth indent and with the `truncated` flag in
 * the footer.
 */
function UiFindPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        Array.isArray(v.elements)
      ) {
        return v as {
          count: number;
          truncated: boolean;
          elements: Array<{
            name: string;
            class_name: string;
            automation_id: string;
            control_type: string;
            rect: { x: number; y: number; width: number; height: number };
            is_enabled: boolean;
            has_keyboard_focus: boolean;
            process_id: number;
          }>;
        };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { count, truncated, elements } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2 py-1 text-[11px] text-text-faint bg-bg-secondary/40 border-b border-border-primary/30 flex items-center justify-between">
        <span>UIA 查找结果</span>
        <span className="font-mono">
          {count} 个{truncated ? "（已截断）" : ""}
        </span>
      </div>
      {elements.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-faint text-center italic">
          未找到匹配元素
        </div>
      ) : (
        <ul className="divide-y divide-border-primary/30 max-h-72 overflow-auto">
          {elements.map((el, i) => {
            const focusMarker = el.has_keyboard_focus ? "●" : "";
            const disabledCls = el.is_enabled ? "" : "text-text-faint italic";
            return (
              <li
                key={`${el.automation_id}-${i}-${el.rect.x}-${el.rect.y}`}
                className={`px-2.5 py-1.5 hover:bg-bg-tertiary/40 text-[12px] ${disabledCls}`}
              >
                <div className="flex items-baseline gap-1.5 truncate">
                  <span className="shrink-0 text-text-faint font-mono text-[10px]">
                    [{el.control_type}]
                  </span>
                  <span className="truncate text-text-primary" title={el.name || "(未命名)"}>
                    {el.name || <span className="italic text-text-faint">（未命名）</span>}
                    {focusMarker && (
                      <span className="ml-1 text-text-secondary" title="当前焦点">{focusMarker}</span>
                    )}
                  </span>
                </div>
                <div className="flex items-center gap-2 mt-0.5 text-[10.5px] text-text-faint font-mono">
                  {el.automation_id && <span>id={el.automation_id}</span>}
                  {el.class_name && <span>· {el.class_name}</span>}
                  {el.process_id > 0 && <span>· pid {el.process_id}</span>}
                  <span className="ml-auto">
                    {el.rect.width}×{el.rect.height} @ ({el.rect.x},{el.rect.y})
                  </span>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/**
 * Compact summary card for `cua_ui_click` tool results.
 *
 * Backend returns:  { name, clicked_at: {x, y} }
 *
 * The click point is the meat of the result — it's the exact
 * pixel the agent picked from the UIA clickable-point contract.
 * We render that prominently so the user can verify "yes the
 * agent clicked on the OK button at (840, 412)" without
 * expanding the raw JSON.
 */
function UiClickPreview({ output }: { output?: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.name === "string" && v.clicked_at &&
        typeof v.clicked_at.x === "number" && typeof v.clicked_at.y === "number"
      ) {
        return v as { name: string; clicked_at: { x: number; y: number } };
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (!parsed) return null;
  const { name, clicked_at } = parsed;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2.5 py-1.5 text-[12px] flex items-center gap-2">
        <span className="text-text-faint">点击</span>
        <span className="text-text-primary truncate" title={name}>{name}</span>
        <span className="text-text-faint">·</span>
        <span className="font-mono text-text-secondary">
          ({clicked_at.x}, {clicked_at.y})
        </span>
      </div>
    </div>
  );
}

/**
 * Compact one-liner for `cua_ui_focus` and `cua_ui_invoke`
 * tool results. Both return `{ name }` and nothing else
 * meaningful, so we collapse to a single row.
 */
function UiSingleNamePreview({ output, verb }: { output?: string; verb: string }) {
  const parsed = useMemo(() => {
    if (!output) return null;
    try {
      const v = JSON.parse(output);
      if (
        v && typeof v === "object" && !Array.isArray(v) &&
        typeof v.name === "string"
      ) {
        return v.name as string;
      }
    } catch {
      // not JSON, fall through
    }
    return null;
  }, [output]);
  if (parsed === null) return null;
  return (
    <div className="mt-1.5 rounded-md overflow-hidden border border-border-primary/40 bg-bg-secondary/20">
      <div className="px-2.5 py-1.5 text-[12px] flex items-center gap-2">
        <span className="text-text-faint">{verb}</span>
        <span className="text-text-primary truncate" title={parsed}>{parsed}</span>
      </div>
    </div>
  );
}

/**
 * Heuristic: does the tool's primary arg point to a real file on
 * disk we could open?  We keep this conservative — false positives
 * trigger a useless "open" click and add a misleading underline.
 */
function isPathArg(tool: string, argsJson?: string, output?: string): boolean {
  const t = tool.toLowerCase();
  const pathTools = ["read_file", "write_file", "edit_file", "patch", "edit", "delete_file", "create_file", "list_files", "search_files"];
  if (!pathTools.some((p) => t.includes(p))) return false;
  // Must have an actual path-looking string in args or output.
  return extractFirstPath(argsJson ?? "") !== null || extractFirstPath(output ?? "") !== null;
}

const PATH_HINT_RE = /(?:(?:[A-Za-z]:[\\/])|(?:[\\/]))(?:[\w.\-]+[\\/])+[\w.\-]+\.[a-z0-9]{1,5}|(?:[A-Za-z]:[\\/])[\w.\-]+\.[a-z0-9]{1,5}/g;
function extractFirstPath(blob: string): string | null {
  PATH_HINT_RE.lastIndex = 0;
  const m = PATH_HINT_RE.exec(blob);
  return m ? m[0] : null;
}

/**
 * Git-style diff stats from a tool's JSON args.
 *
 * - write_file: additions = new file content line count
 * - patch / edit_file: additions = new_string lines, deletions = old_string lines
 * - everything else: empty stats
 */
function extractLineStats(tool: string, argsJson?: string): { added?: number; removed?: number } {
  if (!argsJson) return {};
  let args: Record<string, unknown>;
  try {
    args = JSON.parse(argsJson);
  } catch {
    return {};
  }
  const t = tool.toLowerCase();
  if (t.includes("write_file") && typeof args.content === "string") {
    return { added: args.content.split("\n").length };
  }
  if ((t.includes("patch") || t.includes("edit_file")) && typeof args.new_string === "string") {
    const added = args.new_string.split("\n").length;
    const removed = typeof args.old_string === "string" ? args.old_string.split("\n").length : 0;
    return { added, removed };
  }
  return {};
}


export {
  ScreenshotPreview,
  WindowListPreview,
  WindowAtPointPreview,
  UiTreePreview,
  UiFindPreview,
  UiClickPreview,
  UiSingleNamePreview,
  // Shared arg-parsing helpers used by both these previews and
  // ToolMessageCard (still in MessageBubble.tsx).
  isPathArg,
  extractFirstPath,
  extractLineStats,
};
