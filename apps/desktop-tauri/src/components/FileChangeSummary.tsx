// 本轮文件变更汇总条 —— 轮次尾部的「这一轮动了哪些文件」。
//
// 2026-10-02 升级自原来的 `FileChangeBlock`（一个「文件变更（N个）」+ 文件名
// 列表的静态块）。原版的问题：没有增删行数、不能一眼看出改了什么类型的
// 改动、路径一长就被截断看不出是哪个文件。
//
// 视觉刻意不照抄参考界面。参考那张卡是「圆角描边卡片 + 灰色方块图标 +
// 文件类型图标 + 纯文字链接」，我们这套的取向不同：
//
//  1. **无描边卡片**。项目 2026-10-01 起就在拆「卡片外壳」——边框本身就在
//     宣告「这是内容」。这里用 `--color-bg-tertiary` 铺底 + hover 时才出现
//     的 2px 左侧强调轨，和过程行（`.process-line`）同一套语言。
//  2. **图标说的是「发生了什么」而不是「这是什么类型的文件」**。参考界面
//     那个蓝色 React/TS 图标在 `.css` / `.md` 上就是错的（后缀已经写在路径
//     里了），而「新增 / 修改 / 删除 / 重命名」是用户真正要一眼扫到的信息。
//  3. **计数用等宽小 chip**，颜色沿用过程行已有的 success / error 两档，
//     整个界面的「+N −M」是同一套符号。
//  4. **折叠是标准 disclosure**（旋转的 chevron + 文案），不是裸文字链接 ——
//     点之前能看出这是个可展开的东西。
//
// ⚠ 增删行数的诚实性：来自 `utils/lineStats`，按工具**入参**估算，不是对
// 磁盘的真 diff（patch 替换多次时真实删除是 N 倍，估算值偏小）。tooltip 里
// 写明了这一点。匹配不到写工具时**不显示这一列**，不显示假的 0。
import { useMemo, useState, type MouseEvent as ReactMouseEvent } from "react";
import { ChevronDown, FilePlus2, FilePen, FileX2, FileDiff } from "lucide-react";
import type { FileChangeEntry } from "./messageParser";

/** 变更类型 → 图标 + 文案。图标说的是「发生了什么」。 */
const KIND = {
  created: { icon: FilePlus2, label: "新增" },
  modified: { icon: FilePen, label: "修改" },
  deleted: { icon: FileX2, label: "删除" },
  renamed: { icon: FileDiff, label: "重命名" },
} as const;

/** 增删行数的 tooltip —— 必须说清它不是真 diff。 */
const STATS_TITLE =
  "按工具入参估算的增删行数，不是对磁盘做的真 diff（patch 一次替换多处时真实删除会更大）";

/**
 * 路径拆成「目录 / 文件名」两段：文件名是扫列表时真正要找的东西。
 *
 * 目录段数超限时**只保留最后 2 段**并在前面加省略号。这里刻意不用 CSS 的
 * `direction: rtl` 截断技巧 —— 那招依赖 bidi 规则，路径里混了 `\`、`:`、
 * 数字时渲染结果不稳定（偶尔整段路径左右翻转）。段数截断是确定的：无论
 * 路径多长，文件名一定可见。
 */
function splitPath(p: string, maxSegs = 2): { dir: string; name: string } {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  if (i < 0) return { dir: "", name: p };
  const sep = p[i];
  const dir = p.slice(0, i);
  const name = p.slice(i + 1);
  const segs = dir.split(/[\\/]/).filter(Boolean);
  if (segs.length <= maxSegs) return { dir: dir + sep, name };
  return { dir: `…${sep}${segs.slice(-maxSegs).join(sep)}${sep}`, name };
}

export function FileChangeSummary({
  changes,
  onPathClick,
  className,
  defaultOpen = false,
}: {
  changes: FileChangeEntry[];
  /** 点文件 → 右栏预览抽屉。复用消息列已有的这条路径，不新造一套。 */
  onPathClick?: (path: string, e: ReactMouseEvent) => void;
  className?: string;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const total = useMemo(
    () =>
      changes.reduce(
        (acc, c) => ({
          added: acc.added + (c.added ?? 0),
          removed: acc.removed + (c.removed ?? 0),
          // 「有没有可显示的数」而不是「有没有匹配到写工具」：匹配到了但
          // 增删都是 0（比如 patch 了个空串）时，显示一个空 chip + 一条
          // 悬停提示，比不显示更糟。
          known: acc.known || (c.added ?? 0) > 0 || (c.removed ?? 0) > 0,
        }),
        { added: 0, removed: 0, known: false },
      ),
    [changes],
  );
  if (changes.length === 0) return null;

  const n = changes.length;
  // 按类型分别列，**跳过 0 项**：原先写成「新增 x · 删除 y · 修改 n-x-y」
  // 会把「重命名」悄悄算进「修改」—— 用户扫这一行就是为了知道动了什么，
  // 口径含糊等于没说清。现在四项各归各的，为 0 就不出现。
  const byKind = (k: FileChangeEntry["changeType"]) =>
    changes.filter((c) => c.changeType === k).length;
  const terms: string[] = [];
  if (byKind("created")) terms.push(`新增 ${byKind("created")}`);
  if (byKind("modified")) terms.push(`修改 ${byKind("modified")}`);
  if (byKind("deleted")) terms.push(`删除 ${byKind("deleted")}`);
  if (byKind("renamed")) terms.push(`重命名 ${byKind("renamed")}`);
  const headline = terms.length > 0 ? terms.join(" · ") : `本轮改了 ${n} 个文件`;

  return (
    <div className={`fcx ${className ?? ""}`} data-open={open ? "true" : "false"}>
      <button
        type="button"
        className="fcx-head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        title={open ? "收起文件清单" : "展开本轮改动的文件清单"}
      >
        <span className="fcx-head-text">
          {headline}
          {total.known && (
            <span className="fcx-stats" title={STATS_TITLE}>
              {total.added > 0 && <span className="fcx-stat fcx-stat--add">+{total.added}</span>}
              {total.removed > 0 && <span className="fcx-stat fcx-stat--del">−{total.removed}</span>}
            </span>
          )}
        </span>
        <span className="fcx-toggle">
          {open ? "收起文件" : "查看文件"}
          <ChevronDown className="icon-sm fcx-chev" />
        </span>
      </button>

      {open && (
        <ul className="fcx-list">
          {changes.map((c, i) => {
            const { icon: KindIcon, label } = KIND[c.changeType] ?? KIND.modified;
            const { dir, name } = splitPath(c.path);
            const hasStats = (c.added ?? 0) > 0 || (c.removed ?? 0) > 0;
            return (
              <li key={`${c.path}-${i}`}>
                <button
                  type="button"
                  className="fcx-row"
                  title={c.path}
                  onClick={(e) => onPathClick?.(c.path, e)}
                >
                  <span className="fcx-kind" data-kind={c.changeType} title={label}>
                    <KindIcon className="icon-sm" aria-hidden="true" />
                    <span className="sr-only">{label}</span>
                  </span>
                  <span className="fcx-path">
                    {dir && <span className="fcx-dir">{dir}</span>}
                    <span className="fcx-name">{name}</span>
                  </span>
                  {hasStats && (
                    <span className="fcx-stats" title={STATS_TITLE}>
                      {c.added != null && c.added > 0 && (
                        <span className="fcx-stat fcx-stat--add">+{c.added}</span>
                      )}
                      {c.removed != null && c.removed > 0 && (
                        <span className="fcx-stat fcx-stat--del">−{c.removed}</span>
                      )}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
