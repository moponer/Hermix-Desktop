import { useState, useEffect, useMemo, useRef } from "react";
import {
  Bot, Plus, Sparkles, Trash2, Edit3, Check, X, RefreshCw, Brain,
  MessageSquare, ArrowLeft, ArrowRight, Copy, Play,
  Wand2, Sparkle, Layers,
} from "lucide-react";
import { clsx } from "clsx";
import { BUILTIN_EXPERTS, EXPERT_CATEGORIES, type ExpertDef } from "../data/builtinExperts";
import { useSessionExpert } from "../hooks/useExpert";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

const STORAGE_KEY = STORAGE_KEYS.agents;

/* ════════════════════════════════════════════════════════════════
   Types
   ════════════════════════════════════════════════════════════════ */

export interface AgentPersona {
  id: string;
  name: string;
  avatar: string;           // emoji
  role: string;
  identity: string;         // one-line tagline shown on card
  soul: string;             // longer self-description
  skills: string[];
  specialty: string;
  dialogStyle: string;
  background: string;
  personality: string[];
  systemPrompt: string;
  model?: string;
  /** solid color key — drives card banner + avatar + chip palette */
  colorTheme: AgentColorTheme;
  /** for usage stats on card */
  usageCount: number;
  lastUsedAt: number;
  createdAt: number;
  updatedAt: number;
  /** optional sample Q/A the AI imagined to demonstrate behaviour */
  sampleDialog?: { q: string; a: string };
}

export type AgentColorTheme =
  | "violet" | "rose" | "cyan" | "emerald"
  | "amber" | "teal"  | "indigo" | "fuchsia";

/* ════════════════════════════════════════════════════════════════
   Color palette — single source of truth (solid colors only, no gradients)
   ════════════════════════════════════════════════════════════════ */

interface ColorPalette {
  /** top stripe color on the card */
  banner: string;
  /** avatar background color */
  avatar: string;
  /** soft chip background + fg for skill tags */
  chip: { bg: string; fg: string };
}

const PALETTES: Record<AgentColorTheme, ColorPalette> = {
  violet:  { banner: "#6d5ad6", avatar: "#ece9fb", chip: { bg: "#ece9fb", fg: "#5b4ec1" } },
  rose:    { banner: "#e0518a", avatar: "#fce4ee", chip: { bg: "#fce4ee", fg: "#b1356a" } },
  cyan:    { banner: "#2b8fd6", avatar: "#e1f1fb", chip: { bg: "#e1f1fb", fg: "#1a6aa3" } },
  emerald: { banner: "#2e9d6b", avatar: "#defaed", chip: { bg: "#defaed", fg: "#1f6f4a" } },
  amber:   { banner: "#c2841a", avatar: "#fbeed1", chip: { bg: "#fbeed1", fg: "#8a5d10" } },
  teal:    { banner: "#2c7873", avatar: "#d9eeec", chip: { bg: "#d9eeec", fg: "#1d5754" } },
  indigo:  { banner: "#5b58c9", avatar: "#e7e5fb", chip: { bg: "#e7e5fb", fg: "#403cb0" } },
  fuchsia: { banner: "#b94a8f", avatar: "#fbe0ee", chip: { bg: "#fbe0ee", fg: "#8a326b" } },
};

const COLOR_KEYS: AgentColorTheme[] = Object.keys(PALETTES) as AgentColorTheme[];

/** Stable color picked from agent name so visually similar agents don't get same color. */
function pickColor(seed: string): AgentColorTheme {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  return COLOR_KEYS[h % COLOR_KEYS.length];
}

/* ════════════════════════════════════════════════════════════════
   Agent Templates — 8 preset archetypes
   ════════════════════════════════════════════════════════════════ */

export interface AgentTemplate {
  id: string;
  name: string;
  emoji: string;
  tagline: string;     // 1-line hook shown in card
  role: string;
  category: string;    // human-readable
  colorTheme: AgentColorTheme;
  /** default choices the user can edit on step 2 (clarify) */
  defaults?: Partial<AgentPersona>;
  systemPrompt: string;
  sampleDialog: { q: string; a: string };
  skills: string[];
}

const TEMPLATES: AgentTemplate[] = [
  {
    id: "tpl-engineer",
    name: "编程专家",
    emoji: "⚡",
    tagline: "帮你写出更少 bug 的代码",
    role: "高级软件工程师",
    category: "编程开发",
    colorTheme: "violet",
    skills: ["代码审查", "性能优化", "重构", "单元测试"],
    sampleDialog: {
      q: "我这段代码在 1 万并发下延迟很高，能帮我看看吗？",
      a: "先把消息队列和锁的分布画一下。大概率是锁竞争，profiling 后再下结论。",
    },
    systemPrompt: "你是一名资深软件工程师，擅长系统性分析问题。回答时先给出结论，再分点列出关键证据，最后给出可执行的改进方案。引用 RFC 或官方文档时给出链接。",
  },
  {
    id: "tpl-writer",
    name: "爆款写手",
    emoji: "✍️",
    tagline: "10w+ 标题套路它都懂",
    role: "公众号 / 营销内容写手",
    category: "内容创作",
    colorTheme: "rose",
    skills: ["爆款标题", "故事化叙述", "金句提炼", "金句卡点"],
    sampleDialog: {
      q: "帮我想 5 个关于「远程办公」的公众号标题",
      a: "1) 我们公司远程办公 3 年,发现这 5 件事比在公司更累\n2) 远程办公不是在家躺着,是你重新学习怎么工作\n…",
    },
    systemPrompt: "你是一名擅长中文新媒体写作的写手,熟悉公众号、小红书、知乎等平台的爆款套路。每次输出至少给 3 个不同角度的版本,并解释为什么这个角度能火。",
  },
  {
    id: "tpl-translator",
    name: "同声传译",
    emoji: "🌐",
    tagline: "保留俚语和口吻的多语翻译",
    role: "多语种同声传译",
    category: "翻译",
    colorTheme: "emerald",
    skills: ["中英日韩", "商务场景", "保留俚语", "专业术语"],
    sampleDialog: {
      q: "把这段翻译成日语,要保留口语感:「这个产品真香,买到就是赚到」",
      a: "「このガジェット本当に『やばい』。買わなきゃ損するレベル」\n(注:用『やばい』对应『真香』的口语张力,保留『赚到』的双关意味)",
    },
    systemPrompt: "你是一名专业同声传译,擅长中英日韩互译。翻译时优先保留原文的语气、俚语、玩笑话,而不是字面对应。专业术语给两种译法(直译/意译)并解释。",
  },
  {
    id: "tpl-analyst",
    name: "数据分析师",
    emoji: "📊",
    tagline: "SQL 写手 + 业务洞察",
    role: "数据分析师",
    category: "数据分析",
    colorTheme: "cyan",
    skills: ["SQL", "Python", "可视化", "业务洞察"],
    sampleDialog: {
      q: "DAU 周环比下降了 8%,帮我写一段排查思路",
      a: "1) 拆渠道:哪个渠道 DAU 掉得最多?\n2) 拆人群:新用户/老用户/沉默用户\n3) 看漏斗:激活→注册→首单哪一步崩了\n4) 对比版本:近一周有发版吗?\n我先写个 SQL 给你看 1) 和 2)…",
    },
    systemPrompt: "你是一名数据分析师,擅长用数据回答业务问题。回答时先说「如何拆解这个问题」,再给 SQL/Python 代码,最后给业务解读。永远先讲方法论,再讲工具。",
  },
  {
    id: "tpl-product",
    name: "产品经理",
    emoji: "💡",
    tagline: "需求→方案→排期一把抓",
    role: "资深产品经理",
    category: "产品",
    colorTheme: "amber",
    skills: ["需求分析", "用户访谈", "PRD 撰写", "优先级排序"],
    sampleDialog: {
      q: "老板让我做一个 AI 笔记功能,我该怎么开始?",
      a: "第一步不是画原型,是问清楚:\n1) 解决谁的问题?(目标用户)\n2) 在什么场景下用?(使用时机)\n3) 现有方案是什么?为什么不够好?\n先做 5 个用户访谈再写 PRD,不要被老板的 deadline 带着走。",
    },
    systemPrompt: "你是一名资深产品经理,擅长把模糊需求拆成可执行方案。每次回答先讲「为什么」再讲「做什么」。质疑任何不清晰的假设,主动追问用户场景,不要急着给方案。",
  },
  {
    id: "tpl-tutor",
    name: "私教老师",
    emoji: "🎓",
    tagline: "耐心讲解,因材施教",
    role: "学科私教",
    category: "学习答疑",
    colorTheme: "indigo",
    skills: ["循序渐进", "举例说明", "出题巩固", "举一反三"],
    sampleDialog: {
      q: "我数学总考 60 多分,不知道从哪里补",
      a: "先别急着刷题。我们做 3 件事:\n1) 拿出最近 3 次试卷,标出每道错题「不会做」还是「粗心」\n2) 统计:是基础题扣分多,还是大题完全不会?\n3) 基础题错的 > 70% → 回到课本先把定义过一遍,不是去刷难题\n你先把试卷拍照发我。",
    },
    systemPrompt: "你是一名耐心的私教老师。先诊断学生的问题所在,再针对性讲解。每次讲完一个概念必须举 1-2 个例子,然后用 1 道小问题确认学生真的懂了。永远不要一次性灌输太多。",
  },
  {
    id: "tpl-counselor",
    name: "心理咨询",
    emoji: "💬",
    tagline: "倾听、共情、不评判",
    role: "心理咨询师",
    category: "咨询",
    colorTheme: "teal",
    skills: ["积极倾听", "共情回应", "情绪命名", "开放式提问"],
    sampleDialog: {
      q: "我最近工作压力很大,每天都睡不着",
      a: "听起来你已经在承受不小的负担。能多说说是什么让你感到压力吗?是工作内容本身,还是和同事/老板的关系,或者别的?",
    },
    systemPrompt: "你是一名心理咨询师,采用「人本主义」+「认知行为」的综合方法。永远先共情,再提问,最后才给建议。不要说「我理解你」这种空洞的话,要用对方原话里的具体细节回应。",
  },
  {
    id: "tpl-bizdev",
    name: "商业顾问",
    emoji: "📈",
    tagline: "商业模式、增长、融资",
    role: "战略 / 商业顾问",
    category: "商业",
    colorTheme: "fuchsia",
    skills: ["商业模式画布", "增长策略", "融资 BP", "竞品分析"],
    sampleDialog: {
      q: "我们 SaaS 做到 ARR 100 万,下一步怎么增长?",
      a: "ARR 100 万是关键拐点,通常有 3 条路:\n1) PLG 拉新(免费版 → 付费转化)\n2) 销售拓展 SDR + AE 团队\n3) 渠道分销 / 集成生态\n你们当前的 CAC 和 LTV 比例是多少?决定了哪条路最划算。",
    },
    systemPrompt: "你是一名顶级商业顾问,服务过多家 SaaS / 消费品牌的创始人。回答用「框架 + 数据 + 反常识」三段式:先给分析框架,再举行业基准数据,最后点出对方可能忽略的反常识点。",
  },
];

