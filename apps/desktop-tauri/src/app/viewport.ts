// B6-2（docs/notes/proposed/architecture/2026-11-16-optimization-plan.md H7）：视口缩放计算——从 App.tsx 提取的
// 纯函数（可独立测试）。zoom 由 documentElement.style.zoom 驱动（桌面端
// 自适应缩放），min-width 断点决定主栏/左栏的最小宽度。

/* 自适应 MAIN_MIN_WIDTH — 按**等效视口**动态缩小主列最小宽，避免窄窗口下
   左列 + 主列合计最小宽度超过视口：
   - 等效视口 ≥ 1400px: MAIN_MIN_WIDTH = 580（舒适宽）
   - 等效视口 1000~1399px: MAIN_MIN_WIDTH = 480（中等）
   - 等效视口 800~999px: MAIN_MIN_WIDTH = 400（紧凑）
   - 等效视口 < 800px: MAIN_MIN_WIDTH = 360（最小可读）
   同时 LEFT_MIN 也随视口降低。对应的三列共存阈值随之降到 920。

   注意「等效」二字：zoom 会改变真实可用宽度但不改变 window.innerWidth，
   所以分档前必须除以 zoom，见 currentZoom()。 */
export function currentZoom(): number {
  const z = Number(document.documentElement.style.zoom);
  return z > 0 ? z : 1;
}

export function computeMainMinWidth(vw: number): number {
  const eff = vw / currentZoom();
  if (eff >= 1400) return 580;
  if (eff >= 1000) return 480;
  if (eff >= 800) return 400;
  return 360;
}

export function computeLeftMin(vw: number): number {
  const eff = vw / currentZoom();
  if (eff >= 1200) return 200;
  if (eff >= 900) return 160;
  return 120;
}
