// 工具入参 → git 风格增删行数。**零依赖**纯函数。
//
// 为什么独立成文件（而不是塞进 `bubbleUtils` 或 `ToolOutputPreviews`）：
// 两个调用方分属不同的层 —— `messageParser`（纯解析，不该 import 任何
// 组件 / 图标模块）和 `ProcessLine`（组件）。放进 `bubbleUtils` 会让解析器
// 为了 25 行代码拖进整个 lucide 映射树。同 `utils/toolOutcome.ts` 的做法：
// 零依赖，谁都能引。
//
// ⚠ 诚实性约束（展示它的地方必须带上这句）：这不是**对磁盘做的真 diff**，
// 是按工具**入参**估算的。
// - `patch` 若把 `old_string` 在文件里替换了 N 次，真实删除是 N 倍 ——
//   所以这个数是**下界**，只会偏小，不会虚报。
// - 不产出行数的工具（`create_file` / `delete_file` / 任何非写工具）返回
//   `{}` 而不是 `{added: 0}`：调用方据此**不显示**这一列，而不是显示一个
//   看起来精确的 0。
export function extractLineStats(
  tool: string,
  argsJson?: string,
): { added?: number; removed?: number } {
  if (!argsJson) return {};
  let args: Record<string, unknown>;
  try {
    args = JSON.parse(argsJson);
  } catch {
    return {};
  }
  const t = tool.toLowerCase();
  if (t.includes("write_file") && typeof args.content === "string") {
    return { added: args.content.split("\n").length };
  }
  if ((t.includes("patch") || t.includes("edit_file")) && typeof args.new_string === "string") {
    const added = args.new_string.split("\n").length;
    const removed = typeof args.old_string === "string" ? args.old_string.split("\n").length : 0;
    return { added, removed };
  }
  return {};
}
