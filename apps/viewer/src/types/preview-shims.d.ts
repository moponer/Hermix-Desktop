// pptx-browser 不带类型 —— 包内 PptxView 用到的 surface 极少，照着
// 真实 API 写一个最小声明。
declare module "pptx-browser" {
  export class PptxRenderer {
    constructor();
    load(source: Uint8Array | ArrayBuffer | Blob | File, onProgress?: (frac: number, label: string) => void): Promise<void>;
    slideCount: number;
    renderSlide(slideIndex: number, canvas: HTMLCanvasElement, width?: number): Promise<void>;
    destroy(): void;
  }
}
