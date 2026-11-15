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
