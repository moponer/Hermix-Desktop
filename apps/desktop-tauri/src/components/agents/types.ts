/* ════════════════════════════════════════════════════════════════
   Agents domain types — pure data, no React, no side effects.
   Re-exported from AgentsView.tsx for back-compat with TaskPanel,
   GroupChatView, RoomMemberDialog (which import AgentPersona).
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

export const PALETTES: Record<AgentColorTheme, ColorPalette> = {
  violet:  { banner: "#6d5ad6", avatar: "#ece9fb", chip: { bg: "#ece9fb", fg: "#5b4ec1" } },
  rose:    { banner: "#e0518a", avatar: "#fce4ee", chip: { bg: "#fce4ee", fg: "#b1356a" } },
  cyan:    { banner: "#2b8fd6", avatar: "#e1f1fb", chip: { bg: "#e1f1fb", fg: "#1a6aa3" } },
  emerald: { banner: "#2e9d6b", avatar: "#defaed", chip: { bg: "#defaed", fg: "#1f6f4a" } },
  amber:   { banner: "#c2841a", avatar: "#fbeed1", chip: { bg: "#fbeed1", fg: "#8a5d10" } },
  teal:    { banner: "#2c7873", avatar: "#d9eeec", chip: { bg: "#d9eeec", fg: "#1d5754" } },
  indigo:  { banner: "#5b58c9", avatar: "#e7e5fb", chip: { bg: "#e7e5fb", fg: "#403cb0" } },
  fuchsia: { banner: "#b94a8f", avatar: "#fbe0ee", chip: { bg: "#fbe0ee", fg: "#8a326b" } },
};

export const COLOR_KEYS: AgentColorTheme[] = Object.keys(PALETTES) as AgentColorTheme[];

/** Stable color picked from agent name so visually similar agents don't get same color. */
export function pickColor(seed: string): AgentColorTheme {
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

export const TEMPLATES: AgentTemplate[] = [
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

export const AVATARS = ["🤖", "🧠", "🎯", "🔮", "⚡", "🛡️", "🔬", "📚", "🎨", "🦊", "🐉", "🌟", "💡", "🚀", "🌊", "🔥", "🧙", "🦉"];