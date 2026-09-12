// AppMentionInput — chat textarea with @app / @work mentions.
//
// Architecture: a transparent <textarea> sits on top of a backdrop <div> that
// renders the same text with "@app <name>" / "@work <name>" segments coloured.
// The textarea owns input/caret; the backdrop owns colour. When the user types
// "@app" an inline card lists the installed custom apps; "@work" lists the
// workspaces. Picking one inserts a resolved "<trigger> <name> " token.
// No extra deps, no contentEditable.

import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Plus } from "lucide-react";
import type { CustomAppSummary } from "../lib/customAppApi";
import type { WorkspaceEntry } from "../types";
import {
  MENTION_COLOR,
  MENTION_COLOR_WORK,
  MENTION_TRIGGER_APP,
  MENTION_TRIGGER_WORK,
  applyMention,
  detectActiveMention,
  splitMentions,
  type ActiveMention,
} from "../lib/appMention";

// 虚拟"创建新应用"快捷项 id — 始终作为 @app 面板的第一项。
// 选中后插入 "@app 创建新应用 应用要求："，引导用户描述需求，
// agent 收到后按 custom-app-development 规范走创建流程（需求梳理→方案规划→开发→验证）。
const CREATE_NEW_APP_ID = "__create_new_app__";

// 虚拟"创建工作台"快捷项 id — 始终作为 @work 面板的第一项。
// 选中后插入 "@work 创建工作台 需求："，引导用户描述需求，
// agent 收到后按 workspace-development 规范走建设流程（schema 提案→确认→build→测试→迭代）。
const CREATE_NEW_WORK_ID = "__create_new_work__";

interface Props {
  value: string;
  onChange: (v: string) => void;
  /** Parent's keydown handler (Enter → submit). We intercept when the popup is open. */
  onKeyDown: (e: React.KeyboardEvent) => void;
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  apps: CustomAppSummary[];
  /** 已建工作台（@work 联想列表）。 */
  workbenches?: WorkspaceEntry[];
  disabled?: boolean;
  isStreaming?: boolean;
  placeholder?: string;
  onFilesAttach?: (files: File[]) => void;
}

type ActiveMentionWithTrigger = ActiveMention & { trigger: string };

interface MentionItemView {
  id: string;
  name: string;
  description?: string;
  isCreate: boolean;
}

