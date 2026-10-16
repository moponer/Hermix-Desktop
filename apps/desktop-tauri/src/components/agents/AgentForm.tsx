/* ════════════════════════════════════════════════════════════════
   Agent Form (manual create / edit)
   ════════════════════════════════════════════════════════════════ */

import { useState } from "react";
import { X } from "lucide-react";
import { clsx } from "clsx";
import {
  AVATARS, COLOR_KEYS, PALETTES, pickColor,
  type AgentPersona,
} from "./types";

interface AgentFormProps {
  initial?: Partial<AgentPersona>;
  onSave: (agent: Partial<AgentPersona>) => void;
  onCancel: () => void;
}

export function AgentForm({ initial, onSave, onCancel }: AgentFormProps) {
  const [form, setForm] = useState<Partial<AgentPersona>>({
    name: "", avatar: "🤖", role: "", identity: "", soul: "",
    skills: [], specialty: "", dialogStyle: "", background: "",
    personality: [], systemPrompt: "",
    colorTheme: pickColor(initial?.name || `agent-${Date.now()}`),
    ...initial,
  });
  const [skillInput, setSkillInput] = useState("");
  const [personalityInput, setPersonalityInput] = useState("");

  const addToList = (key: "skills" | "personality", val: string) => {
    const v = val.trim();
    if (!v || (form[key] || []).includes(v)) return;
    setForm((p) => ({ ...p, [key]: [...(p[key] || []), v] }));
    if (key === "skills") setSkillInput(""); else setPersonalityInput("");
  };
  const removeFromList = (key: "skills" | "personality", v: string) =>
    setForm((p) => ({ ...p, [key]: (p[key] || []).filter((x) => x !== v) }));

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSave(form);
  };

  const g = PALETTES[form.colorTheme || "violet"];

  return (
    <form onSubmit={handleSubmit} className="space-y-4">
      {/* Live preview strip */}
      <div className="rounded-lg overflow-hidden bg-bg-primary">
        <div className="p-3 flex items-center gap-2.5 bg-bg-primary">
          <div
            className="w-9 h-9 rounded-lg flex items-center justify-center text-lg shrink-0"
            style={{ background: g.avatar }}
          >
            {form.avatar}
          </div>
          <div className="min-w-0 flex-1">
            <p className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate">
              {form.name || "未命名专家"}
            </p>
            <p className="text-[length:var(--text-meta)] text-text-muted truncate">
              {form.identity || "添加身份标语让卡片更生动"}
            </p>
          </div>
        </div>
      </div>

      {/* Name + Avatar + Color */}
      <div className="grid grid-cols-[auto_1fr_auto] gap-3">
        <div className="flex flex-col gap-1">
          <div
            className="w-12 h-12 rounded-xl flex items-center justify-center text-2xl"
            style={{ background: g.avatar }}
          >
            {form.avatar}
          </div>
          <div className="grid grid-cols-4 gap-0.5">
            {AVATARS.slice(0, 8).map((a) => (
              <button
                key={a}
                type="button"
                onClick={() => setForm((p) => ({ ...p, avatar: a }))}
                className={clsx("w-6 h-6 rounded text-sm flex items-center justify-center", form.avatar === a ? "bg-bg-active" : "hover:bg-bg-hover")}
              >
                {a}
              </button>
            ))}
          </div>
        </div>
        <div className="space-y-2.5">
          <div>
            <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">名称 *</label>
            <input
              required
              value={form.name || ""}
              onChange={(e) => setForm((p) => ({ ...p, name: e.target.value }))}
              className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
              placeholder="如：代码助手"
            />
          </div>
          <div>
            <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">角色 *</label>
            <input
              required
              value={form.role || ""}
              onChange={(e) => setForm((p) => ({ ...p, role: e.target.value }))}
              className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
              placeholder="如：高级全栈工程师"
            />
          </div>
        </div>
        <div>
          <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">颜色</label>
          <div className="grid grid-cols-2 gap-1">
            {COLOR_KEYS.map((k) => (
              <button
                key={k}
                type="button"
                onClick={() => setForm((p) => ({ ...p, colorTheme: k }))}
                className={clsx("w-6 h-6 rounded-md transition-[transform,opacity]", form.colorTheme === k ? "scale-125" : "opacity-50 hover:opacity-100")}
                title={k}
                style={{ background: PALETTES[k].avatar }}
              />
            ))}
          </div>
        </div>
      </div>

      {/* Identity */}
      <div>
        <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">身份标语 (1 句话)</label>
        <input
          value={form.identity || ""}
          onChange={(e) => setForm((p) => ({ ...p, identity: e.target.value }))}
          className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
          placeholder="如：帮你把模糊的产品需求，调成可执行的设计语言"
        />
      </div>

      {/* Soul */}
      <div>
        <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">核心理念</label>
        <textarea
          rows={2}
          value={form.soul || ""}
          onChange={(e) => setForm((p) => ({ ...p, soul: e.target.value }))}
          className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none resize-none"
          placeholder="专家的核心价值观和工作理念..."
        />
      </div>

      {/* Skills */}
      <div>
        <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">技能</label>
        <div className="flex gap-1.5 mb-1.5">
          <input
            value={skillInput}
            onChange={(e) => setSkillInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addToList("skills", skillInput); }}}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="添加技能"
          />
          <button type="button" onClick={() => addToList("skills", skillInput)} className="px-2.5 py-1.5 rounded-lg text-[length:var(--text-meta)] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>
            添加
          </button>
        </div>
        <div className="flex flex-wrap gap-1">
          {(form.skills || []).map((s) => (
            <span key={s} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[length:var(--text-meta)] font-medium bg-bg-tertiary text-text-secondary">
              {s}
              <button type="button" onClick={() => removeFromList("skills", s)} className="text-text-faint hover:text-error"><X className="icon-xs" /></button>
            </span>
          ))}
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">专长领域</label>
          <input
            value={form.specialty || ""}
            onChange={(e) => setForm((p) => ({ ...p, specialty: e.target.value }))}
            className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="如:Web 开发"
          />
        </div>
        <div>
          <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">对话风格</label>
          <input
            value={form.dialogStyle || ""}
            onChange={(e) => setForm((p) => ({ ...p, dialogStyle: e.target.value }))}
            className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="如：简洁专业"
          />
        </div>
      </div>

      {/* Personality */}
      <div>
        <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">性格特点</label>
        <div className="flex gap-1.5 mb-1.5">
          <input
            value={personalityInput}
            onChange={(e) => setPersonalityInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addToList("personality", personalityInput); }}}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="如：严谨、注重细节"
          />
          <button type="button" onClick={() => addToList("personality", personalityInput)} className="px-2.5 py-1.5 rounded-lg text-[length:var(--text-meta)] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>
            添加
          </button>
        </div>
        <div className="flex flex-wrap gap-1">
          {(form.personality || []).map((p) => (
            <span key={p} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[length:var(--text-meta)] font-medium bg-bg-tertiary text-text-secondary">
              {p}
              <button type="button" onClick={() => removeFromList("personality", p)} className="text-text-faint hover:text-error"><X className="icon-xs" /></button>
            </span>
          ))}
        </div>
      </div>

      <div>
        <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1 font-semibold">背景故事</label>
        <textarea
          rows={2}
          value={form.background || ""}
          onChange={(e) => setForm((p) => ({ ...p, background: e.target.value }))}
          className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint outline-none resize-none"
          placeholder="专家的背景故事..."
        />
      </div>

      <div className="flex justify-end gap-2 pt-2">
        <button type="button" onClick={onCancel} className="rounded-lg px-4 py-2 text-[length:var(--text-ui)] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>
          取消
        </button>
        <button type="submit" className="rounded-lg px-4 py-2 text-[length:var(--text-ui)] font-medium" style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}>
          保存
        </button>
      </div>
    </form>
  );
}