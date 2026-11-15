// 本轮文件变更汇总卡 —— 轮次尾部的「这一轮动了哪些文件」。
//
// 2026-10-02 升级自原来的 `FileChangeBlock`（一个「文件变更（N个）」+ 文件名
// 列表的静态块）。原版的问题：没有增删行数、不能一眼看出改了什么类型的
// 改动、路径一长就被截断看不出是哪个文件。
//
// 2026-10-04 再改。对照参考界面后调整了两处**信息层级**（样式在
// components-prose.css 的 .fcx 段）：
//
//  1. **标题主语换成「N 个文件已更改」**。此前标题是「新增 1 · 修改 2」这类
//     类型分布 —— 扫这一行时用户最先要的是「几个文件被动过」，类型分布是
//     展开后逐行才需要的细节。原先放在标题行是信息排错了位置。
//  2. **行内顺序换成「文件名 → 目录」**。文件名才是扫列表时真正要找的东西，
//     此前目录排在前面，长路径会把文件名挤出视野。
//
// 两处都保留的既有决定：
//  - 图标说的是「发生了什么」（新增/修改/删除/重命名）而不是「什么类型的
//    文件」—— 后缀已经写在路径里了。
//  - 增删行数用等宽数字，沿用过程行的 success / 灰两档。
//  - 折叠是标准 disclosure（左端旋转 chevron），不靠文字说明能不能点。
//
// ⚠ 增删行数的诚实性：来自 `utils/lineStats`，按工具**入参**估算，不是对
// 磁盘的真 diff（patch 替换多次时真实删除是 N 倍，估算值偏小）。tooltip 里
// 写明了这一点。匹配不到写工具时**不显示这一列**，不显示假的 0。
import { useMemo, useState, type MouseEvent as ReactMouseEvent } from "react";
import { ChevronRight, FilePlus2, FilePen, FileX2, FileDiff } from "lucide-react";
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

  return (
    <div className={`fcx ${className ?? ""}`} data-open={open ? "true" : "false"}>
      <button
        type="button"
        className="fcx-head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        title={open ? "收起文件清单" : "展开本轮改动的文件清单"}
      >
        <ChevronRight className="icon-sm fcx-chev" aria-hidden="true" />
        <span className="fcx-head-text">
          <span className="fcx-title">{n} 个文件已更改</span>
          {total.known && (
            <span className="fcx-stats" title={STATS_TITLE}>
              {total.added > 0 && <span className="fcx-stat fcx-stat--add">+{total.added}</span>}
              {total.removed > 0 && <span className="fcx-stat fcx-stat--del">−{total.removed}</span>}
            </span>
          )}
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
                  title={`${label}：${c.path}`}
                  onClick={(e) => onPathClick?.(c.path, e)}
                >
                  <span className="fcx-kind" data-kind={c.changeType} title={label}>
                    <KindIcon className="icon-sm" aria-hidden="true" />
                    <span className="sr-only">{label}</span>
                  </span>
                  {/* 文件名在前：扫列表时找的是文件名，长目录不该把它顶出视野 */}
                  <span className="fcx-name">{name}</span>
                  {dir && <span className="fcx-dir">{dir}</span>}
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