function AppMentionInputImpl({
  value,
  onChange,
  onKeyDown,
  textareaRef,
  apps,
  workbenches = [],
  disabled,
  isStreaming,
  placeholder,
  onFilesAttach,
}: Props) {
  const [active, setActive] = useState<ActiveMentionWithTrigger | null>(null);
  const [selIdx, setSelIdx] = useState(0);
  const backdropRef = useRef<HTMLDivElement>(null);
  const pendingCaret = useRef<number | null>(null);
  const blurTimer = useRef<number | null>(null);

  const isWork = active?.trigger === MENTION_TRIGGER_WORK;

  const filtered = useMemo(() => {
    const q = (active?.query ?? "").trim().toLowerCase();
    if (isWork) {
      if (!q) return workbenches;
      return workbenches.filter((w) => w.name.toLowerCase().includes(q));
    }
    if (!q) return apps;
    return apps.filter((a) => a.name.toLowerCase().includes(q));
  }, [apps, workbenches, active, isWork]);

  // 列表首项恒为虚拟创建快捷项，其余为已装应用 / 已建工作台。
  const items = useMemo<MentionItemView[]>(() => {
    const head = isWork
      ? { id: CREATE_NEW_WORK_ID, name: "创建工作台 需求：", description: "让 AI 从零建设一个新的工作台", isCreate: true }
      : { id: CREATE_NEW_APP_ID, name: "创建新应用 应用要求：", description: "让 AI 从零开发一个新的桌面应用", isCreate: true };
    const rest = (isWork ? filtered : filtered).map((it) => ({
      id: it.id,
      name: it.name,
      description: "description" in it ? (it.description as string | undefined) : undefined,
      isCreate: false,
    }));
    return [head, ...rest];
  }, [filtered, isWork]);

  useEffect(() => {
    if (pendingCaret.current != null && textareaRef.current) {
      const pos = pendingCaret.current;
      textareaRef.current.focus();
      textareaRef.current.setSelectionRange(pos, pos);
      pendingCaret.current = null;
    }
  }, [value, textareaRef]);

  const selectItem = (item: MentionItemView) => {
    if (!active) return;
    const { text, caret } = applyMention(value, active, active.trigger, item.name);
    pendingCaret.current = caret;
    onChange(text);
    setActive(null);
  };

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const v = e.target.value;
    const caret = e.target.selectionStart ?? v.length;
    onChange(v);
    // 两个 trigger 都检测，取更靠近光标的那个。
    const appM = detectActiveMention(v, caret, MENTION_TRIGGER_APP);
    const workM = detectActiveMention(v, caret, MENTION_TRIGGER_WORK);
    let next: ActiveMentionWithTrigger | null = null;
    if (appM && workM) {
      next =
        appM.start >= workM.start
          ? { ...appM, trigger: MENTION_TRIGGER_APP }
          : { ...workM, trigger: MENTION_TRIGGER_WORK };
    } else if (appM) {
      next = { ...appM, trigger: MENTION_TRIGGER_APP };
    } else if (workM) {
      next = { ...workM, trigger: MENTION_TRIGGER_WORK };
    }
    setActive(next);
    setSelIdx(0);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (active && items.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelIdx((i) => (i + 1) % items.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelIdx((i) => (i - 1 + items.length) % items.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        selectItem(items[selIdx]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setActive(null);
        return;
      }
    }
    onKeyDown(e);
  };

  const handleScroll = () => {
    if (backdropRef.current && textareaRef.current) {
      backdropRef.current.scrollTop = textareaRef.current.scrollTop;
    }
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(e.clipboardData?.files ?? []);
    if (files.length > 0) {
      e.preventDefault();
      onFilesAttach?.(files);
    }
  };

  const handleFocus = () => {
    if (blurTimer.current) {
      clearTimeout(blurTimer.current);
      blurTimer.current = null;
    }
  };
  const handleBlur = () => {
    blurTimer.current = window.setTimeout(() => setActive(null), 120);
  };

  const backdropText = value.endsWith("\n") ? value + " " : value;
  const segments = splitMentions(backdropText, [MENTION_TRIGGER_APP, MENTION_TRIGGER_WORK]);

  const panelTitle = isWork
    ? `工作台 · ${workbenches.length}`
    : `自定义应用 · ${apps.length}`;

  return (
    <div className="relative">
      {/* Highlight backdrop — mirrors the textarea's box & font metrics. */}
      <div
        ref={backdropRef}
        aria-hidden
        className="absolute inset-0 pointer-events-none overflow-hidden whitespace-pre-wrap break-words px-4 pt-3.5 pb-1.5"
        style={{
          fontSize: "var(--font-size-body)",
          lineHeight: 1.5,
          color: "var(--color-text-primary)",
          boxSizing: "border-box",
        }}
      >
        {segments.map((seg, i) =>
          seg.mention ? (
            <span
              key={i}
              style={{ color: seg.trigger === MENTION_TRIGGER_WORK ? MENTION_COLOR_WORK : MENTION_COLOR }}
            >
              {seg.text}
            </span>
          ) : (
            <span key={i}>{seg.text}</span>
          ),
        )}
        {value.length === 0 && (
          <span className="text-text-muted">{placeholder}</span>
        )}
      </div>

      {/* Real textarea — transparent text, visible caret. */}
      <textarea
        ref={textareaRef}
        value={value}
        onChange={handleChange}
        onKeyDown={handleKeyDown}
        onPaste={handlePaste}
        onScroll={handleScroll}
        onFocus={handleFocus}
        onBlur={handleBlur}
        disabled={disabled}
        placeholder={isStreaming ? "输入消息加入队列..." : placeholder}
        data-no-border
        className="relative z-[1] w-full bg-transparent resize-none outline-none px-4 pt-3.5 pb-1.5 min-h-[72px]"
        style={{
          fontSize: "var(--font-size-body)",
          lineHeight: 1.5,
          border: "none",
          borderRadius: 0,
          boxShadow: "none",
          background: "transparent",
          color: "transparent",
          caretColor: "var(--color-text-primary)",
        }}
        rows={3}
      />

      {/* mention card */}
      {active && (
        <div
          className="absolute bottom-full left-0 mb-2 z-20 rounded-xl overflow-hidden"
          style={{
            width: 320,
            maxWidth: "calc(100% - 1rem)",
            maxHeight: 260,
            background: "var(--color-bg-elevated)",
            border: "1px solid var(--color-border-primary)",
            boxShadow: "var(--shadow-pop)",
          }}
        >
          <div
            className="px-3 py-2 text-[11px] font-medium uppercase tracking-wide"
            style={{ color: "var(--color-text-muted)", borderBottom: "1px solid var(--color-border-primary)" }}
          >
            {panelTitle}
          </div>
          <div className="overflow-y-auto" style={{ maxHeight: 212 }}>
            {items.map((item, i) => {
              return (
                <button
                  key={item.id}
                  type="button"
                  onMouseDown={(e) => e.preventDefault()}
                  onMouseEnter={() => setSelIdx(i)}
                  onClick={() => selectItem(item)}
                  className="w-full text-left px-3 py-2 flex flex-col gap-0.5 transition-colors"
                  style={{
                    background: i === selIdx ? "var(--color-bg-hover)" : "transparent",
                    ...(item.isCreate ? { borderBottom: "1px solid var(--color-border-primary)" } : {}),
                  }}
                >
                  <span
                    className="flex items-center gap-1.5 text-[13px] font-medium"
                    style={{ color: item.isCreate ? "var(--color-accent, #b8860b)" : "var(--color-text-primary)" }}
                  >
                    {item.isCreate && <Plus className="icon-md" />}
                    {item.isCreate ? (isWork ? "创建工作台" : "创建新应用") : item.name}
                  </span>
                  {item.description && (
                    <span
                      className="text-[12px] truncate"
                      style={{ color: "var(--color-text-muted)" }}
                    >
                      {item.description}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

export const AppMentionInput = memo(AppMentionInputImpl);
