// Markdown 预览里的纯文本逻辑：从组件里拆出来，好单测（组件本身要拉
// satteri 的 wasm 与 docx-preview，测试里加载成本高且与断言无关）。

/**
 * 去掉 YAML / TOML frontmatter。
 * 只认文件开头、紧贴首行的定界符 —— 正文里出现的 `---` 分隔线不能被吃掉。
 */
export function stripFrontmatter(src: string): string {
  const m = /^(---|\+\+\+)\r?\n([\s\S]*?)\r?\n\1(\r?\n|$)/.exec(src);
  return m ? src.slice(m[0].length) : src;
}

export interface TocEntry {
  level: number;
  text: string;
}

/** 提取 ATX 标题（# ~ ######），跳过围栏代码块内的 `#` 行。 */
export function extractHeadings(content: string): TocEntry[] {
  const entries: TocEntry[] = [];
  let inFence = false;
  for (const raw of content.split("\n")) {
    const line = raw.trimStart();
    if (/^(```|~~~)/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    const m = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (m) {
      // 行内代码/链接记号只影响展示，剥掉常见包裹符
      const text = m[2].replace(/[*`_]/g, "").trim();
      if (text) entries.push({ level: m[1].length, text });
    }
  }
  return entries;
}
