// pdf.js 的**标准 14 号字度量**（Liberation Sans + Foxit 系列）在哪里。
//
// ## 为什么需要它
//
// ① **字宽**：pdf.js 算文本项宽度要用字体的 advance width。标准 14 号字
//    （Helvetica / Times / Courier）通常**不嵌字体程序**，宽度只能从度量表
//    取 —— 没有度量，`getTextContent()` 给的 `item.width` 会是 0，
//    文本层就点不中（"改字"模式直接失效）。
//
// ② **渲染**：不提供时标准字体会走回退字体，宽度与外观都对不上。
//
// ## 为什么是"固定路径"而不是打包资源
//
// pdf.js 是**按标准文件名**去请求这些度量文件的（`LiberationSans-Regular.ttf`
// 这种名字），所以它们必须以**稳定名字**被服务 —— 走 Vite 的 `?url` 会被
// 加上 hash（`LiberationSans-Regular-cbskN6YR.ttf`），pdf.js 就 404 了。
//
// 因此度量文件放在**两端应用的 `public/standard_fonts/`**（Vite 原样拷贝，
// 不加 hash），运行时用绝对路径 `/standard_fonts/`。
//
// 许可：Liberation 是 SIL OFL 1.1，Foxit 是其许可（见同目录 LICENSE_*）。
// 文件来源：pdfjs-dist/standard_fonts/（随 pdfjs-dist 版本升级同步）。

/** pdf.js 的 `standardFontDataUrl` 参数：标准字体度量的目录 URL。 */
export const standardFontDataUrl = "/standard_fonts/";
