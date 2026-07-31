// 全局大图查看器事件总线。
// 设计：用一个模块级 EventTarget 解耦「触发打开」与「渲染弹层」，
// 任意组件（用户气泡 / 右侧栏预览 / 工具截图）只要 import
// `openImageViewer` 即可唤起全屏大图，无需逐层透传 props。
// <ImageLightbox /> 在 App 根挂载一次，订阅此总线。

export interface ImageViewItem {
  src: string;
  name?: string;
}

interface OpenDetail {
  images: ImageViewItem[];
  index: number;
}

const BUS = new EventTarget();

/** 打开大图查看器。可传单张或一组（组内用 index 定位 + 上一张/下一张）。 */
export function openImageViewer(images: ImageViewItem | ImageViewItem[], index = 0): void {
  const arr = Array.isArray(images) ? images : [images];
  const safeIndex = arr.length > 0 ? Math.max(0, Math.min(index, arr.length - 1)) : 0;
  BUS.dispatchEvent(
    new CustomEvent<OpenDetail>("open", { detail: { images: arr, index: safeIndex } }),
  );
}

/** 订阅打开事件，返回取消订阅函数。 */
export function onImageViewerOpen(cb: (d: OpenDetail) => void): () => void {
  const handler = (e: Event) => cb((e as CustomEvent<OpenDetail>).detail);
  BUS.addEventListener("open", handler);
  return () => BUS.removeEventListener("open", handler);
}
