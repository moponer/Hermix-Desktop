/**
 * 工具调用的一行人话标签（`terminal: Get-CimInstance …`）。
 *
 * 值守页「刚才发生」此前只写工具名 —— 屏幕上连着三行 `terminal`，用户
 * 看不出她到底跑了什么命令（2026-11-06 实报截图）。
 *
 * 规则与后端 `sub_agent_orchestrator::tool_step_label` 同族：取第一个有值的
 * 关键参数、压成一行、截 60 字；取不到就退回工具名，绝不编造。两处刻意的
 * 差别：① **命令优先**（后端从 `path` 起，因为子代理面板多是读写文件，
 * 而值守页要看的是命令）；② **保留完整工具名**（后端取 `split('_').last()`
 * 会把 `read_file` 缩成 `file`，这里是给人看的，工具名本身就是信息）。
 */
const PRIMARY_KEYS = ["command", "path", "query", "url", "tool", "target"] as const;

/** 单行标签的最大字符数（与后端 tool_step_label 的 60 一致）。 */
export const TOOL_LABEL_MAX = 60;

/** 参数对象的浅取值（未知形状一律当没有，不抛错）。 */
function primaryArg(args: unknown): string | undefined {
  if (!args || typeof args !== "object") return undefined;
  const obj = args as Record<string, unknown>;
  for (const key of PRIMARY_KEYS) {
    const v = obj[key];
    if (typeof v === "string" && v.trim()) return v.trim();
  }
  return undefined;
}

export function toolStepLabel(tool: string, args?: unknown): string {
  const primary = primaryArg(args);
  if (!primary) return tool;
  // 多行命令（PowerShell here-string / 管道换行）压成一行：行内的换行会把
  // 单行读数撑成多行，破坏「一条动作一行」的排版。
  const oneLine = primary.replace(/\s+/g, " ");
  const short =
    oneLine.length > TOOL_LABEL_MAX
      ? `${oneLine.slice(0, TOOL_LABEL_MAX - 1)}…`
      : oneLine;
  return `${tool}: ${short}`;
}
