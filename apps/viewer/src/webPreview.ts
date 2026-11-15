// 网址判定与标签命名（地址栏是「万能输入框」，这里只管网址那一路）。
//
// 分派规则（App.tsx 的 onPathSubmit 用）：
//   1. 网址（http/https/www）→ 按设置里的「网页内核」走：内嵌视图 or 系统浏览器；
//   2. 绝对路径 → 目录进资源管理器，文件开预览标签；
//   3. 其余 → 当搜索词。

/** 输入看起来像网址吗？ */
export function isUrlInput(text: string): boolean {
  return /^(https?:\/\/|www\.)/i.test(text.trim());
}

/** 补全协议头（`www.x.com` → `https://www.x.com`）。 */
export function normalizeUrlInput(text: string): string {
  const value = text.trim();
  return value.startsWith("www.") ? `https://${value}` : value;
}

/** 标签名：主机 + 路径（去掉尾斜杠），比整条 URL 短。 */
export function urlTabName(url: string): string {
  try {
    const parsed = new URL(url);
    const path = parsed.pathname.replace(/\/+$/, "");
    return `${parsed.host}${path}`;
  } catch {
    return url;
  }
}
