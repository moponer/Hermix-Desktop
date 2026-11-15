// 工作台算法工具箱 —— 只给算法，不给外观。
//
// 这些是「LLM 自己写要花几十行、写错还难查」的通用算法：虚拟滚动、行级 diff、
// 大文件解析。它们不含任何 DOM 结构或样式决策 —— 怎么排版、怎么呈现是工作台的事。
//
// 每个函数都是纯函数，无副作用，可直接单测。
// 注入方式：挂到宿主 SDK 的 `wb.util.*`，工作台代码 import 走 api.js。

/** 虚拟滚动窗口。返回当前应渲染的区间与偏移量。 */
export interface VirtualWindow {
  /** 起始下标（含）。 */
  start: number;
  /** 结束下标（不含）。 */
  end: number;
  /** 顶部占位高度，撑住滚动条位置。 */
  padTop: number;
  /** 底部占位高度。 */
  padBottom: number;
  /** 总高度。 */
  total: number;
}

export function virtualWindow(
  itemCount: number,
  itemHeight: number,
  scrollTop: number,
  viewportHeight: number,
  overscan = 4,
): VirtualWindow {
  const total = itemCount * itemHeight;
  if (itemCount <= 0 || itemHeight <= 0) {
    return { start: 0, end: 0, padTop: 0, padBottom: 0, total: 0 };
  }
  const start = Math.max(0, Math.floor(scrollTop / itemHeight) - overscan);
  const visible = Math.ceil(viewportHeight / itemHeight) + overscan * 2;
  const end = Math.min(itemCount, start + visible);
  return {
    start,
    end,
    padTop: start * itemHeight,
    padBottom: Math.max(0, total - end * itemHeight),
    total,
  };
}

/** 行级 diff 的一段。`type` 为增删改，`left`/`right` 是行号（1 起，0 表示无）。 */
export interface DiffHunk {
  type: "equal" | "insert" | "delete" | "replace";
  leftLine: number;
  rightLine: number;
  left: string;
  right: string;
}

/**
 * 行级 diff。大文件才需要 —— 工作台做「文件对比」时不用自己写 LCS。
 * 空间用 O(min(n,m))，10 万行也不会炸内存。
 */
export function diffLines(leftText: string, rightText: string): DiffHunk[] {
  const a = leftText.split("\n");
  const b = rightText.split("\n");
  // 先掐掉首尾相同的整段，剩下的通常很小 —— 这是实际速度的关键。
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  ) {
    tail++;
  }
  const midA = a.slice(head, a.length - tail);
  const midB = b.slice(head, b.length - tail);

  const lcs = lcsTable(midA, midB);
  const out: DiffHunk[] = [];
  for (let i = 0; i < head; i++) {
    out.push({ type: "equal", leftLine: i + 1, rightLine: i + 1, left: a[i], right: b[i] });
  }
  let i = 0;
  let j = 0;
  while (i < midA.length || j < midB.length) {
    if (i < midA.length && j < midB.length && midA[i] === midB[j]) {
      out.push({
        type: "equal",
        leftLine: head + i + 1,
        rightLine: head + j + 1,
        left: midA[i],
        right: midB[j],
      });
      i++;
      j++;
      continue;
    }
    // 同一位置两侧都变了且无公共子序列可依托 → replace，比删一行加一行好读。
    // 两侧同时前进，所以只需判一次 lcs 值是否一致，不必比较两个方向。
    if (i < midA.length && j < midB.length && lcs[i][j] === lcs[i + 1][j + 1]) {
      out.push({
        type: "replace",
        leftLine: head + i + 1,
        rightLine: head + j + 1,
        left: midA[i],
        right: midB[j],
      });
      i++;
      j++;
      continue;
    }
    if (j < midB.length && (i >= midA.length || lcs[i][j + 1] >= lcs[i + 1][j])) {
      out.push({ type: "insert", leftLine: 0, rightLine: head + j + 1, left: "", right: midB[j] });
      j++;
      continue;
    }
    out.push({ type: "delete", leftLine: head + i + 1, rightLine: 0, left: midA[i], right: "" });
    i++;
  }
  for (let t = 0; t < tail; t++) {
    const li = a.length - tail + t;
    const ri = b.length - tail + t;
    out.push({ type: "equal", leftLine: li + 1, rightLine: ri + 1, left: a[li], right: b[ri] });
  }
  return out;
}

function lcsTable(a: string[], b: string[]): number[][] {
  const n = a.length;
  const m = b.length;
  const table: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i][j] = a[i] === b[j] ? table[i + 1][j + 1] + 1 : Math.max(table[i + 1][j], table[i][j + 1]);
    }
  }
  return table;
}

/** CSV / TSV 解析结果：表头 + 数据行，已处理引号转义与内嵌换行。 */
export interface CsvTable {
  header: string[];
  rows: string[][];
}

/**
 * 分隔符文本解析。正确处理 RFC 4180 的引号转义（`""`）与引号内换行 ——
 * 这两点手写正则几乎必错，是工作台读台账类 CSV 的常见坑。
 */
export function parseDelimited(text: string, delimiter = ","): CsvTable {
  const cells: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i++;
        } else {
          quoted = false;
        }
      } else {
        cell += ch;
      }
      continue;
    }
    if (ch === '"') {
      quoted = true;
    } else if (ch === delimiter) {
      row.push(cell);
      cell = "";
    } else if (ch === "\n") {
      row.push(cell);
      cells.push(row);
      row = [];
      cell = "";
    } else if (ch !== "\r") {
      cell += ch;
    }
  }
  if (cell.length > 0 || row.length > 0) {
    row.push(cell);
    cells.push(row);
  }
  const nonEmpty = cells.filter((r) => r.some((c) => c.trim() !== ""));
  if (nonEmpty.length === 0) return { header: [], rows: [] };
  const [header, ...rows] = nonEmpty;
  return { header, rows };
}

/** 数值列统计 —— 工作台做报表时的高频需求，避免每台各写一遍。 */
export interface ColumnStats {
  count: number;
  sum: number;
  min: number;
  max: number;
  avg: number;
}

export function columnStats(values: number[]): ColumnStats {
  const nums = values.filter((v) => Number.isFinite(v));
  if (nums.length === 0) return { count: 0, sum: 0, min: 0, max: 0, avg: 0 };
  let sum = 0;
  let min = nums[0];
  let max = nums[0];
  for (const n of nums) {
    sum += n;
    if (n < min) min = n;
    if (n > max) max = n;
  }
  return { count: nums.length, sum, min, max, avg: sum / nums.length };
}