/* ════════════════════════════════════════════════════════════════
   Storage helpers
   ════════════════════════════════════════════════════════════════ */

function loadAgents(): AgentPersona[] {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as AgentPersona[];
    // Migrate old records to new shape
    return parsed.map((a) => ({
      ...a,
      colorTheme: a.colorTheme || pickColor(a.name || a.id),
      usageCount: a.usageCount ?? 0,
      lastUsedAt: a.lastUsedAt ?? a.updatedAt ?? a.createdAt ?? Date.now(),
    }));
  } catch {
    return [];
  }
}

function saveAgents(agents: AgentPersona[]) {
  try { storage.setItem(STORAGE_KEY, JSON.stringify(agents)); } catch {}
}

function buildSystemPrompt(agent: Partial<AgentPersona>): string {
  const parts: string[] = [];
  if (agent.name) parts.push(`你叫「${agent.name}」`);
  if (agent.role) parts.push(`角色:${agent.role}`);
  if (agent.identity) parts.push(`身份定位:${agent.identity}`);
  if (agent.soul) parts.push(`核心理念:${agent.soul}`);
  if (agent.skills?.length) parts.push(`擅长:${agent.skills.join("、")}`);
  if (agent.specialty) parts.push(`专长领域:${agent.specialty}`);
  if (agent.dialogStyle) parts.push(`对话风格:${agent.dialogStyle}`);
  if (agent.background) parts.push(`背景:${agent.background}`);
  if (agent.personality?.length) parts.push(`性格特点:${agent.personality.join("、")}`);
  return parts.join("\n");
}

const AVATARS = ["🤖", "🧠", "🎯", "🔮", "⚡", "🛡️", "🔬", "📚", "🎨", "🦊", "🐉", "🌟", "💡", "🚀", "🌊", "🔥", "🧙", "🦉"];

/* ════════════════════════════════════════════════════════════════
   Agent Card v3 — 参考图风格:大圆形头像 + 名字/角色 + 描述 + chips + 行动按钮
   ════════════════════════════════════════════════════════════════ */

interface AgentCardProps {
  agent: AgentPersona;
  onEdit: () => void;
  onDelete: () => void;
  onTest: () => void;
  onUse: () => void;
}

