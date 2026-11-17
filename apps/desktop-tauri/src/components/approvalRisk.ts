/**
 * 审批风险等级的**唯一**文案来源。
 *
 * 四张卡（`ApprovalBubble` / `ApprovalBatchBubble` / `ApprovalDrawer` /
 * `InlineApprovalCard`）此前各自维护一份：三份叫「高风险/中风险/低风险」，
 * 内联卡叫「高危/中危/低危」——同一个后端字段在同一个界面上有两种说法。
 *
 * 只共享文案，不共享归一化默认值：`ApprovalBubble` 缺字段时按 caution（能弹卡
 * 的最轻等级），`InlineApprovalCard` 按 dangerous（没有风险信息时按最坏处理）。
 * 这两个默认值都是有意为之，见各自文件内的注释。
 */
export type ApprovalRisk = "dangerous" | "caution" | "safe";

export const APPROVAL_RISK_LABEL: Record<ApprovalRisk, string> = {
  dangerous: "高风险",
  caution: "中风险",
  safe: "低风险",
};

/**
 * 工具权限范围的**唯一**文案来源（B8-1：审批卡名称项混淆修复）。
 *
 * 用户反馈：审批卡只显示工具名（terminal / execute_code / read_file…），
 * 分不清各自的权限范围。这里维护「工具名 → 权限范围一句话」的映射，
 * 四张审批卡统一引用。匹配按子串命中（与 toolIcon 同风格），首个命中生效。
 */
const TOOL_CAPABILITY_RULES: [needle: string, text: string][] = [
  ["terminal", "可执行任意系统命令（含读写文件、联网、安装/删除）"],
  ["shell", "可执行任意系统命令（含读写文件、联网、安装/删除）"],
  ["exec", "可执行命令（含读写文件、联网）"],
  ["browser", "联网浏览网页（只读，不出本机数据）"],
  ["fetch", "联网读取内容（只读）"],
  ["execute_code", "在受限沙箱内运行代码（文件系统受限、断网）"],
  ["code", "在受限沙箱内运行代码（文件系统受限）"],
  ["write_file", "写入/覆盖指定文件（其余文件不可碰）"],
  ["patch", "修改指定文件（最小差异编辑）"],
  ["read_file", "只读指定文件（不可写）"],
  ["read_files", "只读指定的多个文件（不可写）"],
  ["search", "只读搜索（文件名/内容匹配，不可写）"],
  ["list_dir", "只读目录列表（不可写）"],
  ["delegate", "派发子代理执行任务（工具面由角色档位决定）"],
  ["memory", "读写会话记忆库（仅记忆条目）"],
];

export function toolCapability(tool: string): string | null {
  const t = tool.toLowerCase();
  for (const [needle, text] of TOOL_CAPABILITY_RULES) {
    if (t.includes(needle)) return text;
  }
  return null;
}
