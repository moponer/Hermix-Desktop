// 极简 Markdown 渲染（与 BBS sanitize 同策略）：先整体转义再生成白名单
// 标签。查看器一期预览级——标题/列表/行内代码/围栏/粗斜体/链接够用；
// 不引 markdown 依赖（六步阶梯；主应用的 MarkdownBody 拖整个构建链）。

const ESC: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

export function escapeHtml(s: string): string {
  return String(s).replace(/[&<>"']/g, (c) => ESC[c]);
}

function safeHref(raw: string): boolean {
  return /^(https?:\/\/)/i.test(raw);
}

function inline(src: string): string {
  let s = escapeHtml(src);
  s = s.replace(/`([^`]+)`/g, "<code>$1</code>");
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, t: string, u: string) => {
    const href = u.replace(/&amp;/g, "&");
    if (!safeHref(href)) return `${t} (${u})`;
    return `<a href="${u}" rel="nofollow noopener noreferrer" target="_blank">${t}</a>`;
  });
  s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  s = s.replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>");
  return s;
}

export function mdToHtml(src: string): string {
  const lines = String(src).split(/\r?\n/);
  const out: string[] = [];
  let inFence = false;
  let fence: string[] = [];
  let inList = false;
  const closeList = () => {
    if (inList) {
      out.push("</ul>");
      inList = false;
    }
  };
  for (const line of lines) {
    if (/^```/.test(line.trim())) {
      closeList();
      if (inFence) {
        out.push(`<pre><code>${escapeHtml(fence.join("\n"))}</code></pre>`);
        fence = [];
        inFence = false;
      } else {
        inFence = true;
      }
      continue;
    }
    if (inFence) {
      fence.push(line);
      continue;
    }
    const h = /^(#{1,4})\s+(.*)$/.exec(line);
    if (h) {
      closeList();
      const level = h[1].length;
      out.push(`<h${level}>${inline(h[2])}</h${level}>`);
      continue;
    }
    const li = /^\s*[-*]\s+(.*)$/.exec(line);
    if (li) {
      if (!inList) {
        out.push("<ul>");
        inList = true;
      }
      out.push(`<li>${inline(li[1])}</li>`);
      continue;
    }
    closeList();
    if (line.trim() === "") {
      out.push("");
      continue;
    }
    out.push(`<p>${inline(line)}</p>`);
  }
  closeList();
  if (inFence && fence.length) {
    out.push(`<pre><code>${escapeHtml(fence.join("\n"))}</code></pre>`);
  }
  return out.join("\n");
}
