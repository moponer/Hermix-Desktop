/* 界面缩放的唯一读写入口。
 *
 * 实现方式是根元素 `zoom`（不是 CSS transform，也不是 tauri 的 setZoom），
 * 因为只有它能同时缩放文字、间距、边框与滚动条，且不产生模糊。
 *
 * ⚠ 这条选择的三个后果，改动前先读：
 * 1. `zoom` 不改变 `window.innerWidth` —— 布局里任何基于视口宽度的分档
 *    （App.tsx 的面板最小宽、右列自动收起阈值）都必须先除以当前 zoom，
 *    否则会在"看起来很宽"的窗口里撑破三列布局。
 * 2. `zoom` 不改变视口单位 —— `100vh` 仍是物理视口高度。放大 1.3 后
 *    `height: 100vh` 的元素实际占 130% 视口高，底部被裁掉。外壳用
 *    `height: 100%` 而非 `100vh`，见 components-layout.css 的 .app-shell。
 * 3. 缩放值必须持久化并在启动时恢复，否则重启回到 100%，用户以为设置没保存。
 */
import { storage } from "./storageCompat";

const SCALE_KEY = "hermix.desktop.ui-scale";

/** 缩放值变化时派发，供布局层重算等效视口宽度。 */
export const UI_SCALE_EVENT = "hermix:ui-scale";

export const SCALE_MIN = 0.7;
export const SCALE_MAX = 1.5;

const SCALE_OPTIONS = [0.85, 1, 1.15, 1.3] as const;

export { SCALE_OPTIONS };

function clamp(v: number): number {
  return Math.min(SCALE_MAX, Math.max(SCALE_MIN, v));
}

/** 当前缩放。以 DOM 上的实际值为准（含启动恢复的即时写入），storage 仅作持久层。 */
export function readScale(): number {
  const z = Number(document.documentElement.style.zoom);
  if (z > 0) return z;
  const stored = Number(storage.getItem(SCALE_KEY));
  return stored > 0 ? clamp(stored) : 1;
}

/** 写入缩放：更新 DOM、持久化、广播。三者必须同进同退。 */
export function applyScale(v: number): number {
  const next = clamp(Math.round(v * 100) / 100);
  document.documentElement.style.zoom = String(next);
  try { storage.setItem(SCALE_KEY, String(next)); } catch {}
  window.dispatchEvent(new CustomEvent(UI_SCALE_EVENT, { detail: next }));
  return next;
}

/** 启动时恢复上次的缩放。在 React 挂载前调用，避免先按 100% 布局再跳变。 */
export function restoreScale(): void {
  let stored = 0;
  try { stored = Number(storage.getItem(SCALE_KEY)); } catch { return; }
  if (!(stored > 0) || stored === 1) return;
  document.documentElement.style.zoom = String(clamp(stored));
}
