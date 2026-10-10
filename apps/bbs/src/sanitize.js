//! 白名单消毒：Markdown → 安全 HTML。
//!
//! 策略（决策笔记 2026-10-09-agent-bbs-integration「内容消毒」条目）：
//! 先把整篇输入 **HTML 转义**，再在已转义文本上应用极小的 Markdown 子集——
//! 产出的每个标签都由本模块生成（输出即白名单），用户输入里的任何
//! `<tag>` 都已变成 `&lt;tag&gt;` 字面量，注入面为零。
//! 子集当前：行内代码 / 链接（仅 http(s)）/ 粗体 / 斜体 / ``` 围栏代码块。
//! 不够用再按需扩（六步阶梯：先最小实现）。

'use strict';

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ESC[c]);
}

// 仅 http(s) 可成为 <a href>；javascript: 等一律退化为纯文本。
function safeHref(raw) {
  return /^(https?:\/\/)/i.test(raw);
}

function inline(rawLine) {
  // 1) 整行转义。
  let s = escapeHtml(rawLine);
  // 2) 行内代码 `code`（反引号不被 escapeHtml 改写）。
  s = s.replace(/`([^`]+)`/g, '<code>$1</code>');
  // 3) 链接 [text](url)。url 取自转义文本（属性安全）；协议用反解后的
  //    原值校验（&amp; 反解不影响协议头判断）。
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, t, u) => {
    const href = u.replace(/&amp;/g, '&');
    if (!safeHref(href)) return `${t} (${u})`;
    return `<a href="${u}" rel="nofollow noopener noreferrer" target="_blank">${t}</a>`;
  });
  // 4) 粗体 / 斜体（先粗后斜，避免 ** 被 * 规则吃掉一半）。
  s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  s = s.replace(/(^|[^*])\*([^*\n]+)\*/g, '$1<em>$2</em>');
  return s;
}

function mdToSafeHtml(src) {
  const lines = String(src).split(/\r?\n/);
  const out = [];
  let inFence = false;
  let fenceBuf = [];
  for (const line of lines) {
    if (/^```/.test(line.trim())) {
      if (inFence) {
        out.push('<pre><code>' + escapeHtml(fenceBuf.join('\n')) + '</code></pre>');
        fenceBuf = [];
        inFence = false;
      } else {
        inFence = true;
      }
      continue;
    }
    if (inFence) {
      fenceBuf.push(line);
      continue;
    }
    out.push(inline(line));
  }
  // 未闭合围栏：剩余内容照常出代码块（不吞掉）。
  if (inFence && fenceBuf.length) {
    out.push('<pre><code>' + escapeHtml(fenceBuf.join('\n')) + '</code></pre>');
  }
  return out.join('\n');
}

module.exports = { escapeHtml, mdToSafeHtml };
