// components/strings.ts —— 界面文案常量。
//
// 为什么要有这个模块（2026-10-21 前端设计审计 §4.2 问题 12）：
// 重复字面量既是**可见文案**又是 **aria-label** —— 「关闭」×21、「删除」×14、
// 「已复制」×10、「新建对话」×9、「保存」×7、「已启用」×6、「失败」×6。
// 同一串字散落在几十个文件里，改一次漏一处，可见文案与可访问名就会分叉。
// 收进常量模块后，一处改动同时修好两者。
//
// 收录规则：
// · 一个动作一个词（问题 13）：保存 = 「保存」/「保存中…」/「已保存」。
// · 省略号一律全角「…」（U+2026），禁 ASCII 三点。
// · 冒号与括号一律全角：「：」「（）」。
// · 错误文案模板固定「<发生了什么>，<下一步做什么>」——见 failure()。

// ── 动作按钮 ────────────────────────────────────────────────────

export const CLOSE = "关闭";
export const DELETE = "删除";
export const CANCEL = "取消";
export const CONFIRM = "确认";
export const SAVE = "保存";
export const ALL = "全部";
export const NEW_CHAT = "新建对话";

// ── 进行中（后缀统一全角省略号）────────────────────────────────────

export const SAVING = "保存中…";
export const PACKAGING = "打包中…";
/** 纯文字加载态在全仓只有这一种措辞（审计问题 58）。 */
export const LOADING = "读取中…";

// ── 完成态 ──────────────────────────────────────────────────────

export const SAVED = "已保存";
export const COPIED = "已复制";
export const ENABLED = "已启用";
export const COMPLETED = "已完成";
export const EXECUTED_COMMAND = "已执行命令";
export const NO_OUTPUT = "（无输出）";

// ── 失败 ────────────────────────────────────────────────────────

export const FAILED = "失败";
export const SAVE_FAILED = "保存失败";
export const COPY_FAILED = "复制失败";
export const LOAD_FAILED = "读取失败";

// ── 复制动作 ────────────────────────────────────────────────────

export const COPY_OUTPUT = "复制输出";
export const COPY_MESSAGE = "复制消息";

/**
 * 错误文案模板：「<发生了什么>，<下一步做什么>」。
 *
 * 审计问题 52 与 §4.2 规范建议：同一失败不能再有三种说法（"复制失败" /
 * "复制失败，请检查剪贴板权限" / "复制失败，请手动选中复制"）。
 * 仓库里的正面样本是 App.tsx:3602、main.tsx:160、AutomationView.tsx:162、
 * McpCatalogGrid.tsx:131-143 —— 给出原因 + 替代路径 + 可执行的下一步。
 *
 * @param what    发生了什么（用 SAVE_FAILED / COPY_FAILED 等常量）
 * @param next    下一步做什么（祈使句）
 * @param detail  可选：后端返回的原始原因
 */
export function failure(what: string, next: string, detail?: string): string {
  const reason = detail ? `${what}：${detail}` : what;
  return `${reason}，${next}`;
}
