/**
 * 分类 → 图标色调。
 *
 * 为什么要这一层（2026-10-23）：配置 / 设置域此前所有图标同一档灰，色相
 * 完全不参与信息传达。彩色化的价值不在「好看」，而在**扫视速度** —— 人眼
 * 在列表里先抓色块再读文字，灰图标等于关掉这条通道。
 *
 * 纪律（与 icon-tone.css 配套）：
 *  1. 这里只产**色调名**，不产颜色值。颜色一律由 CSS 变量 --color-tone-*
 *     供给，主题切换自动跟随 —— tsx 里出现字面色值会被 check-design-guard
 *     的 G1 拦下。
 *  2. 色调只涂**图标**，不涂文字。文字对比度另有一套 --color-text-* 安全档，
 *     用身份色写小字会掉到 4.5:1 以下。
 *  3. 与状态语义色物理隔离：success/warning/error/info 表达「状态」，
 *     tone-* 表达「是什么」，两者不复用同一个名字，避免把「数据库」读成
 *     「成功」。
 */

export type IconTone =
  | "slate"
  | "blue"
  | "cyan"
  | "green"
  | "amber"
  | "orange"
  | "rose"
  | "violet";

/**
 * 已知闭集类目 → 色调（精确匹配，大小写不敏感）。
 *
 * 为什么需要这一层（2026-10-23）：关键词兜底对**短类目名**几乎无效 ——
 * 技能页 7 个分类全是 slug（software-development / devops / productivity /
 * finance / lifestyle / writing / office），其中只有 office 恰好含 "office"
 * 一条关键词，其余 6 个全部回退成 slate。插件页 13 个中文类目（编程 / 产品 /
 * 翻译 / 内容创作 …）也一个都命中不了。**结果是四页里三页的图标仍然是灰的**，
 * 与"按类目上色"的目标正好相反。
 *
 * 结论：类目在数据侧是**闭集**，闭集就该枚举，不该靠猜。
 * 兜底逻辑只为将来新增的未知类目服务。
 *
 * 分档口径（与 icon-tone.css 的 8 档一致）：
 *   violet 智能规划 · blue 开发运维 · cyan 数据检索 · green 文档写作
 *   amber 事务商业 · orange 创作媒体 · rose 沟通语言 · slate 通用未知
 */
const EXPLICIT_TONE: Readonly<Record<string, IconTone>> = {
  // ── 技能分类（后端闭集，CATEGORY_LABELS 的 7 个 key）─────────
  "software-development": "blue",
  devops: "blue",
  productivity: "amber",
  finance: "amber",
  lifestyle: "rose",
  writing: "green",
  office: "green",
  uncategorized: "slate",

  // ── 插件类目（中文闭集）───────────────────────────────────
  编程: "blue",
  编程开发: "blue",
  运维: "blue",
  开发: "blue",
  产品: "violet",
  学习: "violet",
  学习答疑: "violet",
  数据: "cyan",
  数据分析: "cyan",
  翻译: "rose",
  咨询: "rose",
  工作: "amber",
  商业: "amber",
  写作: "green",
  文档: "green",
  内容创作: "orange",
  媒体: "orange",

  // ── 工具集 / MCP 目录（英文与中文混排）───────────────────
  mcp: "blue",
  tools: "blue",
  toolset: "blue",
  办公: "green",
  检索: "cyan",
  通讯: "rose",
  通用: "slate",
  其它: "slate",
};

/** 关键词 → 色调。命中即返回，先到先得（数组顺序即优先级）。 */
const KEYWORD_TONE: ReadonlyArray<readonly [readonly string[], IconTone]> = [
  // 智能 / 技能 / 模型 —— 最独特的一类，放最前避免被 "agent" 之类误吞
  ([["ai", "skill", "agent", "llm", "model", "bot", "assistant", "智能", "技能", "模型", "助手", "expert", "专家", "learn", "study", "学习", "product", "产品"] as const, "violet"] as const),
  ([["memory", "persona", "soul", "chat", "message", "social", "heart", "记忆", "人格", "社交", "通讯", "translat", "翻译", "consult", "咨询", "life", "生活"] as const, "rose"] as const),
  ([["automation", "cron", "schedule", "task", "timer", "todo", "定时", "任务", "自动化", "日程", "待办", "work", "效率", "工作", "financ", "金融", "商业"] as const, "amber"] as const),
  ([["media", "audio", "video", "music", "image", "photo", "camera", "film", "媒体", "音视频", "图像", "图片", "截图", "creative", "创作", "设计"] as const, "orange"] as const),
  ([["database", "db", "sql", "data", "search", "index", "检索", "数据", "数据库", "查询", "analy"] as const, "cyan"] as const),
  ([["file", "doc", "document", "office", "pdf", "note", "folder", "sheet", "write", "文件", "文档", "办公", "笔记", "写作"] as const, "green"] as const),
  ([["mcp", "connector", "network", "server", "cloud", "api", "web", "http", "dev", "code", "git", "term", "shell", "cli", "cmd", "console", "browser", "chrom", "连接器", "网络", "服务", "云", "编程", "开发", "运维", "终端", "命令", "浏览器"] as const, "blue"] as const),
];

/**
 * 按类目名推色调。先用闭集表精确匹配，未命中再走关键词子串兜底
 * （类目也可能是自由文本，如 "mcp-filesystem"、"software-development"）。
 * 仍未命中返回 slate —— 宁可无色，也不猜一个会读错的颜色。
 */
export function toneForCategory(category?: string | null): IconTone {
  if (!category) return "slate";
  const key = category.toLowerCase().trim();
  const exact = EXPLICIT_TONE[key];
  if (exact) return exact;
  for (const [words, tone] of KEYWORD_TONE) {
    if (words.some((w) => key.includes(w))) return tone;
  }
  return "slate";
}

/** 给 className 用：`cat-icon--blue`。 */
export function toneClass(tone: IconTone, prefix: "cat-icon" | "cat-ink" = "cat-icon"): string {
  return `${prefix}--${tone}`;
}
