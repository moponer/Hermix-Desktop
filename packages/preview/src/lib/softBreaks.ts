/**
 * Soft → hard break preprocessor.
 *
 * satteri (CommonMark default) collapses single newlines inside a
 * paragraph into a single space. `remark-breaks` previously treated
 * every `\n` as a hard break. Hard-break encoding in CommonMark is
 * two trailing spaces + `\n` ("  \n"); this preprocessor inserts
 * those two spaces for every `\n` that lives strictly inside a
 * paragraph / list item / heading, so visual output matches the old
 * `remark-breaks` behavior.
 *
 * Lines inside a fenced code block are untouched.
 * Paragraph boundaries (blank lines) are untouched.
 */

const FENCE_RE = /^\s*(```+|~~~+)/;

/**
 * 为单行末尾追加两个空格（CommonMark hard break 标记）。
 * 空行（`""`）不变：它是段边界。
 * 已经在 fence 内的行不变。
 */
export function hardenSoftLineBreaks(content: string): string {
  const lines = content.split("\n");
  const out: string[] = [];
  let inFence = false;
  let fenceMarker = "";
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const m = FENCE_RE.exec(line);
    if (m) {
      const marker = m[1]!;
      if (!inFence) {
        inFence = true;
        fenceMarker = marker;
      } else if (marker.startsWith(fenceMarker)) {
        inFence = false;
        fenceMarker = "";
      }
    }
    // 段尾（下一行是空行或文件末）跳过，fence 内跳过
    const nextBlank = i + 1 >= lines.length || lines[i + 1] === "";
    if (inFence || nextBlank || line === "" || /\s$/.test(line)) {
      out.push(line);
    } else {
      out.push(line + "  ");
    }
  }
  return out.join("\n");
}