function AgentCard({ agent, onEdit, onDelete, onTest, onUse }: AgentCardProps) {
  const visibleSkills = agent.skills.slice(0, 3);
  // 右上小标签:按使用次数分级
  const label =
    agent.usageCount === 0 ? "新" :
    agent.usageCount < 4   ? "新晋" :
    agent.usageCount < 10  ? "常用" : "推荐";

  return (
    <div className="group relative rounded-xl bg-bg-card p-3.5 transition-all hover:-translate-y-0.5 hover:shadow-md">
      <div className="flex items-start gap-2.5">
        {/* Round avatar */}
        <div className="w-9 h-9 rounded-full flex items-center justify-center text-[20px] shrink-0 bg-bg-tertiary">
          {agent.avatar}
        </div>

        {/* Content */}
        <div className="min-w-0 flex-1">
          <div className="flex items-start justify-between gap-2 mb-0.5">
            <div className="min-w-0 flex-1">
              <h3 className="text-[12.5px] font-semibold text-text-primary truncate leading-tight">
                {agent.name}
              </h3>
              <p className="text-[10.5px] text-text-muted truncate mt-0.5">
                {agent.role || "AI 助手"}
              </p>
            </div>
            <span
              className="text-[9.5px] font-medium px-1.5 py-0.5 rounded shrink-0 mt-0.5 bg-accent-soft text-accent"
            >
              {label}
            </span>
          </div>

          {agent.identity && (
            <p className="text-[11px] text-text-secondary leading-relaxed line-clamp-2 mt-1 min-h-[2.4em]">
              {agent.identity}
            </p>
          )}

          {(visibleSkills.length > 0 || agent.specialty) && (
            <div className="flex flex-wrap gap-1 mt-2">
              {visibleSkills.map((s) => (
                <span
                  key={s}
                  className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary"
                >
                  {s}
                </span>
              ))}
              {visibleSkills.length === 0 && agent.specialty && (
                <span className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary">
                  {agent.specialty}
                </span>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Actions — no border-top, just gap */}
      <div className="flex items-center gap-1.5 mt-2.5">
        <button
          onClick={onUse}
          className="flex-1 flex items-center justify-center gap-1.5 py-1.5 rounded-lg text-[11.5px] font-medium transition-all hover:opacity-90"
          style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
        >
          <MessageSquare size={11} />
          在对话中使用
        </button>
        <button
          onClick={onTest}
          title="快速测试"
          className="p-1.5 rounded-lg hover:bg-bg-hover"
          style={{ color: "var(--color-btn-ghost-text)" }}
        >
          <Play size={11} />
        </button>
        <button
          onClick={onEdit}
          title="编辑"
          className="p-1.5 rounded-lg hover:bg-bg-hover"
          style={{ color: "var(--color-btn-ghost-text)" }}
        >
          <Edit3 size={11} />
        </button>
        <button
          onClick={onDelete}
          title="删除"
          className="p-1.5 rounded-lg hover:bg-error-soft"
          style={{ color: "var(--color-btn-ghost-text)" }}
        >
          <Trash2 size={11} />
        </button>
      </div>
    </div>
  );
}

/* ════════════════════════════════════════════════════════════════
   Agent Form (manual create / edit)
   ════════════════════════════════════════════════════════════════ */

interface AgentFormProps {
  initial?: Partial<AgentPersona>;
  onSave: (agent: Partial<AgentPersona>) => void;
  onCancel: () => void;
}

function AgentForm({ initial, onSave, onCancel }: AgentFormProps) {
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
            <p className="text-[12.5px] font-semibold text-text-primary truncate">
              {form.name || "未命名专家"}
            </p>
            <p className="text-[11px] text-text-muted truncate">
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
            <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">名称 *</label>
            <input
              required
              value={form.name || ""}
              onChange={(e) => setForm((p) => ({ ...p, name: e.target.value }))}
              className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
              placeholder="如:代码助手"
            />
          </div>
          <div>
            <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">角色 *</label>
            <input
              required
              value={form.role || ""}
              onChange={(e) => setForm((p) => ({ ...p, role: e.target.value }))}
              className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
              placeholder="如:高级全栈工程师"
            />
          </div>
        </div>
        <div>
          <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">颜色</label>
          <div className="grid grid-cols-2 gap-1">
            {COLOR_KEYS.map((k) => (
              <button
                key={k}
                type="button"
                onClick={() => setForm((p) => ({ ...p, colorTheme: k }))}
                className={clsx("w-6 h-6 rounded-md transition-all", form.colorTheme === k ? "scale-125" : "opacity-50 hover:opacity-100")}
                title={k}
                style={{ background: PALETTES[k].avatar }}
              />
            ))}
          </div>
        </div>
      </div>

      {/* Identity */}
      <div>
        <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">身份标语 (1 句话)</label>
        <input
          value={form.identity || ""}
          onChange={(e) => setForm((p) => ({ ...p, identity: e.target.value }))}
          className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
          placeholder="如:帮你把模糊的产品需求,调成可执行的设计语言"
        />
      </div>

      {/* Soul */}
      <div>
        <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">核心理念</label>
        <textarea
          rows={2}
          value={form.soul || ""}
          onChange={(e) => setForm((p) => ({ ...p, soul: e.target.value }))}
          className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none resize-none"
          placeholder="专家的核心价值观和工作理念..."
        />
      </div>

      {/* Skills */}
      <div>
        <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">技能</label>
        <div className="flex gap-1.5 mb-1.5">
          <input
            value={skillInput}
            onChange={(e) => setSkillInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addToList("skills", skillInput); }}}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="添加技能"
          />
          <button type="button" onClick={() => addToList("skills", skillInput)} className="px-2.5 py-1.5 rounded-lg text-[12px] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>
            添加
          </button>
        </div>
        <div className="flex flex-wrap gap-1">
          {(form.skills || []).map((s) => (
            <span key={s} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[11px] font-medium bg-bg-tertiary text-text-secondary">
              {s}
              <button type="button" onClick={() => removeFromList("skills", s)} className="text-text-faint hover:text-error"><X size={10} /></button>
            </span>
          ))}
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">专长领域</label>
          <input
            value={form.specialty || ""}
            onChange={(e) => setForm((p) => ({ ...p, specialty: e.target.value }))}
            className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="如:Web 开发"
          />
        </div>
        <div>
          <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">对话风格</label>
          <input
            value={form.dialogStyle || ""}
            onChange={(e) => setForm((p) => ({ ...p, dialogStyle: e.target.value }))}
            className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="如:简洁专业"
          />
        </div>
      </div>

      {/* Personality */}
      <div>
        <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">性格特点</label>
        <div className="flex gap-1.5 mb-1.5">
          <input
            value={personalityInput}
            onChange={(e) => setPersonalityInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addToList("personality", personalityInput); }}}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
            placeholder="如:严谨、注重细节"
          />
          <button type="button" onClick={() => addToList("personality", personalityInput)} className="px-2.5 py-1.5 rounded-lg text-[12px] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>
            添加
          </button>
        </div>
        <div className="flex flex-wrap gap-1">
          {(form.personality || []).map((p) => (
            <span key={p} className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[11px] font-medium bg-bg-tertiary text-text-secondary">
              {p}
              <button type="button" onClick={() => removeFromList("personality", p)} className="text-text-faint hover:text-error"><X size={10} /></button>
            </span>
          ))}
        </div>
      </div>

      <div>
        <label className="block text-[10.5px] text-text-muted uppercase tracking-wider mb-1 font-semibold">背景故事</label>
        <textarea
          rows={2}
          value={form.background || ""}
          onChange={(e) => setForm((p) => ({ ...p, background: e.target.value }))}
          className="w-full rounded-lg bg-bg-tertiary px-3 py-2 text-[13px] text-text-primary placeholder:text-text-faint outline-none resize-none"
          placeholder="专家的背景故事..."
        />
      </div>

      <div className="flex justify-end gap-2 pt-2">
        <button type="button" onClick={onCancel} className="rounded-lg px-4 py-2 text-[13px] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>
          取消
        </button>
        <button type="submit" className="rounded-lg px-4 py-2 text-[13px] font-medium" style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}>
          保存
        </button>
      </div>
    </form>
  );
}

/* ════════════════════════════════════════════════════════════════
   AI Generation — 4-step wizard
   ════════════════════════════════════════════════════════════════ */

type GenStep = "template" | "input" | "clarify" | "preview";
type GenClarify = {
  domain: string | null;        // 领域
  style: string | null;         // 风格
  tools: string[];               // 工具能力
};

interface GeneratedAgent extends Partial<AgentPersona> {
  reasoning?: string;            // AI's design thinking
  systemPrompt: string;
  sampleDialog?: { q: string; a: string };
  colorTheme: AgentColorTheme;
}

const DOMAIN_OPTIONS = [
  { id: "creative",   label: "创意写作",   emoji: "✍️" },
  { id: "code",       label: "编程开发",   emoji: "💻" },
  { id: "analysis",   label: "数据分析",   emoji: "📊" },
  { id: "design",     label: "设计辅助",   emoji: "🎨" },
  { id: "learning",   label: "学习答疑",   emoji: "🎓" },
  { id: "biz",        label: "商业战略",   emoji: "📈" },
  { id: "translate",  label: "翻译",       emoji: "🌐" },
  { id: "chat",       label: "日常对话",   emoji: "💬" },
];

const STYLE_OPTIONS = [
  { id: "professional", label: "专业严谨" },
  { id: "humorous",     label: "幽默风趣" },
  { id: "concise",      label: "简洁直接" },
  { id: "patient",      label: "耐心温和" },
  { id: "sarcastic",    label: "犀利吐槽" },
];

const TOOL_OPTIONS = [
  { id: "search",     label: "网络搜索" },
  { id: "files",      label: "文件操作" },
  { id: "shell",      label: "执行命令" },
  { id: "browser",    label: "浏览器" },
  { id: "write",      label: "写文档" },
  { id: "image",      label: "生成图片" },
];

/** Step 1: when user is stuck, show these as clickable starters that pre-fill description. */
const EXAMPLE_DESCRIPTIONS = [
  "想做一个能帮我审合同的法律顾问,用语严谨,会主动标出风险点",
  "想做一个能陪我练英语口语的朋友,生活话题为主,纠错但不打击人",
  "想做一个能帮我减肥的运动教练,每周出计划,根据反馈调整强度",
  "想做一个能帮我写小红书爆款的写手,熟悉 5 类标题套路,带 emoji",
  "想做一个能帮我读书的助手,30 分钟讲透一本非虚构书的逻辑",
];

interface AIGenerateWizardProps {
  onComplete: (agent: Partial<AgentPersona>) => void;
  onCancel: () => void;
  /** optional initial description if user came from elsewhere */
  initialDescription?: string;
}

