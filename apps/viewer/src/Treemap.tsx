// Treemap（squarified treemap）——目录体积可视化。
//
// 算法用 Bruls/Huizing/van Wijk 2000 年的 *squarified treemap*：把每个
// 目录按体积切成矩形，尽量让每块的**长宽比接近 1**（普通"按面积切"会
// 切出 1:20 的细条，标签放不下、看着也不成比例）。开源实现里
// d3-hierarchy、WinDirStat、WizTree 用的是同一套思路。
//
// 为什么放在这：用户问"哪个目录吃掉了整盘"时，一张图比一列数字快得多；
// 而切分必须自己算——现成库在 viewer 里没有，也不值得为一个视图引依赖。

export interface TreemapNode {
  /** 唯一键（完整路径）。 */
  path: string;
  /** 显示名（目录名）。 */
  name: string;
  /** 体积（字节）。 */
  bytes: number;
}

export interface TreemapRect {
  x: number;
  y: number;
  w: number;
  h: number;
  node: TreemapNode;
}

/** 一行矩形的最差长宽比（越大越"歪"）。 */
function worstRatio(values: number[], rowSum: number, side: number): number {
  if (values.length === 0 || rowSum <= 0 || side <= 0) return Number.POSITIVE_INFINITY;
  const max = Math.max(...values);
  const min = Math.min(...values);
  const s2 = rowSum * rowSum;
  const w2 = side * side;
  return Math.max((w2 * max) / s2, s2 / (w2 * min));
}

/**
 * 把 `nodes` 铺进 `[x, y, w, h]` 这块画布，返回每个节点的矩形。
 *
 * 经典实现：按体积降序，逐行贪心——一行内的短边长度取"剩余区域短边"，
 * 行厚 = 行面积 / 短边，行内各项按比例瓜分长边。
 */
export function squarify(
  nodes: TreemapNode[],
  x: number,
  y: number,
  w: number,
  h: number,
): TreemapRect[] {
  const items = nodes
    .filter((node) => node.bytes > 0)
    .slice()
    .sort((a, b) => b.bytes - a.bytes);
  if (items.length === 0 || w <= 0 || h <= 0) return [];

  const total = items.reduce((sum, node) => sum + node.bytes, 0);
  if (total <= 0) return [];
  const scale = (w * h) / total;

  const rects: TreemapRect[] = [];
  let row: TreemapNode[] = [];
  let rowSum = 0;
  let rx = x;
  let ry = y;
  let rw = w;
  let rh = h;

  const placeRow = (rowToPlace: TreemapNode[], sum: number) => {
    if (rowToPlace.length === 0) return;
    const side = Math.min(rw, rh);
    if (side <= 0) return;
    const thickness = (sum * scale) / side;
    let offset = 0;
    for (const node of rowToPlace) {
      const length = (node.bytes * scale) / thickness;
      if (rw >= rh) {
        rects.push({ x: rx, y: ry + offset, w: thickness, h: length, node });
      } else {
        rects.push({ x: rx + offset, y: ry, w: length, h: thickness, node });
      }
      offset += length;
    }
    if (rw >= rh) {
      rx += thickness;
      rw -= thickness;
    } else {
      ry += thickness;
      rh -= thickness;
    }
  };

  for (const node of items) {
    const candidateSum = rowSum + node.bytes;
    const side = Math.min(rw, rh);
    if (
      row.length === 0 ||
      worstRatio(
        [...row.map((n) => n.bytes), node.bytes],
        candidateSum * scale,
        side,
      ) <= worstRatio(
        row.map((n) => n.bytes),
        rowSum * scale,
        side,
      )
    ) {
      row.push(node);
      rowSum = candidateSum;
    } else {
      placeRow(row, rowSum);
      row = [node];
      rowSum = node.bytes;
    }
  }
  placeRow(row, rowSum);

  return rects;
}

/** 目录名 → 稳定的色相（同一目录每次颜色一致）。 */
function hueOf(path: string): number {
  let hash = 0;
  for (let i = 0; i < path.length; i += 1) {
    hash = (hash * 31 + path.charCodeAt(i)) >>> 0;
  }
  return hash % 360;
}

export interface TreemapProps {
  nodes: TreemapNode[];
  /** 画布宽高比（宽 / 高）。 */
  aspect?: number;
  onSelect?: (node: TreemapNode) => void;
  selectedPath?: string | null;
}

/**
 * Treemap 本体。SVG + viewBox 缩放：容器宽度变化时矩形按比例重排，
 * 不需要测量 DOM 尺寸。
 */
export function Treemap({ nodes, aspect = 2.6, onSelect, selectedPath }: TreemapProps) {
  const W = 1000;
  const H = Math.round(W / aspect);
  const rects = squarify(nodes, 0, 0, W, H);
  const total = nodes.reduce((sum, node) => sum + node.bytes, 0);

  if (nodes.length === 0) {
    return <p className="pane-empty">没有可展示的目录。</p>;
  }

  return (
    <div className="treemap">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="xMidYMid meet"
        role="img"
        aria-label="目录体积分布"
      >
        {rects.map((rect) => {
          const hue = hueOf(rect.node.path);
          const selected = selectedPath === rect.node.path;
          // 太小放不下标签就不放：挤在一起的字比没有字更糟。
          const showLabel = rect.w > 46 && rect.h > 20;
          const showSize = rect.w > 46 && rect.h > 40;
          const percent = total > 0 ? (rect.node.bytes / total) * 100 : 0;
          return (
            <g
              key={rect.node.path}
              className={`treemap-cell${selected ? " is-selected" : ""}`}
              onClick={() => onSelect?.(rect.node)}
              role={onSelect ? "button" : undefined}
              tabIndex={onSelect ? 0 : undefined}
              onKeyDown={(event) => {
                if (onSelect && (event.key === "Enter" || event.key === " ")) {
                  event.preventDefault();
                  onSelect(rect.node);
                }
              }}
            >
              <title>
                {`${rect.node.name}\n${formatBytes(rect.node.bytes)}（${percent.toFixed(1)}%）`}
              </title>
              <rect
                x={rect.x + 1}
                y={rect.y + 1}
                width={Math.max(0, rect.w - 2)}
                height={Math.max(0, rect.h - 2)}
                rx={3}
                fill={`hsl(${hue} 62% 62%)`}
              />
              {showLabel && (
                <text x={rect.x + 7} y={rect.y + 17} className="treemap-label">
                  {rect.node.name}
                </text>
              )}
              {showSize && (
                <text x={rect.x + 7} y={rect.y + 34} className="treemap-size">
                  {formatBytes(rect.node.bytes)}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}
