// 增强提示词（Prompt Enhancement）的前端契约。
//
// 能力定位：把一句模糊需求（"帮我写方案"）扩写成结构化、专业、
// 可直接投喂 AI 的提示词——角色 / 背景 / 任务拆解 / 输入 / 约束 /
// 输出格式六段，段内再按草稿隐含的场景（周报/方案/竞品分析…）落到
// 具体的产出规格。与另外两个模板互不共用：
// - polish.chat：对话润色（改写得更清楚，长度不放大、不加结构）
// - polish.automation：自动化 PROMPT 改写（保留 {{变量}} 占位符）
// - polish.enhance：**增强**（扩写 + 补齐结构 + 场景规格）
//
// 提示词模板本身**没有**前端副本：它在
// `crates/hermix-server/src/dashboard/polish.rs` 的 `ENHANCE_PROMPT`
// （唯一副本，避免多语言副本漂移）。场景规格表也住在那里，由模型按
// 草稿自动选用最贴切的一条——**用户不需要选场景**。

export interface EnhanceInput {
  /** 用户当前草稿（通常只有一句话） */
  draft: string;
  ctx?: {
    /** 最近 1-2 条对话，让增强出的提示词接得上上下文 */
    recentTurns?: string;
    /** 是否带附件（影响提示词里要不要交代附件内容） */
    hasAttachments?: boolean;
  };
}
