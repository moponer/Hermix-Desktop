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