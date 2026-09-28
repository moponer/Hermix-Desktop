// pptx-browser 只发布源码（src/*.js），没有类型声明。
// 这里按它 README 的公开 API 声明我们用到的部分。

declare module "pptx-browser" {
  export class PptxRenderer {
    /** 幻灯片总数，load() 之后可用。 */
    slideCount: number;
    load(
      source: File | Blob | ArrayBuffer | Uint8Array,
      onProgress?: (progress: number, message: string) => void,
    ): Promise<void>;
    /** 把第 index 页（0 基）画到 canvas，宽度决定渲染分辨率。 */
    renderSlide(index: number, canvas: HTMLCanvasElement, width?: number): Promise<void>;
    renderAllSlides(width?: number): Promise<HTMLCanvasElement[]>;
    /** 释放渲染期间创建的 blob URL。 */
    destroy(): void;
  }
}
