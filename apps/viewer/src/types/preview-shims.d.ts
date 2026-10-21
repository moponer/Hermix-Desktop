// pptx-browser 不带类型 —— 包内 PptxView 用到的 surface 极少，照着
// 真实 API 写一个最小声明。
//
// PptxWriter 也在这里声明：它是这个包**已经带**的写回器（改形状文字、加
// 文本框、加图、复制/删除/重排页、改页背景、改主题色、写备注、save），
// 之前只声明了渲染面，于是"能改 PPT"这件事在类型层面就不存在。
declare module "pptx-browser" {
  export class PptxRenderer {
    constructor();
    load(source: Uint8Array | ArrayBuffer | Blob | File, onProgress?: (frac: number, label: string) => void): Promise<void>;
    slideCount: number;
    renderSlide(slideIndex: number, canvas: HTMLCanvasElement, width?: number): Promise<void>;
    destroy(): void;
  }

  /** PPTX 写回器：直接改 OOXML 文件本体。 */
  export class PptxWriter {
    constructor(files: unknown);
    static fromBytes(buffer: Uint8Array | ArrayBuffer): Promise<PptxWriter>;
    static fromRenderer(renderer: PptxRenderer): PptxWriter;
    getSlideCount(): number;
    listShapes(slideIdx: number): Array<{ id: string; name: string; type: string }>;
    getShapeText(slideIdx: number, shapeName: string): string;
    setShapeText(slideIdx: number, shapeName: string, text: string, opts?: { preserveFormatting?: boolean }): PptxWriter;
    addTextBox(
      slideIdx: number,
      text: string,
      style?: {
        x?: number;
        y?: number;
        w?: number;
        h?: number;
        color?: string;
        fontSize?: number;
        bold?: boolean;
        align?: "l" | "ctr" | "r";
        fontFamily?: string;
      },
    ): PptxWriter;
    addImage(
      slideIdx: number,
      imageBytes: Uint8Array,
      mimeType?: string,
      rect?: { x?: number; y?: number; w?: number; h?: number },
    ): Promise<PptxWriter>;
    setShapeImage(
      slideIdx: number,
      shapeName: string,
      imageBytes: Uint8Array,
      mimeType?: string,
    ): Promise<PptxWriter>;
    setSlideBackground(slideIdx: number, hexRgb: string): PptxWriter;
    setThemeColor(key: string, hexRgb: string): PptxWriter;
    duplicateSlide(fromIdx: number, toIdx: number): PptxWriter;
    removeSlide(slideIdx: number): PptxWriter;
    reorderSlides(newOrder: number[]): PptxWriter;
    setSlideNotes(slideIdx: number, text: string): PptxWriter;
    save(): Promise<Uint8Array>;
    download(filename?: string): Promise<void>;
  }
}
