/* ════════════════════════════════════════════════════════════════
   Agent Form (manual create / edit)
   ════════════════════════════════════════════════════════════════ */

import { useState } from "react";
import { X } from "lucide-react";
import { clsx } from "clsx";
import {
  COLOR_KEYS, PALETTES, pickColor,
  type AgentColorTheme,
  type AgentPersona,
} from "./types";
import { AgentAvatar } from "./AgentAvatar";
import { Button, Input, TextArea } from "../ui";
import { CANCEL, SAVE } from "../strings";

interface AgentFormProps {
  initial?: Partial<AgentPersona>;
  onSave: (agent: Partial<AgentPersona>) => void;
  onCancel: () => void;
}

/** 色键是历史数据键（violet 实际画出来是陶土红）。可访问名要说人话：
 *  读屏用户看到的是颜色，不是内部键名。 */
const COLOR_LABELS: Record<AgentColorTheme, string> = {
  violet: "陶土红",
  rose: "玫红",
  cyan: "青绿",
  emerald: "翠绿",
  amber: "琥珀",
  teal: "墨青",
  indigo: "橄榄绿",
  fuchsia: "紫红",
};

export function AgentForm({ initial, onSave, onCancel }: AgentFormProps) {
  const [form, setForm] = useState<Partial<AgentPersona>>({
    /* avatar 字段保留（后端 / 历史数据契约），但已不参与渲染 ——
       头像由「姓名首字 + colorTheme」派生，见 AgentAvatar。 */
    name: "", avatar: "", role: "", identity: "", soul: "",
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



  return (
    <form onSubmit={handleSubmit} className="space-y-4">
      {/* Live preview strip */}
      <div className="rounded-lg overflow-hidden bg-bg-primary">
        <div className="p-3 flex items-center gap-2.5 bg-bg-primary">
          <AgentAvatar name={form.name} colorTheme={form.colorTheme} className="shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate">
              {form.name || "未命名专家"}
            </p>
            <p className="text-[length:var(--text-meta)] text-text-muted truncate">
              {form.identity || "—"}
            </p>
          </div>
        </div>
      </div>

      {/* Name + Avatar + Color */}
      <div className="grid grid-cols-[auto_1fr_auto] gap-3">
        {/* 头像不再可选：由姓名首字 + colorTheme 自动生成（见 AgentAvatar）。
            原来的 8 个 emoji 选择器已删除 —— emoji 字形随系统字体变化、
            大小不齐、不跟主题，且与全站 lucide 图标不是一套视觉语言。 */}
        <AgentAvatar name={form.name} colorTheme={form.colorTheme} size="lg" className="mt-0.5" />
        <div className="space-y-2.5">
          <Input
            required
            size="lg"
            label="名称"
            value={form.name || ""}
            onChange={(e) => setForm((p) => ({ ...p, name: e.target.value }))}
            placeholder="如：代码助手"
          />
          <Input
            required
            size="lg"
            label="角色"
            value={form.role || ""}
            onChange={(e) => setForm((p) => ({ ...p, role: e.target.value }))}
            placeholder="如：高级全栈工程师"
          />
        </div>
        <div>
          <div className="block text-[length:var(--text-ui)] font-medium text-text-primary mb-1">颜色</div>
          <div className="grid grid-cols-2 gap-1">
            {COLOR_KEYS.map((k) => (
              <button
                key={k}
                type="button"
                aria-label={`选择${COLOR_LABELS[k]}配色`}
                onClick={() => setForm((p) => ({ ...p, colorTheme: k }))}
                className={clsx("w-6 h-6 rounded-md transition-[transform,opacity]", form.colorTheme === k ? "scale-125" : "opacity-50 hover:opacity-100")}
                title={COLOR_LABELS[k]}
                style={{ background: PALETTES[k].avatar }}
              />
            ))}
          </div>
        </div>
      </div>

      {/* Identity */}
      <Input
        size="lg"
        label="身份标语 (1 句话)"
        value={form.identity || ""}
        onChange={(e) => setForm((p) => ({ ...p, identity: e.target.value }))}
        placeholder="如：帮你把模糊的产品需求，调成可执行的设计语言"
      />

      {/* Soul */}
      <TextArea
        rows={2}
        label="核心理念"
        value={form.soul || ""}
        onChange={(e) => setForm((p) => ({ ...p, soul: e.target.value }))}
        placeholder="专家的核心价值观和工作理念…"
      />

      {/* Skills */}
      <div>
        <div className="block text-[length:var(--text-ui)] font-medium text-text-primary mb-1">技能</div>
        <div className="flex gap-1.5 mb-1.5 items-end">
          <Input
            size="md"
            label="添加技能"
            labelHidden
            containerClassName="flex-1"
            value={skillInput}
            onChange={(e) => setSkillInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addToList("skills", skillInput); } }}
            placeholder="添加技能"
          />
          <Button type="button" variant="ghost" size="sm" onClick={() => addToList("skills", skillInput)}>
            添加
          </Button>
        </div>
        <div className="flex flex-wrap gap-1">
          {(form.skills || []).map((s) => (
            <span key={s} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[length:var(--text-meta)] font-medium bg-bg-tertiary text-text-secondary">
              {s}
              <button type="button" aria-label={`移除技能 ${s}`} onClick={() => removeFromList("skills", s)} className="text-text-faint hover:text-text-error"><X className="icon-xs" /></button>
            </span>
          ))}
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <Input
          size="lg"
          label="专长领域"
          value={form.specialty || ""}
          onChange={(e) => setForm((p) => ({ ...p, specialty: e.target.value }))}
          placeholder="如：Web 开发"
        />
        <Input
          size="lg"
          label="对话风格"
          value={form.dialogStyle || ""}
          onChange={(e) => setForm((p) => ({ ...p, dialogStyle: e.target.value }))}
          placeholder="如：简洁专业"
        />
      </div>

      {/* Personality */}
      <div>
        <div className="block text-[length:var(--text-ui)] font-medium text-text-primary mb-1">性格特点</div>
        <div className="flex gap-1.5 mb-1.5 items-end">
          <Input
            size="md"
            label="添加性格特点"
            labelHidden
            containerClassName="flex-1"
            value={personalityInput}
            onChange={(e) => setPersonalityInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addToList("personality", personalityInput); } }}
            placeholder="如：严谨、注重细节"
          />
          <Button type="button" variant="ghost" size="sm" onClick={() => addToList("personality", personalityInput)}>
            添加
          </Button>
        </div>
        <div className="flex flex-wrap gap-1">
          {(form.personality || []).map((p) => (
            <span key={p} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[length:var(--text-meta)] font-medium bg-bg-tertiary text-text-secondary">
              {p}
              <button type="button" aria-label={`移除性格特点 ${p}`} onClick={() => removeFromList("personality", p)} className="text-text-faint hover:text-text-error"><X className="icon-xs" /></button>
            </span>
          ))}
        </div>
      </div>

      <TextArea
        rows={2}
        label="背景故事"
        value={form.background || ""}
        onChange={(e) => setForm((p) => ({ ...p, background: e.target.value }))}
        placeholder="专家的背景故事…"
      />

      <div className="flex justify-end gap-2 pt-2">
        <Button type="button" variant="ghost" onClick={onCancel}>
          {CANCEL}
        </Button>
        <Button type="submit" variant="primary">
          {SAVE}
        </Button>
      </div>
    </form>
  );
}
