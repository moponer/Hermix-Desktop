# Decision Note: 查看器接上 markitdown sidecar —— 文本抽取不再靠"读文本"退化

Status: implemented — 查看器的 PDF 文本视图、epub / msg 预览与 Office 文档的 Markdown 抽取都走随桌面端一起分发的 markitdown；`cargo check -p hermix-viewer --all-targets` 通过

## Decision

查看器新增 `document_to_markdown` 命令，调用**随桌面端一起分发**的 `markitdown-x86_64-pc-windows-msvc.exe`（Rust port of microsoft/markitdown），并把前端 `viewerFileSource.readMarkdown` 从"退化成 readText"换成真正调用这个命令。

为什么这件事几乎是零成本：桌面端的 tauri.conf 把 `hermix` / `markitdown` / `hermix-viewer` / `hermix-filesearch` 四个 sidecar 装在**同一个 `binaries/` 目录**里，查看器是其中之一 —— 装版后两者是兄弟文件，接上它**不需要任何打包改动**（不新增依赖、不加 8.5 MB、不动构建脚本）。

支持哪些扩展名按 sidecar 自己的 `--list-plugins` 输出写死（docx / xlsx / xlsm / xls / pptx / pdf / html / epub / msg / csv / tsv / json / xml / zip / 图片 / 音视频……），**不含** doc / ppt / rtf / odt / ods / odp —— sidecar 并不支持它们。

## Alternatives considered

- **在查看器里自带一份 markitdown 副本**：多 8.5 MB、还要改打包与 freshness 门禁；两个 sidecar 已经在同一个目录里了，没必要。
- **用 Rust 库自己解析 Office 文档**：docx-preview / ExcelJS / pptx-browser 都是渲染器不是抽取器；PDF 的文本抽取更没有等价物。为一个已经在场的二进制再写一套解析器是纯重复。
- **给查看器写一个 PDF 文本抽取**（pdf.js 的 getTextContent 已经在 PdfPageEditor 里用着）：技术上可行，但只能覆盖 PDF；markitdown 一次覆盖 PDF + Office + epub + msg，且**与桌面端同源**（同一份二进制、同一种输出格式），两端不会漂移。
- **沿用桌面端那份乐观白名单**（含 doc / rtf / odt / ods / odp / ppt）：sidecar 不支持，用户点下去只会等一个必然失败的报错。查看器这份按真实能力写，桌面端那份应当同样收紧（未在本次改动范围内）。

## Consequences

- 查看器里 PDF 的「文本」视图从"点了直接报错"变成真的能抽出文字；epub / msg 从"完全打不开"变成可预览。
- 旧二进制 Office（.doc / .ppt / .rtf / .odt / .ods / .odp）与 WPS 系列（.wps / .et / .dps）**仍然打不开** —— 现在报的是一句明确的"暂不支持 .doc 的文本抽取"，并给"用系统程序打开"的出口（错误卡里本来就有这个按钮），不再是一句含糊的"二进制文件无法渲染为 Markdown"。
- 转换走 `spawn_blocking`：sidecar 跑一个大 PDF 要几秒，不能占住主线程（那会让整个窗口假死）。
- 输出上限 20 万字符（与桌面端一致）：超长会截断并如实标记 `truncated`，前端提示"仅显示前 N 字符"。
- 查看器多了一个对外部二进制的运行时依赖：markitdown 缺失时命令返回明确错误（而不是静默失败），装版时它与查看器同目录，开发态需要先构建桌面端的 `binaries/`。
