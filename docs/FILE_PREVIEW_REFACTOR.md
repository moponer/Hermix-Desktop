# 文件预览重构（2026-09-27）

## 起因

`components/PreviewPanel.tsx` 是 736 行的上帝组件，顶层只有 3 个声明，剩下全是内联逻辑。两个致命结构问题：

1. **渲染分支复制两份** —— 面板一份（原 455-573 行）、全屏一份（原 586-681 行），loading / error / binary / empty / 状态栏全部双份。新增文件类型要改两处，漏一处就表现为"改了没变化"。
2. **全屏那份从来没被执行过** —— `setFullscreen` 全项目没有任何调用入口，等于一半代码是死的。

判定逻辑则是一条 if-else 链上串了五个独立谓词（`isImageExt` / `isPdfExt` / `isMarkdownExt` / `needsFrontendParse` / `kind === "office"`），顺序耦合，加类型要想清楚插在哪。

## 调研：先找现成的，别自己造

| 候选 | 结论 |
|---|---|
| **kkFileView** | 出局。Java + Spring Boot + LibreOffice，需要 JRE 与 Office 套件，与 portable 零安装定位冲突 |
| **@open-file-viewer/core** | 出局。6.6 MB，内部塞了 three(3D) / leaflet(GIS) / hls.js / CAD 等我们用不上的东西 |
| **@files-preview-app/preview-file** | 不直接用。8.6 MB 黑盒组件，自带 toolbar 会接管整个 UI，与项目 UI token / 禁蓝紫铁律冲突 |
| **@iamjariwala/react-doc-viewer** | 同上，且缺 PPTX |

**正确姿势：不引黑盒组件，直接用它们内部的引擎。** 这几个"大而全"库本身就是底层引擎的拼装（`files-preview-app` = `pdfjs-dist` + `docx-preview` + `pptx-browser` + `@panzoom/panzoom` + highlight.js）。

## 选型

| 类型 | 旧实现 | 新引擎 | 许可 / 体积 |
|---|---|---|---|
| PDF | `<embed>` 交给 WebView 内置查看器，无页码、无缩放、无法选中文本 | `pdfjs-dist` 6.3 | Apache-2.0，worker 独立产出 1.27 MB |
| DOCX | officecli 外部进程转 HTML | `docx-preview` 0.4.1 | Apache-2.0，0.93 MB，仅依赖 jszip |
| PPTX | officecli 转 HTML，无分页概念 | `pptx-browser` 4.1.5 | MIT，0.42 MB，零依赖，Canvas 逐页渲染 |
| 图片 | base64 拼串（8 MiB 上限，主线程卡死），无交互 | blob URL + `@panzoom/panzoom` | MIT，0.15 MB，零依赖 |
| XLSX / CSV / XLS | SheetJS（已有） | 保留 | — |
| 代码 | prismjs + refractor（已有） | 保留 | — |
| Markdown | satteri（已有） | 保留 | — |
| doc / ppt（老二进制） | officecli | **保留 officecli 兜底** | 纯前端无解 |

**老二进制格式为什么不一起换掉**：`doc` / `ppt` 是 OLE 复合文档，浏览器侧没有成熟解析器；`.xls` 例外——SheetJS 社区版读得动 BIFF，所以仍走前端。

## 新架构

```
components/preview/
  types.ts          PreviewKind / PreviewPayload / PreviewState 联合类型
  resolveKind.ts    文件名 → 预览种类（纯函数，可单测）
  loaders.ts        每种载荷的加载器，副作用集中在这一层
  PreviewSurface.tsx  唯一渲染入口：payload → JSX，面板与全屏共用
  viewers/
    PdfView.tsx     pdf.js → canvas，翻页 / 缩放
    DocxView.tsx    docx-preview → HTML
    PptxView.tsx    pptx-browser → canvas，翻页
    ImageView.tsx   blob + panzoom，缩放 / 拖拽 / 旋转
    MarkdownView.tsx  satteri（从旧组件抽出）
PreviewPanel.tsx    壳：头部工具栏 + 加载状态机 + 全屏容器
```

三条规则：

- **判定是纯函数** —— `resolveKind()` 不碰 IO，返回值唯一，测试钉住。
- **加载器只管取数据** —— 可以返回与请求种类不同的载荷（排版视图失败退 Markdown），所以调用方以 `payload.kind` 为准。
- **渲染只有一处** —— `PreviewSurface` 是唯一 switch；三个重引擎用 `React.lazy` 按需加载，打开文本文件不为 PDF/DOCX/PPTX 付体积。

## 已知代价

- **pptx-browser 会请求 Google Fonts**（`fonts.js:503`）。库内没有禁用开关，离线时靠 5s 硬超时 + `onerror` 兜底，不会卡死，但首个 PPTX 可能慢几秒。Windows 自带 Calibri / Cambria / Segoe UI 会被识别为系统字体而跳过该请求。
- **pdf.js 的 ArrayBuffer 会被转移所有权**，传参必须给副本，否则后续渲染拿到 detached buffer。
- **pdf.js v6 的 `render()` 吃 `canvas` 而非 `canvasContext`**，`destroy()` 在 loadingTask 上而不是 document proxy 上——只丢 doc 引用会留着 worker。

## 验证

- `tsc --noEmit` 通过
- `resolveKind.test.ts` 5 条通过（分派表 + 视图模式）
- `vite build` 通过，`pdf.worker.min` 独立产出；CSP 已有 `worker-src 'self' blob:`，无需改动
