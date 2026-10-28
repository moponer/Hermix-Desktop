/**
 * 会话标题清洗。
 *
 * 会话标题来自「首条用户消息」的直接切片（App.handleSendMessage），而首条
 * 消息有时本身是 LLM 产物 —— 自动化/值守流程里就出现过
 * `["<think>已识别为超星课程自动学习任务；按登录、选课、…"]` 这种推理原文
 * 带 JSON 外壳的文本被原样存进 sessions.title，删除确认卡又原样印出来的
 * 事（2026-10-28 截图实报）。
 *
 * 规则（按序尝试，取第一个"还有可读文字"的结果）：
 *  1. 删完整 `<think>…</think>` 块；未闭合的 `<think>` 连同其后内容一起删
 *     （推理没写完，后面全是推理）。
 *  2. 只剥 think 标签本身、保留文字 —— 首选路径可能把截断标题剥成空壳
 *     （`["<think>…"]` 只剩 `["`），此时推理文字是唯一可读内容，聊胜于无。
 *  3. 剥 JSON 数组/引号外壳：合法 JSON 取第一个非空字符串成员；不是合法
 *     JSON（截断产物）就剥外壳括号与残缺引号。
 * 剥完只剩符号/空白（没有任何文字）才返回空串，由调用方决定兜底标题。
 */
export function cleanSessionTitle(raw: string | undefined | null): string {
  if (!raw) return "";
  const base = raw.trim();
  if (!base) return "";

  const candidates = [
    // ① 整块删除：闭合块删掉；未闭合块连内容一起删。
    base.replace(/<think>[\s\S]*?<\/think>/gi, " ").replace(/<think>[\s\S]*$/i, " "),
    // ② 只剥标签：为 ① 把文本剥空的情况兜底。
    base.replace(/<\/?think>/gi, " "),
  ];

  for (const candidate of candidates) {
    const text = unwrapJsonish(candidate).replace(/\s+/g, " ").trim();
    if (hasReadableText(text)) return text;
  }
  return "";
}

const QUOTE_PAIRS: Record<string, string> = {
  '"': '"',
  "'": "'",
  "“": "”",
  "‘": "’",
  "”": "“",
  "’": "‘",
};

function isQuotePair(open: string, close: string): boolean {
  return QUOTE_PAIRS[open] === close || open === close;
}

/**
 * 剥 JSON 数组 / 包裹引号外壳。
 *
 * 数组外壳只在「前导 `[` 后紧跟引号」时才认定 —— `["…` 是外壳，
 * `[重要] 会议纪要` 这种以方括号开头的正常标题不是。
 * 被截断的数组（缺收尾 `]`/引号，或尾部挂着 `..."` 残迹）也按外壳处理：
 * 开引号剥了，收尾引号/省略号作为截断残迹一并剥。
 */
function unwrapJsonish(text: string): string {
  let t = text.trim();

  // 合法 JSON 数组：直接取第一个非空字符串成员。
  if (t.startsWith("[") && t.endsWith("]")) {
    try {
      const parsed: unknown = JSON.parse(t);
      if (Array.isArray(parsed)) {
        const first = parsed.find((v) => typeof v === "string" && v.trim());
        if (typeof first === "string") return first.trim();
      }
    } catch {
      /* 不是合法 JSON（常见于被截断的数组）—— 走下面的外壳剥离。 */
    }
  }

  let openedQuote = false;
  const shell = t.match(/^\[\s*(["'“”‘’])/);
  if (shell) {
    openedQuote = true;
    t = t.slice(1).trim(); // 去 [
    if (t.endsWith("]")) t = t.slice(0, -1).trim();
  }

  const paired = t.match(/^(["'“”‘’])([\s\S]*)(["'“”‘’])$/);
  if (paired && isQuotePair(paired[1], paired[3])) {
    return paired[2].trim();
  }

  if (openedQuote) {
    // 开引号已确认是外壳，收尾的孤立引号（可夹省略号）是截断残迹。
    t = t.replace(/^["'“”‘’]/, "").trim();
    t = t.replace(/(\.\.\.|…)?\s*["'“”‘’]\s*(\.\.\.|…)?\s*$/, "").trim();
  }
  return t;
}

/** 剩下的字符串里有没有真文字（字母/数字/CJK），没有就只是符号壳。 */
function hasReadableText(text: string): boolean {
  return /[\p{L}\p{N}]/u.test(text);
}
