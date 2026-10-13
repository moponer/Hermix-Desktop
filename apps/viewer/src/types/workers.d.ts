// 让 Vite 风格的 `?url` 导入生效（package 内 PdfView 用
// `pdfjs-dist/build/pdf.worker.min.mjs?url` 拿 worker 路径）。
declare module "*?url" {
  const src: string;
  export default src;
}
