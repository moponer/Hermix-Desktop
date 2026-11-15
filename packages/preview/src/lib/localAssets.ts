// 本地文档里的相对路径解析。
//
// Markdown / HTML 预览的是磁盘上的文件，正文里的 `![](img/a.png)`、
// `[x](./b.md)` 是相对**文档自身**的。预览页跑在应用源（tauri://）下，
// 浏览器会拿应用根目录去解析，于是满屏裂图 —— 必须先按文档目录补全，
// 再交给已有的产物读取链（那条链已经带产物根目录白名单）。

/** 取目录部分，保留尾部分隔符便于拼接。 */
export function dirnameOf(p: string): string {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i < 0 ? "" : p.slice(0, i + 1);
}

const SCHEME = /^[a-z][a-z0-9+.-]*:/i;

/** 0 基列号 → Excel 列标（A / B / … / Z / AA / …）。
 *  这是个纯字符串变换，不依赖任何引擎；放在 `localAssets` 是因为它跟
 *  路径/列号同为"位置 ↔ 文本"的范畴，导出来 `workbook.ts` 可以静态用，
 *  不会因为它把整个引擎包拽进主 chunk。 */
export function colName(index: number): string {
  let n = index;
  let s = "";
  do {
    s = String.fromCharCode(65 + (n % 26)) + s;
    n = Math.floor(n / 26) - 1;
  } while (n >= 0);
  return s;
}

/** 是不是 http(s)/file 这类交给系统或远程的地址。 */
export function isExternalHref(href: string): boolean {
  return SCHEME.test(href) || href.startsWith("//");
}

/**
 * 把相对路径按 `fromDir` 补成绝对路径；已经是绝对地址的按原样返回。
 * 支持 `../` 回退、`./` 与 URL 百分号编码（文件名里的空格在 Markdown
 * 里必须写成 %20）。
 */
export function resolveRelative(fromDir: string, href: string): string {
  const decoded = (() => {
    try {
      return decodeURIComponent(href);
    } catch {
      return href;
    }
  })();
  if (isExternalHref(decoded)) return decoded;
  if (decoded.startsWith("#") || decoded.startsWith("data:")) return decoded;

  const parts = `${fromDir}${decoded}`.split(/[\\/]/);
  const atDriveRoot = /^[A-Za-z]:$/.test(parts[0] ?? "");
  const out: string[] = [];
  for (const part of parts) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      // 已经到盘根再往上就停住，不要把 `C:` 弹成空串。
      if (out.length && !(atDriveRoot && out.length === 1)) out.pop();
      continue;
    }
    out.push(part);
  }
  const normalized = out.join("\\");
  if (/^[A-Za-z]:/.test(normalized)) return normalized;
  // POSIX 根目录要保留前导斜杠，否则 `/a/b` 会被拼成 Windows 式的 `\a\b`。
  return fromDir.startsWith("/") ? `/${out.join("/")}` : `\\${normalized}`;
}
