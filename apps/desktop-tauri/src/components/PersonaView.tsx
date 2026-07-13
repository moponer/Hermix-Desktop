import { useEffect, useState, useCallback, useRef, useMemo } from "react";
import {
  UserCog, Sparkles, ScrollText, BookText, IdCard,
  Save, Check, Loader2, Plus, Trash2, RefreshCw, AlertCircle, FileText,
} from "lucide-react";
import { clsx } from "clsx";
import {
  getSoul, putSoul,
  getUserProfile, putUserProfile,
  getMemory, putMemory,
  getPersonaOverrides, putPersonaOverrides,
  listPersonalities, getPersonality, putPersonality, deletePersonality,
  type PersonalitySummary, type PersonaTextFile, type PersonaOverrides,
} from "../api";

// ── types ─────────────────────────────────────────────────────────

type SubTab = "soul" | "system" | "personalities" | "user" | "memory";

interface SubTabDef {
  id: SubTab;
  label: string;
  icon: React.ElementType;
  hint: string;
}

const SUB_TABS: SubTabDef[] = [
  { id: "soul",          label: "灵魂",     icon: Sparkles, hint: "专家的核心信念 — 注入到每轮对话" },
  { id: "system",        label: "系统提示词", icon: ScrollText, hint: "完整覆盖 agent 默认 system message" },
  { id: "personalities", label: "角色",     icon: IdCard,  hint: "可雇佣的人设 (内置 3 个 + 自定义)" },
  { id: "user",          label: "用户画像", icon: UserCog, hint: "你是谁、有什么偏好 — 持续注入" },
  { id: "memory",        label: "记忆",     icon: BookText, hint: "专家自己的笔记 — 跨会话保留" },
];

// ── save status indicator ─────────────────────────────────────────

type SaveStatus =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved"; at: number }
  | { kind: "error"; message: string };