function AIGenerateWizard({ onComplete, onCancel, initialDescription = "" }: AIGenerateWizardProps) {
  const [step, setStep] = useState<GenStep>("template");
  const [selectedTpl, setSelectedTpl] = useState<AgentTemplate | null>(null);
  const [description, setDescription] = useState(initialDescription);
  const [clarify, setClarify] = useState<GenClarify>({ domain: null, style: null, tools: [] });
  const [generated, setGenerated] = useState<GeneratedAgent | null>(null);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const topRef = useRef<HTMLDivElement>(null);

  // Scroll to top on step change
  useEffect(() => {
    topRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, [step]);

  // ── Step transitions ──
  const pickTemplate = (tpl: AgentTemplate) => {
    setSelectedTpl(tpl);
    if (!description.trim()) {
      setDescription(`${tpl.tagline},能帮我解决 ${tpl.category} 相关问题`);
    }
    setStep("input");
  };
  const skipToCustom = () => {
    setSelectedTpl(null);
    setStep("input");
  };
  const submitInput = () => {
    if (!description.trim()) return;
    setStep("clarify");
  };
  const submitClarify = () => {
    if (!clarify.domain) {
      setError("请至少选择 1 个领域");
      return;
    }
    setError(null);
    setStep("preview");
    runGeneration();
  };

  // ── AI generation with strengthened prompt ──
  const runGeneration = async () => {
    setGenerating(true);
    setError(null);
    try {
      const apiUrl = storage.getItem(STORAGE_KEYS.agentsServerUrl) || "http://127.0.0.1:3000";
      const apiKey = storage.getItem(STORAGE_KEYS.agentsApiKey) || "";
      const domainText = DOMAIN_OPTIONS.find((o) => o.id === clarify.domain)?.label || clarify.domain;
      const styleText  = STYLE_OPTIONS.find((o) => o.id === clarify.style)?.label  || clarify.style || "未指定";
      const toolsText  = clarify.tools.map((id) => TOOL_OPTIONS.find((o) => o.id === id)?.label).filter(Boolean).join("、") || "无";

      const sysPrompt = `你是一个专业的专家设计师 (Expert Designer)。根据用户的需求设计一个高度专业、表现稳定的 AI 专家。

**设计要求**:
1. 名字简洁,2-4 个汉字或 1 个英文单词,不要包含「AI」「助手」「机器人」等通用词
2. 角色 (role) 必须是具体的职业身份,例如「资深 React 架构师」「小红书爆款写手」
3. 身份标语 (identity) 一句话讲清「这个专家能帮用户做什么」,30 字以内
4. 核心理念 (soul) 1-2 句话,体现价值观和工作方式
5. 技能 (skills) 3-5 个具体能力,避免「对话/分析/生成」这种万能词
6. 性格 (personality) 2-3 个形容词
7. systemPrompt 写一段 80-200 字的完整 system prompt,告诉 LLM 如何扮演这个角色,包含:角色定位、回答风格、回答模板、避免什么
8. sampleDialog 必须给一个真实场景的 Q/A 演示,展示这个专家的说话方式
9. reasoning 简要说明你为什么这样设计 (1-2 句话),让用户知道你是否理解正确

**用户上下文**:
- 需求描述: ${description}
- 领域: ${domainText}
- 风格: ${styleText}
- 工具能力: ${toolsText}
${selectedTpl ? `- 用户从模板「${selectedTpl.name}」开始,可参考其人设` : ""}

**输出**: 严格的 JSON,不要 markdown 包裹。格式:
{"name":"","avatar":"一个emoji","role":"","identity":"","soul":"","skills":[],"specialty":"","dialogStyle":"","personality":[],"systemPrompt":"","sampleDialog":{"q":"","a":""},"reasoning":""}`;

      const res = await fetch(`${apiUrl}/v1/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json", ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}) },
        body: JSON.stringify({
          messages: [
            { role: "system", content: sysPrompt },
            { role: "user", content: description },
          ],
          stream: false,
          temperature: 0.8,
        }),
      });
      if (!res.ok) throw new Error(`API ${res.status}`);
      const data = await res.json();
      const content: string = data.choices?.[0]?.message?.content || "";
      const jsonMatch = content.match(/\{[\s\S]*\}/);
      if (!jsonMatch) throw new Error("AI 返回格式错误");
      const parsed = JSON.parse(jsonMatch[0]);
      const merged: GeneratedAgent = {
        ...parsed,
        colorTheme: selectedTpl?.colorTheme || pickColor(parsed.name || description),
        systemPrompt: parsed.systemPrompt || buildSystemPrompt(parsed),
      };
      setGenerated(merged);
    } catch (e) {
      // Fallback to template-based generation
      const fb = generateFromTemplate(description, selectedTpl, clarify);
      setGenerated(fb);
      setError("AI 服务不可用,已使用模板生成");
    } finally {
      setGenerating(false);
    }
  };

  const handleConfirm = () => {
    if (!generated) return;
    onComplete(generated);
  };

  // ── Renders ──
  const STEPS: GenStep[] = ["template", "input", "clarify", "preview"];
  const STEP_INDEX = STEPS.indexOf(step);
  const g = PALETTES[generated?.colorTheme || selectedTpl?.colorTheme || "violet"];

  return (
    <div className="p-6" ref={topRef}>
      <div className="max-w-2xl mx-auto">
        {/* Header */}
        <div className="flex items-center justify-between mb-5">
          <div className="flex items-center gap-2.5">
            <button
              onClick={step === "template" ? onCancel : () => setStep(STEPS[STEP_INDEX - 1])}
              className="p-1.5 rounded-md hover:bg-bg-hover text-text-muted"
              title="返回"
            >
              <ArrowLeft size={16} />
            </button>
            <h3 className="text-[15px] font-semibold text-text-primary flex items-center gap-2">
              <Sparkle size={16} className="text-text-secondary" />
              AI 生成专家
            </h3>
          </div>
          <div className="flex items-center gap-1.5">
            {STEPS.map((s, i) => (
              <div key={s} className="flex items-center">
                <div className={clsx(
                  "w-6 h-6 rounded-full flex items-center justify-center text-[11px] font-medium transition-all",
                  i < STEP_INDEX ? "bg-green-500 text-white"
                  : i === STEP_INDEX ? "bg-accent text-accent-on-text"
                  : "bg-bg-tertiary text-text-faint"
                )}>
                  {i < STEP_INDEX ? <Check size={11} /> : i + 1}
                </div>
                {i < STEPS.length - 1 && (
                  <div className={clsx("w-6 h-px", i < STEP_INDEX ? "bg-green-500" : "bg-bg-tertiary")} />
                )}
              </div>
            ))}
          </div>
        </div>

        {/* ── Step 1: Template picker + 不知道写什么时的示例语句 ── */}
        {step === "template" && (
          <div className="rounded-xl bg-bg-card p-5 space-y-4">
            <div>
              <p className="text-[14px] font-medium text-text-primary mb-1">
                想要一个什么类型的专家?
              </p>
              <p className="text-[11.5px] text-text-muted">
                点示例快速开始,或选模板改,或自己写
              </p>
            </div>

            {/* 示例语句 (灵感起点) */}
            <div>
              <p className="text-[11px] text-text-muted mb-1.5">不知道怎么描述?从这些开始:</p>
              <div className="flex flex-wrap gap-1.5">
                {EXAMPLE_DESCRIPTIONS.map((d) => (
                  <button
                    key={d}
                    onClick={() => { setSelectedTpl(null); setDescription(d); setStep("input"); }}
                    className="text-left px-2.5 py-1.5 rounded-md text-[11.5px] text-text-secondary bg-bg-tertiary hover:bg-bg-hover transition-colors"
                  >
                    {d}
                  </button>
                ))}
              </div>
            </div>

            {/* 模板 */}
            <div>
              <p className="text-[11px] text-text-muted mb-1.5">或者从模板开始改:</p>
              <div className="grid grid-cols-2 gap-2">
                {TEMPLATES.map((tpl) => (
                  <button
                    key={tpl.id}
                    onClick={() => pickTemplate(tpl)}
                    className="text-left rounded-lg bg-bg-primary p-2.5 hover:bg-bg-hover transition-all"
                  >
                    <div className="flex items-center gap-2 mb-1">
                      <div
                        className="w-8 h-8 rounded-full flex items-center justify-center text-base shrink-0 bg-bg-tertiary"
                      >
                        {tpl.emoji}
                      </div>
                      <div className="min-w-0 flex-1">
                        <p className="text-[12.5px] font-semibold text-text-primary truncate">{tpl.name}</p>
                        <p className="text-[10.5px] text-text-muted">{tpl.category}</p>
                      </div>
                    </div>
                    <p className="text-[11px] text-text-secondary line-clamp-2 leading-relaxed">{tpl.tagline}</p>
                  </button>
                ))}
              </div>
            </div>

            <button
              onClick={skipToCustom}
              className="w-full py-2 rounded-md text-[12px] text-text-muted hover:text-text-primary hover:bg-bg-hover transition-colors"
            >
              + 跳过模板,完全自定义
            </button>
          </div>
        )}

        {/* ── Step 2: Description input ── */}
        {step === "input" && (
          <div className="rounded-xl bg-bg-card p-5 space-y-4">
            {selectedTpl && (
              <div className="flex items-center gap-2.5 p-3 rounded-lg bg-bg-primary">
                <div
                  className="w-8 h-8 rounded-lg flex items-center justify-center text-base shrink-0"
                  style={{ background: PALETTES[selectedTpl.colorTheme].avatar }}
                >
                  {selectedTpl.emoji}
                </div>
                <div className="min-w-0 flex-1">
                  <p className="text-[11.5px] text-text-muted">基于模板</p>
                  <p className="text-[13px] font-medium text-text-primary">{selectedTpl.name}</p>
                </div>
                <button onClick={() => { setSelectedTpl(null); setStep("template"); }} className="text-[11px] text-text-muted hover:text-text-primary">
                  换一个
                </button>
              </div>
            )}
            <div>
              <label className="block text-[11px] text-text-muted uppercase tracking-wider mb-1.5 font-semibold">
                描述你想要的专家
              </label>
              <textarea
                rows={4}
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="越具体越好:要解决什么问题?服务谁?在什么场景下使用?有什么特殊要求?"
                className="w-full rounded-lg bg-bg-tertiary px-3 py-2.5 text-[14px] text-text-primary placeholder:text-text-faint outline-none resize-none"
                autoFocus
              />
              <p className="mt-1.5 text-[11px] text-text-faint">
                提示:不要只写名字,告诉 AI 这个专家要「做什么」「给谁用」「怎么回答」
              </p>
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button
                onClick={() => setStep("template")}
                className="px-3.5 py-2 rounded-lg text-[12.5px] font-medium"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                上一步
              </button>
              <button
                onClick={submitInput}
                disabled={!description.trim()}
                className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-medium disabled:opacity-40"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
              >
                下一步 <ArrowRight size={13} />
              </button>
            </div>
          </div>
        )}

        {/* ── Step 3: Clarify ── */}
        {step === "clarify" && (
          <div className="rounded-xl bg-bg-card p-5 space-y-4">
            <div>
              <p className="text-[13px] font-medium text-text-primary mb-0.5">再补几个关键问题</p>
              <p className="text-[11px] text-text-muted">回答几个问题,AI 会设计得更准</p>
            </div>

            <div>
              <p className="text-[12px] font-medium text-text-secondary mb-2">
                1. 这个专家主要在哪个领域? <span className="text-error">*</span>
              </p>
              <div className="flex flex-wrap gap-1.5">
                {DOMAIN_OPTIONS.map((o) => (
                  <button
                    key={o.id}
                    onClick={() => setClarify((c) => ({ ...c, domain: o.id }))}
                    className={clsx(
                      "px-2.5 py-1.5 rounded-full text-[12px] font-medium transition-all",
                      clarify.domain === o.id
                        ? "bg-accent text-accent-on-text"
                        : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover"
                    )}
                  >
                    <span className="mr-1">{o.emoji}</span>{o.label}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <p className="text-[12px] font-medium text-text-secondary mb-2">2. 对话风格偏好?</p>
              <div className="flex flex-wrap gap-1.5">
                {STYLE_OPTIONS.map((o) => (
                  <button
                    key={o.id}
                    onClick={() => setClarify((c) => ({ ...c, style: o.id }))}
                    className={clsx(
                      "px-2.5 py-1.5 rounded-full text-[12px] font-medium transition-all",
                      clarify.style === o.id
                        ? "bg-accent text-accent-on-text"
                        : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover"
                    )}
                  >
                    {o.label}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <p className="text-[12px] font-medium text-text-secondary mb-2">3. 需要什么工具能力? (可多选)</p>
              <div className="flex flex-wrap gap-1.5">
                {TOOL_OPTIONS.map((o) => {
                  const on = clarify.tools.includes(o.id);
                  return (
                    <button
                      key={o.id}
                      onClick={() => setClarify((c) => ({
                        ...c,
                        tools: on ? c.tools.filter((t) => t !== o.id) : [...c.tools, o.id],
                      }))}
                      className={clsx(
                        "px-2.5 py-1.5 rounded-full text-[12px] font-medium transition-all flex items-center gap-1",
                        on
                          ? "bg-accent text-accent-on-text"
                          : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover"
                      )}
                    >
                      {on && <Check size={11} />}{o.label}
                    </button>
                  );
                })}
              </div>
            </div>

            {error && <p className="text-[12px] text-error">{error}</p>}

            <div className="flex justify-end gap-2 pt-2">
              <button
                onClick={() => setStep("input")}
                className="px-3.5 py-2 rounded-lg text-[12.5px] font-medium"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                上一步
              </button>
              <button
                onClick={submitClarify}
                className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-medium"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
              >
                <Wand2 size={13} /> 生成专家
              </button>
            </div>
          </div>
        )}

        {/* ── Step 4: Preview ── */}
        {step === "preview" && (
          <div className="space-y-4">
            {generating ? (
              <div className="rounded-xl bg-bg-card p-12 flex flex-col items-center justify-center">
                <div className="w-10 h-10 rounded-full border-2 border-bg-tertiary" style={{ borderTopColor: "var(--color-text-primary)" }} />
                <p className="text-[13px] text-text-secondary">AI 正在为你设计专家...</p>
                <p className="text-[11px] text-text-faint mt-1">推理 + 生成人设 + 写 system prompt + 想样例对话</p>
              </div>
            ) : generated ? (
              <>
                {/* Reasoning banner */}
                {generated.reasoning && (
                  <div className="rounded-lg bg-info-soft p-3.5 flex gap-2.5">
                    <div className="text-[15px] shrink-0">💡</div>
                    <div className="flex-1 min-w-0">
                      <p className="text-[10.5px] text-text-muted uppercase tracking-wider mb-0.5 font-semibold">设计思路</p>
                      <p className="text-[12.5px] text-text-secondary leading-relaxed">{generated.reasoning}</p>
                    </div>
                  </div>
                )}

                {/* Card preview */}
                <div className="rounded-xl bg-bg-card overflow-hidden">
                  <div className="p-4">
                    <div className="flex items-start gap-3 mb-3">
                      <div
                        className="w-12 h-12 rounded-xl flex items-center justify-center text-2xl shrink-0 shadow-sm"
                        style={{ background: g.avatar }}
                      >
                        {generated.avatar || "🤖"}
                      </div>
                      <div className="min-w-0 flex-1">
                        <h4 className="text-[15px] font-semibold text-text-primary">{generated.name || "未命名"}</h4>
                        <p className="text-[12px] text-text-muted">{generated.role || "AI 助手"}</p>
                      </div>
                    </div>
                    {generated.identity && (
                      <p className="text-[12.5px] text-text-secondary leading-relaxed mb-2.5">{generated.identity}</p>
                    )}
                    {generated.soul && (
                      <p className="text-[12px] text-text-muted leading-relaxed mb-3 italic">{generated.soul}</p>
                    )}
                    {(generated.skills?.length ?? 0) > 0 && (
                      <div className="flex flex-wrap gap-1 mb-2">
                        {generated.skills!.map((s) => (
                          <span key={s} className="px-1.5 py-0.5 rounded-md text-[10.5px] font-medium" style={{ background: g.chip.bg, color: g.chip.fg }}>{s}</span>
                        ))}
                      </div>
                    )}
                    {generated.dialogStyle && (
                      <p className="text-[11px] text-text-faint">对话风格:{generated.dialogStyle}</p>
                    )}
                  </div>
                </div>

                {/* System prompt editor */}
                <details className="rounded-xl bg-bg-card overflow-hidden group" open>
                  <summary className="flex items-center justify-between p-3.5 cursor-pointer hover:bg-bg-hover">
                    <span className="text-[12.5px] font-medium text-text-primary flex items-center gap-1.5">
                      <Layers size={13} className="text-text-muted" />
                      System Prompt (可编辑)
                    </span>
                    <button
                      onClick={(e) => { e.preventDefault(); navigator.clipboard?.writeText(generated.systemPrompt); }}
                      className="text-[11px] text-text-muted hover:text-text-primary flex items-center gap-1"
                    >
                      <Copy size={11} /> 复制
                    </button>
                  </summary>
                  <textarea
                    value={generated.systemPrompt}
                    onChange={(e) => setGenerated((g) => g ? { ...g, systemPrompt: e.target.value } : g)}
                    rows={6}
                    className="w-full px-3.5 pb-3.5 text-[12px] text-text-secondary font-mono leading-relaxed bg-bg-primary outline-none resize-none"
                  />
                </details>

                {/* Sample dialog */}
                {generated.sampleDialog && (
                  <div className="rounded-xl bg-bg-card p-4 space-y-2">
                    <p className="text-[11px] text-text-muted uppercase tracking-wider font-semibold flex items-center gap-1.5">
                      <MessageSquare size={11} /> 样例对话预览
                    </p>
                    <div className="bg-bg-tertiary rounded-lg px-3 py-2 text-[12.5px] text-text-primary">
                      <span className="text-text-faint text-[10.5px] mr-1.5">用户</span>
                      {generated.sampleDialog.q}
                    </div>
                    <div className="rounded-lg px-3 py-2 text-[12.5px] text-text-primary ml-4" style={{ background: g.chip.bg, color: g.chip.fg }}>
                      <span className="opacity-60 text-[10.5px] mr-1.5">{generated.name || "专家"}</span>
                      <span className="text-text-primary">{generated.sampleDialog.a}</span>
                    </div>
                  </div>
                )}

                {error && <p className="text-[11.5px] text-warning bg-warning-soft p-2 rounded">{error}</p>}

                <div className="flex justify-between items-center pt-2">
                  <button
                    onClick={() => setStep("clarify")}
                    className="text-[12.5px] text-text-muted hover:text-text-primary"
                  >
                    ← 调整参数
                  </button>
                  <div className="flex gap-2">
                    <button
                      onClick={runGeneration}
                      className="flex items-center gap-1.5 px-3.5 py-2 rounded-lg text-[12.5px] font-medium"
                      style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
                    >
                      <RefreshCw size={13} /> 换个版本
                    </button>
                    <button
                      onClick={handleConfirm}
                      className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-[12.5px] font-medium"
                      style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                    >
                      <Check size={13} /> 确认保存
                    </button>
                  </div>
                </div>
              </>
            ) : null}
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Template fallback (when AI API is unreachable) ── */

function generateFromTemplate(
  description: string,
  tpl: AgentTemplate | null,
  clarify: GenClarify,
): GeneratedAgent {
  const domainText = DOMAIN_OPTIONS.find((o) => o.id === clarify.domain)?.label || "通用";
  const styleText = STYLE_OPTIONS.find((o) => o.id === clarify.style)?.label || "简洁专业";
  return {
    name: tpl?.name || description.slice(0, 6) || "新专家",
    avatar: tpl?.emoji || "🤖",
    role: tpl?.role || `${domainText}专家`,
    identity: tpl?.tagline || description.slice(0, 30),
    soul: `专注于 ${domainText} 场景,提供 ${styleText} 的回答`,
    skills: tpl?.skills || ["对话", "分析", "建议"],
    specialty: tpl?.category || domainText,
    dialogStyle: styleText,
    personality: ["专业", "高效", "耐心"],
    systemPrompt: tpl?.systemPrompt || `你是一个 ${domainText} 领域的专家,提供 ${styleText} 的回答。`,
    sampleDialog: tpl?.sampleDialog || {
      q: "你好",
      a: "你好!有什么我可以帮你的吗?",
    },
    reasoning: "(AI 服务不可用,使用了基于模板的快速生成。请检查网络后重新生成。)",
    colorTheme: tpl?.colorTheme || pickColor(description || "agent"),
  };
}

/* ════════════════════════════════════════════════════════════════
   Test dialog (quick preview)
   ════════════════════════════════════════════════════════════════ */

interface TestDialogProps {
  agent: AgentPersona;
  onClose: () => void;
}

function TestDialog({ agent, onClose }: TestDialogProps) {
  const g = PALETTES[agent.colorTheme];
  const [messages, setMessages] = useState<Array<{ role: "user" | "agent"; text: string }>>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  // Auto-play the sample dialog
  useEffect(() => {
    if (agent.sampleDialog && messages.length === 0) {
      setMessages([
        { role: "user", text: agent.sampleDialog.q },
        { role: "agent", text: agent.sampleDialog.a },
      ]);
    }
  }, [agent.sampleDialog]);

  const send = async () => {
    const text = input.trim();
    if (!text || streaming) return;
    setInput("");
    setMessages((prev) => [...prev, { role: "user", text }]);
    setStreaming(true);
    abortRef.current = new AbortController();
    try {
      const apiUrl = storage.getItem(STORAGE_KEYS.agentsServerUrl) || "http://127.0.0.1:3000";
      const apiKey = storage.getItem(STORAGE_KEYS.agentsApiKey) || "";
      const res = await fetch(`${apiUrl}/v1/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json", ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}) },
        body: JSON.stringify({
          messages: [
            { role: "system", content: agent.systemPrompt },
            ...messages.map((m) => ({ role: m.role === "user" ? "user" : "assistant", content: m.text })),
            { role: "user", content: text },
          ],
          stream: true,
        }),
        signal: abortRef.current.signal,
      });
      if (!res.ok || !res.body) throw new Error(`API ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let acc = "";
      setMessages((prev) => [...prev, { role: "agent", text: "" }]);
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        const chunk = decoder.decode(value, { stream: true });
        for (const line of chunk.split("\n").filter((l) => l.startsWith("data: "))) {
          const payload = line.slice(6).trim();
          if (payload === "[DONE]") continue;
          try {
            const json = JSON.parse(payload);
            const delta = json.choices?.[0]?.delta?.content || "";
            acc += delta;
            setMessages((prev) => {
              const next = [...prev];
              next[next.length - 1] = { role: "agent", text: acc };
              return next;
            });
          } catch {}
        }
      }
    } catch (e) {
      setMessages((prev) => [...prev, { role: "agent", text: `(错误:${(e as Error).message})` }]);
    } finally {
      setStreaming(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4" style={{ background: "rgba(0,0,0,0.4)" }} onClick={onClose}>
      <div
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-md h-[600px] max-h-[80vh] rounded-2xl bg-bg-card overflow-hidden flex flex-col shadow-2xl"
      >
        {/* Header */}
        <div className="flex items-center gap-2.5 p-3.5">
          <div
            className="w-9 h-9 rounded-lg flex items-center justify-center text-lg shrink-0"
            style={{ background: g.avatar }}
          >
            {agent.avatar}
          </div>
          <div className="min-w-0 flex-1">
            <p className="text-[13px] font-semibold text-text-primary truncate">{agent.name}</p>
            <p className="text-[10.5px] text-text-muted truncate">{agent.role}</p>
          </div>
          <button onClick={onClose} className="p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-hover">
            <X size={14} />
          </button>
        </div>

        {/* Messages */}
        <div className="flex-1 overflow-y-auto p-3.5 space-y-2">
          {messages.length === 0 ? (
            <div className="h-full flex flex-col items-center justify-center text-center text-text-muted">
              <MessageSquare size={28} className="mb-2 opacity-30" />
              <p className="text-[12px]">开始和 {agent.name} 聊几句</p>
            </div>
          ) : (
            messages.map((m, i) => (
              <div key={i} className={clsx("flex", m.role === "user" ? "justify-end" : "justify-start")}>
                <div
                  className={clsx(
                    "max-w-[80%] rounded-xl px-3 py-2 text-[12.5px] leading-relaxed",
                    m.role === "user"
                      ? "bg-bg-tertiary text-text-primary"
                      : "text-text-primary"
                  )}
                  style={m.role === "agent" ? { background: g.chip.bg } : undefined}
                >
                  {m.text || (streaming && i === messages.length - 1 ? "..." : "")}
                </div>
              </div>
            ))
          )}
        </div>

        {/* Input */}
        <div className="p-3 flex gap-2">
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }}}
            placeholder={`问 ${agent.name} 任何问题...`}
            disabled={streaming}
            className="flex-1 rounded-lg bg-bg-tertiary px-3 py-2 text-[12.5px] text-text-primary placeholder:text-text-faint outline-none"
          />
          <button
            onClick={send}
            disabled={!input.trim() || streaming}
            className="p-2 rounded-lg disabled:opacity-40"
            style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
          >
            <ArrowRight size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}

/* ════════════════════════════════════════════════════════════════
   Main View
   ════════════════════════════════════════════════════════════════ */

export type AgentsTab = "my" | "builtin";

interface AgentsViewProps {
  initialTab?: AgentsTab;
  activeSessionId?: string | null;
  onAfterHire?: () => void;
}

export function AgentsView({
  initialTab = "my",
  activeSessionId = null,
  onAfterHire,
}: AgentsViewProps = {}) {
  const [tab, setTab] = useState<AgentsTab>(initialTab);
  const [agents, setAgents] = useState<AgentPersona[]>(loadAgents);
  const [mode, setMode] = useState<"list" | "create" | "generate">("list");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [sortBy, setSortBy] = useState<"recent" | "popular" | "name">("recent");

  useEffect(() => { saveAgents(agents); }, [agents]);

  /** 切 tab 时如果向导/表单开着,自动重置 — 否则用户切到"内置专家"再切回"我的专家"还会被向导锁住 */
  const switchTab = (newTab: AgentsTab) => {
    if (tab === newTab) return;
    if (mode !== "list") {
      setMode("list");
      setEditingId(null);
    }
    setTab(newTab);
  };

  const handleSave = (data: Partial<AgentPersona>) => {
    const now = Date.now();
    const systemPrompt = data.systemPrompt || buildSystemPrompt(data);
    if (editingId) {
      setAgents((prev) => prev.map((a) =>
        a.id === editingId
          ? { ...a, ...data, systemPrompt, colorTheme: data.colorTheme || a.colorTheme, updatedAt: now }
          : a
      ));
      setEditingId(null);
    } else {
      const newAgent: AgentPersona = {
        id: `agent-${Date.now()}`,
        name: data.name || "未命名专家",
        avatar: data.avatar || "🤖",
        role: data.role || "AI 助手",
        identity: data.identity || "",
        soul: data.soul || "",
        skills: data.skills || [],
        specialty: data.specialty || "",
        dialogStyle: data.dialogStyle || "",
        background: data.background || "",
        personality: data.personality || [],
        systemPrompt,
        model: data.model,
        sampleDialog: data.sampleDialog,
        colorTheme: data.colorTheme || pickColor(data.name || `agent-${Date.now()}`),
        usageCount: 0,
        lastUsedAt: now,
        createdAt: now,
        updatedAt: now,
      };
      setAgents((prev) => [...prev, newAgent]);
    }
    setMode("list");
  };

  const handleDelete = (id: string) => {
    setAgents((prev) => prev.filter((a) => a.id !== id));
  };

  const handleEdit = (id: string) => {
    setEditingId(id);
    setMode("create");
  };

  const handleUse = (id: string) => {
    // Increment usage + stamp lastUsedAt
    setAgents((prev) => prev.map((a) =>
      a.id === id ? { ...a, usageCount: a.usageCount + 1, lastUsedAt: Date.now() } : a
    ));
    onAfterHire?.();
  };

  const handleTest = (id: string) => {
    setTestingId(id);
  };

  const editingAgent = editingId ? agents.find((a) => a.id === editingId) : null;
  const testingAgent = testingId ? agents.find((a) => a.id === testingId) : null;

  // Sort agents
  const sortedAgents = useMemo(() => {
    const list = [...agents];
    if (sortBy === "recent") list.sort((a, b) => b.lastUsedAt - a.lastUsedAt);
    else if (sortBy === "popular") list.sort((a, b) => b.usageCount - a.usageCount);
    else if (sortBy === "name") list.sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
    return list;
  }, [agents, sortBy]);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4">
        <div className="flex items-center gap-2.5">
          <Brain size={20} className="text-text-secondary" />
          <h2 className="text-[16px] font-semibold text-text-primary">专家</h2>
          <span className="text-[11px] text-text-faint font-mono">{agents.length}</span>
        </div>
        <div className="flex items-center gap-1 rounded-lg bg-bg-tertiary p-0.5">
          <button
            onClick={() => switchTab("my")}
            className={clsx(
              "px-3 py-1 rounded-md text-[12.5px]",
              tab === "my" ? "bg-accent-soft text-accent font-medium" : "text-text-muted hover:text-text-primary"
            )}
          >
            我的专家
            {agents.length > 0 && <span className="ml-1 text-[10.5px] text-text-faint">{agents.length}</span>}
          </button>
          <button
            onClick={() => switchTab("builtin")}
            className={clsx(
              "px-3 py-1 rounded-md text-[12.5px]",
              tab === "builtin" ? "bg-accent-soft text-accent font-medium" : "text-text-muted hover:text-text-primary"
            )}
          >
            内置专家
            <span className="ml-1 text-[10.5px] text-text-faint">{BUILTIN_EXPERTS.length}</span>
          </button>
        </div>
        <div className="flex gap-2">
          {tab === "my" && mode === "list" && (
            <>
              <button
                onClick={() => { setEditingId(null); setMode("create"); }}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[13px] font-medium"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                <Plus size={14} /> 手动创建
              </button>
              <button
                onClick={() => setMode("generate")}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[13px] font-medium"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
              >
                <Sparkles size={14} /> AI 生成
              </button>
            </>
          )}
          {tab === "my" && mode !== "list" && (
            <button
              onClick={() => { setMode("list"); setEditingId(null); }}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[13px] font-medium"
              style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
            >
              <ArrowLeft size={14} /> 返回列表
            </button>
          )}
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto">
        {tab === "my" && (
          <>
            {mode === "list" && (
              <div className="p-6">
                {agents.length === 0 ? (
                  <div className="flex flex-col items-center justify-center py-20 text-center">
                    <Bot size={48} className="text-text-faint mb-4" />
                    <h3 className="text-[15px] font-medium text-text-secondary mb-2">还没有专家</h3>
                    <p className="text-[13px] text-text-muted mb-6">手动创建或让 AI 帮你生成一个专家</p>
                    <div className="flex gap-2">
                      <button
                        onClick={() => { setEditingId(null); setMode("create"); }}
                        className="px-4 py-2 rounded-lg text-[13px] font-medium"
                        style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
                      >
                        手动创建
                      </button>
                      <button
                        onClick={() => setMode("generate")}
                        className="px-4 py-2 rounded-lg text-[13px] font-medium"
                        style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                      >
                        AI 生成
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    {/* Sort tabs */}
                    <div className="flex items-center justify-between mb-4">
                      <div className="flex items-center gap-1 text-[11.5px]">
                        <span className="text-text-faint">排序:</span>
                        {([["recent", "最近使用"], ["popular", "最常使用"], ["name", "按名称"]] as const).map(([k, l]) => (
                          <button
                            key={k}
                            onClick={() => setSortBy(k)}
                            className={clsx(
                              "px-2 py-0.5 rounded",
                              sortBy === k ? "bg-bg-tertiary text-text-primary" : "text-text-muted hover:text-text-primary"
                            )}
                          >
                            {l}
                          </button>
                        ))}
                      </div>
                      <p className="text-[11px] text-text-faint">{sortedAgents.length} 个专家</p>
                    </div>

                    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
                      {sortedAgents.map((agent) => (
                        <AgentCard
                          key={agent.id}
                          agent={agent}
                          onEdit={() => handleEdit(agent.id)}
                          onDelete={() => handleDelete(agent.id)}
                          onTest={() => handleTest(agent.id)}
                          onUse={() => handleUse(agent.id)}
                        />
                      ))}
                    </div>
                  </>
                )}
              </div>
            )}

            {mode === "create" && (
              <div className="p-6">
                <div className="max-w-2xl mx-auto">
                  <h3 className="text-[15px] font-semibold text-text-primary mb-4">
                    {editingAgent ? `编辑「${editingAgent.name}」` : "创建新专家"}
                  </h3>
                  <div className="rounded-xl bg-bg-card p-5">
                    <AgentForm
                      initial={editingAgent || undefined}
                      onSave={handleSave}
                      onCancel={() => { setMode("list"); setEditingId(null); }}
                    />
                  </div>
                </div>
              </div>
            )}

            {mode === "generate" && (
              <AIGenerateWizard
                onComplete={handleSave}
                onCancel={() => setMode("list")}
              />
            )}
          </>
        )}

        {tab === "builtin" && (
          <BuiltInExpertsTab
            activeSessionId={activeSessionId}
            onAfterHire={onAfterHire}
          />
        )}
      </div>

      {/* Test dialog */}
      {testingAgent && (
        <TestDialog agent={testingAgent} onClose={() => setTestingId(null)} />
      )}
    </div>
  );
}

/* ════════════════════════════════════════════════════════════════
   Built-in Experts tab (unchanged)
   ════════════════════════════════════════════════════════════════ */

function BuiltInExpertsTab({
  activeSessionId,
  onAfterHire,
}: {
  activeSessionId: string | null;
  onAfterHire?: () => void;
}) {
  const [search, setSearch] = useState("");
  const [activeCategory, setActiveCategory] = useState<string>("全部");
  const { expert, setExpert } = useSessionExpert(activeSessionId);

  const filtered = useMemo(() => {
    let list: ExpertDef[] = BUILTIN_EXPERTS;
    if (activeCategory !== "全部") {
      list = list.filter((e) => e.category === activeCategory);
    }
    if (search.trim()) {
      const q = search.trim().toLowerCase();
      list = list.filter(
        (e) =>
          e.name.toLowerCase().includes(q) ||
          e.role.toLowerCase().includes(q) ||
          e.description.toLowerCase().includes(q) ||
          e.category.toLowerCase().includes(q),
      );
    }
    return list;
  }, [activeCategory, search]);

  const handleHire = async (e: ExpertDef) => {
    // 单击即可：选择专家并跳转回对话界面，不做取消/切换
    if (onAfterHire) await onAfterHire();
    setExpert(e.id);
  };

  return (
    <div className="p-6">
      <div className="max-w-7xl mx-auto">
        <p className="text-[12.5px] text-text-muted mb-4">
          Hermix 内置的专家角色 — 涵盖产品、技术、金融、营销、内容创作等领域。点击"使用"即可让当前对话采纳此专家的人设和工具集。
        </p>
        <div className="flex items-center gap-2 mb-3">
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索专家、角色、类别..."
            className="flex-1 px-3 py-1.5 rounded-md bg-bg-card text-[12.5px] text-text-primary placeholder-text-faint outline-none"
          />
        </div>
        <div className="flex items-center gap-1.5 mb-5 overflow-x-auto" style={{ scrollbarWidth: "none" }}>
          <CategoryChip
            active={activeCategory === "全部"}
            onClick={() => setActiveCategory("全部")}
            label="全部"
            count={BUILTIN_EXPERTS.length}
          />
          {EXPERT_CATEGORIES.map((cat) => {
            const count = BUILTIN_EXPERTS.filter((e) => e.category === cat).length;
            if (count === 0) return null;
            return (
              <CategoryChip
                key={cat}
                active={activeCategory === cat}
                onClick={() => setActiveCategory(cat)}
                label={cat}
                count={count}
              />
            );
          })}
        </div>
        {filtered.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-center text-text-muted">
            <Bot size={36} className="mb-3 opacity-40" />
            <p className="text-sm">没有匹配的内置专家</p>
            <p className="text-[11px] text-text-faint mt-1">试试其他类别或搜索词</p>
          </div>
        ) : (
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3">
            {filtered.map((e) => (
              <ExpertCard
                key={e.id}
                expert={e}
                isHired={expert?.id === e.id}
                canHire
                onHire={() => handleHire(e)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function CategoryChip({
  active, onClick, label, count,
}: { active: boolean; onClick: () => void; label: string; count: number }) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "flex items-center gap-1 px-2.5 py-1 rounded-full text-[11.5px] whitespace-nowrap",
        active
          ? "bg-accent text-white"
          : "bg-bg-tertiary text-text-muted hover:text-text-primary",
      )}
    >
      {label}
      <span className={clsx("text-[10px]", active ? "text-white/80" : "text-text-faint")}>{count}</span>
    </button>
  );
}

function ExpertCard({
  expert, isHired, canHire, onHire,
}: { expert: ExpertDef; isHired: boolean; canHire: boolean; onHire: () => void }) {
  return (
    <div
      className="group relative rounded-xl bg-bg-card p-3.5 transition-all hover:-translate-y-0.5 hover:shadow-md cursor-pointer"
      onClick={canHire ? onHire : undefined}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => { if (canHire && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); onHire(); } }}
    >
      <div className="flex items-start gap-2.5">
        {/* Round avatar */}
        <div className="w-9 h-9 rounded-full flex items-center justify-center text-[20px] shrink-0 bg-bg-tertiary">
          {expert.avatar}
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex items-start justify-between gap-2 mb-0.5">
            <div className="min-w-0 flex-1">
              <h3 className="text-[12.5px] font-semibold text-text-primary truncate leading-tight">
                {expert.name}
              </h3>
              <p className="text-[10.5px] text-text-muted truncate mt-0.5">
                {expert.role}
              </p>
            </div>
            <span
              className={clsx(
                "text-[9.5px] font-medium px-1.5 py-0.5 rounded shrink-0 mt-0.5",
                isHired
                  ? "bg-accent text-accent-on-text"
                  : "bg-bg-primary text-text-secondary"
              )}
            >
              {isHired ? "使用中" : "待邀"}
            </span>
          </div>

          <p className="text-[11px] text-text-secondary leading-relaxed line-clamp-2 mt-1 min-h-[2.4em]">
            {expert.description}
          </p>

          <div className="flex flex-wrap gap-1 mt-2">
            <span className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary">
              {expert.category}
            </span>
            {expert.toolset && (
              <span className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary">
                {expert.toolset}
              </span>
            )}
          </div>
        </div>
      </div>

      <div className="flex items-center gap-1.5 mt-2.5">
        <button
          onClick={onHire}
          disabled={!canHire}
          className={clsx(
            "flex-1 flex items-center justify-center gap-1.5 py-1.5 rounded-lg text-[11.5px] font-medium transition-all",
            isHired
              ? "hover:bg-bg-hover"
              : "hover:opacity-90",
            !canHire && "opacity-50 cursor-not-allowed",
          )}
          style={isHired
            ? { background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }
            : { background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }
          }
        >
          {isHired ? <><Check size={11} /> 使用此专家</> : canHire ? "使用此专家" : "请先打开对话"}
        </button>
      </div>
    </div>
  );
}
