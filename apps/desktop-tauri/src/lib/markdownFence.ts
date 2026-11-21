/**
 * Fence auto-close preprocessor for streaming LLM replies.
 *
 * LLM streaming sometimes drops the closing ``` of a fenced code block
 * (truncation, token limit, model drift). Without a closing fence,
 * react-markdown keeps eating subsequent paragraphs as code — turning
 * prose into "CODE" + copy-button boxes (visible in screenshots from
 * 2026-08-19 where "Let me look the at end of the file..." rendered as
 * a code block). This preprocessor scans the content for fence-only
 * lines and appends a closing ``` when the count is unbalanced.
 *
 * Rules:
 *   - count lines matching `^\s*(```+|~~~+)` (3+ backticks/tildes);
 *   - inline `code` on a non-fence line is ignored;
 *   - a stray single backtick (e.g. the prose "use the ```-fence")
 *     is ignored — only 3+ consecutive backticks at line start count;
 *   - if the running count is odd, append `\n````\``` to the end.
 *
 * Lifted out of MarkdownBody.tsx so the function is testable without
 * pulling in react-markdown.
 */
export function autoCloseUnbalancedFences(content: string): string {
  const lines = content.split("\n");
  let fenceCount = 0;
  for (const line of lines) {
    if (/^\s*(```+|~~~+)/.test(line)) fenceCount++;
  }
  if (fenceCount % 2 === 1) return content + "\n```";
  return content;
}

/**
 * 给**裸围栏**（``` 之后不写语言）补一个 `text` info string。
 *
 * 为什么要补：hast 把围栏块和行内 code 都降成 <code>，渲染器只能靠
 * 「有没有 language-xxx 类」或「内容里有没有换行」区分。裸围栏两者皆无
 * ——单行内容会被当成行内 code，整段落在一颗药丸底色的 <code> 里，
 * 既没有代码块外壳的底色，也没有语言标签和复制按钮。用户看到的是
 * 「项目结构这块不像 markdown 渲染」（2026-11-18 实拍反馈）。
 *
 * 只改**开**围栏（收围栏带 info string 是非法写法，不能碰）。
 */
export function labelBareFences(content: string): string {
  const lines = content.split("\n");
  let inFence = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const fence = /^(\s*)(`{3,}|~{3,})(.*)$/.exec(line);
    if (!fence) continue;
    if (inFence) {
      inFence = false;
      continue;
    }
    inFence = true;
    if (fence[3]!.trim() === "") lines[i] = `${fence[1]}${fence[2]}text`;
  }
  return lines.join("\n");
}