function SaveBadge({ status }: { status: SaveStatus }) {
  if (status.kind === "idle") return null;
  if (status.kind === "saving") {
    return (
      <span className="inline-flex items-center gap-1 text-[11px] text-text-muted">
        <Loader2 size={11} className="animate-spin" /> 保存中…
      </span>
    );
  }
  if (status.kind === "saved") {
    const ago = Math.max(0, Math.round((Date.now() - status.at) / 1000));
    return (
      <span className="inline-flex items-center gap-1 text-[11px] text-success">
        <Check size={11} /> 已保存{ago > 0 ? ` · ${ago}秒前` : ""}
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 text-[11px] text-error">
      <AlertCircle size={11} /> {status.message}
    </span>
  );
}

// ── file editor (textarea + save button) ──────────────────────────

interface FileEditorProps {
  title: string;
  hint: string;
  initial: PersonaTextFile | null;
  onSave: (content: string) => Promise<PersonaTextFile | null>;
  placeholder: string;
  rows?: number;
}

function FileEditor({ title, hint, initial, onSave, placeholder, rows = 18 }: FileEditorProps) {
  const [content, setContent] = useState(initial?.content ?? "");
  const [status, setStatus] = useState<SaveStatus>({ kind: "idle" });
  const dirtyRef = useRef(false);
  const lastSavedRef = useRef(initial?.content ?? "");

  // Reload when the file changes upstream (rare — e.g. after rename).
  useEffect(() => {
    if (!dirtyRef.current) {
      setContent(initial?.content ?? "");
      lastSavedRef.current = initial?.content ?? "";
    }
  }, [initial?.content, initial?.path]);

  // Mark dirty on edits, clear "saved" badge after a beat so it doesn't
  // linger forever.
  useEffect(() => {
    if (content !== lastSavedRef.current) dirtyRef.current = true;
  }, [content]);

  useEffect(() => {
    if (status.kind !== "saved") return;
    const t = setTimeout(() => {
      // Re-render the relative time by bumping setStatus only if still saved
      setStatus((s) => (s.kind === "saved" ? { ...s } : s));
    }, 30_000);
    return () => clearTimeout(t);
  }, [status]);

  const handleSave = useCallback(async () => {
    setStatus({ kind: "saving" });
    const res = await onSave(content);
    if (res) {
      lastSavedRef.current = content;
      dirtyRef.current = false;
      setStatus({ kind: "saved", at: Date.now() });
    } else {
      setStatus({ kind: "error", message: "保存失败,后端未响应" });
    }
  }, [content, onSave]);

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-[14px] font-semibold text-text-primary">{title}</h3>
          <p className="text-[12px] text-text-muted mt-0.5">{hint}</p>
          {initial?.path && !initial.path.startsWith("<") && (
            <p className="text-[10.5px] font-mono text-text-faint mt-1 truncate max-w-xl" title={initial.path}>
              {initial.path}
            </p>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <SaveBadge status={status} />
          <button
            onClick={handleSave}
            disabled={status.kind === "saving" || !dirtyRef.current}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[12.5px] font-medium transition-all disabled:opacity-40 disabled:cursor-not-allowed"
            style={{
              background: dirtyRef.current ? "var(--color-accent)" : "var(--color-bg-tertiary)",
              color: dirtyRef.current ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
            }}
            title="保存修改"
          >
            {status.kind === "saving" ? <Loader2 size={12} className="animate-spin" /> : <Save size={12} />}
            保存
          </button>
        </div>
      </div>
      <textarea
        value={content}
        onChange={(e) => setContent(e.target.value)}
        placeholder={placeholder}
        rows={rows}
        className="w-full rounded-lg px-3.5 py-3 text-[12.5px] font-mono leading-relaxed text-text-primary placeholder:text-text-faint outline-none resize-y"
        style={{
          background: "var(--color-bg-primary)",
          border: "1px solid var(--color-border-secondary)",
          minHeight: 320,
        }}
        spellCheck={false}
      />
    </div>
  );
}

// ── personalities editor (list + textarea) ────────────────────────

function PersonalitiesEditor() {
  const [list, setList] = useState<PersonalitySummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [summary, setSummary] = useState<PersonalitySummary | null>(null);
  const [status, setStatus] = useState<SaveStatus>({ kind: "idle" });
  const [, setLoading] = useState(false);
  const [newName, setNewName] = useState("");
  const [showNew, setShowNew] = useState(false);
  const dirtyRef = useRef(false);
  const lastSavedRef = useRef("");

  const refresh = useCallback(async () => {
    const next = await listPersonalities();
    setList(next);
    if (selected && !next.some((p) => p.name === selected)) {
      setSelected(null);
      setContent("");
    }
  }, [selected]);

  useEffect(() => { refresh(); }, []);

  const load = useCallback(async (name: string) => {
    setLoading(true);
    setSelected(name);
    const file = await getPersonality(name);
    const text = file?.content ?? "";
    setContent(text);
    lastSavedRef.current = text;
    dirtyRef.current = false;
    setSummary(list.find((p) => p.name === name) ?? null);
    setStatus({ kind: "idle" });
    setLoading(false);
  }, [list]);

  useEffect(() => {
    if (content !== lastSavedRef.current) dirtyRef.current = true;
  }, [content]);

  const handleSave = useCallback(async () => {
    if (!selected) return;
    setStatus({ kind: "saving" });
    const res = await putPersonality(selected, content);
    if (res) {
      lastSavedRef.current = content;
      dirtyRef.current = false;
      setStatus({ kind: "saved", at: Date.now() });
      refresh();
    } else {
      setStatus({ kind: "error", message: "保存失败" });
    }
  }, [selected, content, refresh]);

  const handleDelete = useCallback(async () => {
    if (!selected) return;
    const target = list.find((p) => p.name === selected);
    if (!target) return;
    if (target.is_builtin) {
      setStatus({ kind: "error", message: "内置人设不可删除" });
      return;
    }
    if (!confirm(`确认删除人设「${selected}」?`)) return;
    const ok = await deletePersonality(selected);
    if (ok) {
      setSelected(null);
      setContent("");
      setStatus({ kind: "idle" });
      refresh();
    } else {
      setStatus({ kind: "error", message: "删除失败" });
    }
  }, [selected, list, refresh]);

  const handleCreate = useCallback(async () => {
    const name = newName.trim().toLowerCase().replace(/\s+/g, "-");
    if (!name) return;
    if (!/^[a-z0-9_-]+$/.test(name)) {
      setStatus({ kind: "error", message: "名字只能包含字母、数字、下划线、连字符" });
      return;
    }
    if (list.some((p) => p.name === name)) {
      setStatus({ kind: "error", message: `「${name}」已存在` });
      return;
    }
    setStatus({ kind: "saving" });
    const res = await putPersonality(name, `# ${name}\n\n你的人设描述…`);
    if (res) {
      setNewName("");
      setShowNew(false);
      setStatus({ kind: "idle" });
      await refresh();
      load(name);
    } else {
      setStatus({ kind: "error", message: "创建失败" });
    }
  }, [newName, list, refresh, load]);

  const sorted = useMemo(() => {
    const builtin = list.filter((p) => p.is_builtin);
    const custom = list.filter((p) => !p.is_builtin).sort((a, b) => a.name.localeCompare(b.name));
    return [...builtin, ...custom];
  }, [list]);

  return (
    <div className="space-y-3">
      <div>
        <h3 className="text-[14px] font-semibold text-text-primary">角色 / 人设</h3>
        <p className="text-[12px] text-text-muted mt-0.5">
          多个命名的人设 — 内置 3 个(coder / writer / analyst),可添加自定义版本。打开对话时通过 chat 区域的"雇佣专家"chip 选择。
        </p>
      </div>

      <div className="flex gap-4 min-h-[420px]">
        {/* List */}
        <div className="w-56 shrink-0 space-y-1">
          {showNew ? (
            <div className="flex items-center gap-1.5 p-2 rounded-lg" style={{ background: "var(--color-bg-secondary)" }}>
              <input
                autoFocus
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleCreate();
                  if (e.key === "Escape") { setShowNew(false); setNewName(""); }
                }}
                placeholder="人设名,如 finance-planner"
                className="flex-1 rounded-md px-2 py-1 text-[12px] text-text-primary placeholder:text-text-faint outline-none"
                style={{ background: "var(--color-bg-primary)" }}
              />
              <button onClick={handleCreate} className="p-1 rounded text-accent hover:bg-bg-hover" title="创建" aria-label="创建">
                <Check size={12} />
              </button>
              <button onClick={() => { setShowNew(false); setNewName(""); }} className="p-1 rounded text-text-muted hover:bg-bg-hover" title="取消">
                <Trash2 size={12} />
              </button>
            </div>
          ) : (
            <button
              onClick={() => setShowNew(true)}
              className="w-full flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-[12px] text-accent hover:bg-bg-hover"
              style={{ background: "var(--color-bg-secondary)" }}
            >
              <Plus size={12} /> 新建人设
            </button>
          )}

          {sorted.map((p) => (
            <button
              key={p.name}
              onClick={() => load(p.name)}
              className={clsx(
                "w-full flex items-start gap-2 px-2.5 py-2 rounded-lg text-left text-[12.5px]",
                selected === p.name ? "bg-bg-active" : "hover:bg-bg-hover",
              )}
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-1.5">
                  <FileText size={11} className="shrink-0 text-text-muted" />
                  <span className="font-medium text-text-primary truncate">{p.name}</span>
                  {p.is_builtin && (
                    <span className="text-[9.5px] font-mono text-text-faint px-1 py-px rounded bg-bg-tertiary shrink-0">内置</span>
                  )}
                </div>
                {p.summary && (
                  <p className="text-[10.5px] text-text-muted truncate mt-0.5 ml-4">{p.summary}</p>
                )}
              </div>
            </button>
          ))}
        </div>

        {/* Editor */}
        <div className="flex-1 min-w-0">
          {selected ? (
            <div className="space-y-3">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <h4 className="text-[13px] font-semibold text-text-primary truncate">{selected}</h4>
                    {summary?.is_builtin && (
                      <span className="text-[10px] font-mono text-text-faint px-1.5 py-px rounded bg-bg-tertiary">只读</span>
                    )}
                  </div>
                  {summary?.is_builtin && (
                    <p className="text-[10.5px] text-text-faint mt-0.5">
                      内置人设 — 复制为自定义版本来修改
                    </p>
                  )}
                </div>
                <div className="flex items-center gap-1.5 shrink-0">
                  <SaveBadge status={status} />
                  {!summary?.is_builtin && (
                    <button
                      onClick={handleDelete}
                      className="p-1.5 rounded-md text-text-muted hover:text-error hover:bg-bg-hover"
                      title="删除人设"
                     aria-label="删除人设">
                      <Trash2 size={12} />
                    </button>
                  )}
                  <button
                    onClick={handleSave}
                    disabled={status.kind === "saving" || !dirtyRef.current || !!summary?.is_builtin}
                    className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[12.5px] font-medium transition-all disabled:opacity-40 disabled:cursor-not-allowed"
                    style={{
                      background: dirtyRef.current && !summary?.is_builtin ? "var(--color-accent)" : "var(--color-bg-tertiary)",
                      color: dirtyRef.current && !summary?.is_builtin ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
                    }}
                  >
                    {status.kind === "saving" ? <Loader2 size={12} className="animate-spin" /> : <Save size={12} />}
                    保存
                  </button>
                </div>
              </div>
              <textarea
                value={content}
                onChange={(e) => setContent(e.target.value)}
                disabled={!!summary?.is_builtin}
                rows={18}
                className="w-full rounded-lg px-3.5 py-3 text-[12.5px] font-mono leading-relaxed text-text-primary placeholder:text-text-faint outline-none resize-y disabled:opacity-70 disabled:cursor-not-allowed"
                style={{
                  background: "var(--color-bg-primary)",
                  border: "1px solid var(--color-border-secondary)",
                  minHeight: 320,
                }}
                spellCheck={false}
              />
            </div>
          ) : (
            <div className="h-full flex items-center justify-center text-text-muted text-[13px]" style={{ minHeight: 320 }}>
              ← 选择左侧人设查看 / 编辑
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// ── system prompt override (overrides.system_prompt) ─────────────

function SystemPromptEditor({ initial }: { initial: PersonaOverrides }) {
  const [content, setContent] = useState(initial.system_prompt);
  const [status, setStatus] = useState<SaveStatus>({ kind: "idle" });
  const dirtyRef = useRef(false);
  const lastSavedRef = useRef(initial.system_prompt);

  useEffect(() => {
    if (!dirtyRef.current) {
      setContent(initial.system_prompt);
      lastSavedRef.current = initial.system_prompt;
    }
  }, [initial.system_prompt]);

  useEffect(() => {
    if (content !== lastSavedRef.current) dirtyRef.current = true;
  }, [content]);

  const handleSave = useCallback(async () => {
    setStatus({ kind: "saving" });
    const res = await putPersonaOverrides({ ...initial, system_prompt: content });
    if (res) {
      lastSavedRef.current = content;
      dirtyRef.current = false;
      setStatus({ kind: "saved", at: Date.now() });
    } else {
      setStatus({ kind: "error", message: "保存失败" });
    }
  }, [content, initial]);

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-[14px] font-semibold text-text-primary">系统提示词 (覆盖)</h3>
          <p className="text-[12px] text-text-muted mt-0.5 max-w-2xl">
            完整覆盖 agent 的默认 system message。<span className="text-text-faint">留空 = 使用默认(角色 + 灵魂 + 技能 + 时间戳 + 模型操作约束等自动拼接)</span>
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <SaveBadge status={status} />
          <button
            onClick={handleSave}
            disabled={status.kind === "saving" || !dirtyRef.current}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[12.5px] font-medium transition-all disabled:opacity-40 disabled:cursor-not-allowed"
            style={{
              background: dirtyRef.current ? "var(--color-accent)" : "var(--color-bg-tertiary)",
              color: dirtyRef.current ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
            }}
          >
            {status.kind === "saving" ? <Loader2 size={12} className="animate-spin" /> : <Save size={12} />}
            保存
          </button>
        </div>
      </div>
      <textarea
        value={content}
        onChange={(e) => setContent(e.target.value)}
        placeholder={"留空使用默认 system prompt。&#10;&#10;示例:&#10;你是一个简洁专业的助手,直接给出答案,不要寒暄。&#10;回答使用中文,代码块用 ```lang 包裹。"}
        rows={18}
        className="w-full rounded-lg px-3.5 py-3 text-[12.5px] font-mono leading-relaxed text-text-primary placeholder:text-text-faint outline-none resize-y"
        style={{
          background: "var(--color-bg-primary)",
          border: "1px solid var(--color-border-secondary)",
          minHeight: 320,
        }}
        spellCheck={false}
      />
    </div>
  );
}

// ── main view ─────────────────────────────────────────────────────

export function PersonaView() {
  const [tab, setTab] = useState<SubTab>("soul");
  const [soul, setSoul] = useState<PersonaTextFile | null>(null);
  const [user, setUser] = useState<PersonaTextFile | null>(null);
  const [memory, setMemory] = useState<PersonaTextFile | null>(null);
  const [overrides, setOverrides] = useState<PersonaOverrides>({ system_prompt: "", preferences: null });
  const [loaded, setLoaded] = useState(false);

  // Pull all four sources on mount. Cheap (a few file reads).
  useEffect(() => {
    (async () => {
      const [s, u, m, o] = await Promise.all([
        getSoul(), getUserProfile(), getMemory(), getPersonaOverrides(),
      ]);
      setSoul(s);
      setUser(u);
      setMemory(m);
      setOverrides(o);
      setLoaded(true);
    })();
  }, []);

  const refresh = useCallback(async () => {
    const [s, u, m, o] = await Promise.all([
      getSoul(), getUserProfile(), getMemory(), getPersonaOverrides(),
    ]);
    setSoul(s);
    setUser(u);
    setMemory(m);
    setOverrides(o);
  }, []);

  const current = SUB_TABS.find((t) => t.id === tab)!;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center gap-2.5 px-6 py-4">
        <UserCog size={20} className="text-text-secondary" />
        <h2 className="text-[16px] font-semibold text-text-primary">画像</h2>
        <span className="text-[11px] text-text-faint">系统的 agent 提示词 · 灵魂 · 角色 · 用户 · 记忆</span>
        <button
          onClick={refresh}
          className="ml-auto p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-hover"
          title="重新加载"
         aria-label="重新加载">
          <RefreshCw size={13} />
        </button>
      </div>

      {/* Sub-tab nav */}
      <div className="flex items-center gap-1 px-4 border-b" style={{ borderColor: "var(--color-border-primary)" }}>
        {SUB_TABS.map((t) => {
          const Icon = t.icon;
          const active = tab === t.id;
          return (
            <button
              key={t.id}
              onClick={() => setTab(t.id)}
              className={clsx(
                "flex items-center gap-1.5 px-3 py-2.5 text-[12.5px] font-medium border-b-2",
                active ? "border-accent text-accent" : "border-transparent text-text-muted hover:text-text-primary",
              )}
            >
              <Icon size={13} />
              {t.label}
            </button>
          );
        })}
      </div>

      {/* Body */}
      <div className="flex-1 overflow-y-auto p-6">
        {!loaded ? (
          <div className="flex items-center gap-2 text-text-muted text-[13px]">
            <Loader2 size={14} className="animate-spin" /> 加载中…
          </div>
        ) : (
          <>
            <p className="text-[12px] text-text-muted mb-4">{current.hint}</p>
            {tab === "soul" && (
              <FileEditor
                title="灵魂 (SOUL.md)"
                hint="专家的核心信念和价值观,每轮对话开始时注入到 system prompt 顶部。"
                initial={soul}
                onSave={async (c) => { const r = await putSoul(c); if (r) setSoul(r); return r; }}
                placeholder={"# 灵魂\n\n你是一个乐于助人、认真负责的助手。\n- 实事求是,不知道就说不知道\n- 直接给出答案,避免冗长寒暄\n- …"}
                rows={20}
              />
            )}
            {tab === "system" && <SystemPromptEditor initial={overrides} />}
            {tab === "personalities" && <PersonalitiesEditor />}
            {tab === "user" && (
              <FileEditor
                title="用户画像 (USER.md)"
                hint="关于用户的稳定事实 — 姓名、角色、偏好、习惯,专家每次开新对话都会读这个。"
                initial={user}
                onSave={async (c) => { const r = await putUserProfile(c); if (r) setUser(r); return r; }}
                placeholder={"# User profile\n\n- 姓名:张三\n- 职业:物流安全员\n- 偏好:中文、简洁回答、表格优于段落"}
                rows={20}
              />
            )}
            {tab === "memory" && (
              <FileEditor
                title="记忆 (MEMORY.md)"
                hint="专家自己维护的笔记 — 用户授权它保留的事实。LLM 可以通过 memory 工具自动写入,你也可以直接编辑。"
                initial={memory}
                onSave={async (c) => { const r = await putMemory(c); if (r) setMemory(r); return r; }}
                placeholder={"# Memory\n\n跨会话保留的事实:\n- 用户常用目录: <repo-parent>/hermix-agent-rs\n- 默认模型: deepseek-chat\n- …"}
                rows={20}
              />
            )}
          </>
        )}
      </div>
    </div>
  );
}
