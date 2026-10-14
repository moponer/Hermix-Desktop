/**
 * 最小化 markdown → HTML 渲染器。
 *
 * 只处理对话里最常见的格式，不引入任何依赖：
 *   代码块 ```…```、行内 `code`、**粗体**、*斜体*、
 *   [链接](url)、列表、标题、表格（| col | col |）
 *
 * 全部单次正则替换，不递归——这对对话气泡的复杂度已经足够。
 */
export function renderMarkdown(text: string): string {
  // 代码块（先处理，防止内部 ** 等被二次替换）
  text = text.replace(/```(\w*)\n?([\s\S]*?)```/g, (_, _lang, code) => {
    const esc = code.trimEnd()
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
    return `<pre class="md-code"><code>${esc}</code></pre>`
  })

  // 行内代码
  text = text.replace(/`([^`]+)`/g, '<code class="md-code-inline">$1</code>')

  // 粗体（**text**）
  text = text.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')

  // 斜体（*text*，但不是 **）
  text = text.replace(/(?<!\*)\*([^*\n]+)\*(?!\*)/g, '<em>$1</em>')

  // 链接 [text](url)
  text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a class="md-link" href="$2" target="_blank">$1</a>')

  // 标题 ### ###
  text = text.replace(/^#{1,3} (.+)$/gm, '<strong class="md-h">$1</strong>')

  // 无序列表 - item（每行开头）
  text = text.replace(/^- (.+)$/gm, '<li class="md-li">$1</li>')
  // 合并连续的 <li>
  text = text.replace(/(<li class="md-li">.*<\/li>(\n<li class="md-li">.*<\/li>)*)/g, '<ul class="md-ul">$1</ul>')

  // 表格 | col1 | col2 |（在所有块替换之后处理，防止代码块里的 | 被误判）
  text = text.replace(
    /\|.+\|\n\|[-: |]+\|\n((?:\|[^\n]+\|\n?)*)/,
    (_, body) => {
      const lines = body.trim().split('\n')
      // 每行："| a | b | c |" → [" a ", " b ", " c "]
      const rows = lines.map((line) => {
        const cells: string[] = []
        // 去掉首尾 |，剩余 " a | b | c "，再按 | 分割
        const inner = line.replace(/^\|/, '').replace(/\|$/, '')
        inner.split('|').forEach((raw) => {
          const c = raw
            .replace(/\\\|/g, '|')
            .replace(/\\`/g, '`')
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
          cells.push(`<td class="md-td">${c}</td>`)
        })
        return `<tr>${cells.join('')}</tr>`
      })
      return `<table class="md-table"><tbody>${rows.join('')}</tbody></table>`
    },
  )

  // 换行保留（最后处理，保证 <table> 等块级元素内部不受影响）
  text = text.replace(/\n/g, '<br>')

  return text
}
