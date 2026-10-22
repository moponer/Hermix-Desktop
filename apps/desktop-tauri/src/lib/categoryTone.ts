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

/** 关键词 → 色调。命中即返回，先到先得（数组顺序即优先级）。 */
const KEYWORD_TONE: ReadonlyArray<readonly [readonly string[], IconTone]> = [
  // 智能 / 技能 / 模型 —— 最独特的一类，放最前避免被 "agent" 之类误吞
  ([["ai", "skill", "agent", "llm", "model", "bot", "assistant", "智能", "技能", "模型", "助手", "expert", "专家"] as const, "violet"] as const),
  ([["memory", "persona", "soul", "chat", "message", "social", "heart", "记忆", "人格", "社交", "通讯"] as const, "rose"] as const),
  ([["automation", "cron", "schedule", "task", "timer", "todo", "定时", "任务", "自动化", "日程", "待办"] as const, "amber"] as const),
  ([["media", "audio", "video", "music", "image", "photo", "camera", "film", "媒体", "音视频", "图像", "图片", "截图"] as const, "orange"] as const),
  ([["database", "db", "sql", "data", "search", "index", "检索", "数据", "数据库", "查询"] as const, "cyan"] as const),
  ([["file", "doc", "document", "office", "pdf", "note", "folder", "sheet", "文件", "文档", "办公", "笔记"] as const, "green"] as const),
  ([["mcp", "connector", "network", "server", "cloud", "api", "web", "http", "连接器", "网络", "服务", "云"] as const, "blue"] as const),
];

/**
 * 按类目名 / 描述推色调。匹配用子串而非相等 —— 类目名是自由文本
 * （"software-development"、"mcp-filesystem"、中文"办公"），穷举枚举不现实。
 * 未命中一律 slate（无彩色档），保证不会出现「猜一个颜色却猜错」。
 */
export function toneForCategory(category?: string | null): IconTone {
  if (!category) return "slate";
  const key = category.toLowerCase();
  for (const [words, tone] of KEYWORD_TONE) {
    if (words.some((w) => key.includes(w))) return tone;
  }
  return "slate";
}

/** 给 className 用：`cat-icon--blue`。 */
export function toneClass(tone: IconTone, prefix: "cat-icon" | "cat-ink" = "cat-icon"): string {
  return `${prefix}--${tone}`;
}
