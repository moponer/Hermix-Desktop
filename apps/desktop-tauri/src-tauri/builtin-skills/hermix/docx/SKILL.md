---

name: docx
version: 1
description: >
  Author real Word (.docx) deliverables with real typography — not an HTML page
  renamed to .docx. Use when the user wants a Word document, contract, report,
  resume, 公文/方案/说明书, or asks to "export as Word", "genereate docx", "写成
  Word 文档". Covers three routes (HTML→docx quick path, structured blocks, full
  python-docx authoring), environment checks, and a visual verification loop via
  the bundled conversion engine. Also use when a previous docx attempt opened
  with broken layout, missing Chinese fonts, or tables spilling off the page.
  Do NOT use for plain text/markdown deliverables or for PDF-only requests
  (those go through file_converter).
license: MIT
tags:
  - docx
  - word
  - documents
  - typography
  - office
---

# Skill: DOCX Authoring

A .docx is a ZIP of OOXML. Three routes exist in this app; **pick the cheapest
that meets the quality bar**, and never claim "Word file" for an HTML dump.

## Route selection (read this first)

| Situation | Route | Tool |
|---|---|---|
| Content already exists as HTML (you wrote it, or scraped clean HTML), simple structure: headings, paragraphs, lists, a table | **Quick path** | `html_to_docx` (纯 Rust，零依赖) |
| You have structured blocks (heading1-3/paragraph/bullet) and want native OOXML without Python | **Structured path** | `office_export` (format=`docx`) |
| Needs real typography: cover page, page numbers, TOC, headers/footers, styles, Chinese font control, precise column widths | **Full path** | python-docx script (below) |
| Office→PDF / DOCX→PNG preview | **Convert/render** | `file_converter`（内置 dsh 引擎，离线） |

Rule of thumb: if the user will print it or show it to a boss, use the full
path. If it is a data handoff, use quick/structured.

## Environment check (full path only)

```bat
python --version
python -c "import docx; print('python-docx', docx.__version__)"
```

Missing `python-docx`: `python -m pip install python-docx -i https://pypi.tuna.tsinghua.edu.cn/simple`
(中国网络优先清华镜像). If Python itself is missing, **tell the user** and fall
back to the quick/structured path — do not silently degrade.

## Full path: authoring script

**Use the bundled generator first** — it ships with the app at
`<HERMIX_HOME>/skills/hermix/docx/scripts/build_docx.py` and already encodes the
hard rules below (中文字体双设置、A4 页边距、首行缩进、定宽表格、页码域):

```bat
python "%HERMIX_HOME%\skills\hermix\docx\scripts\build_docx.py" --check
python "%HERMIX_HOME%\skills\hermix\docx\scripts\build_docx.py" spec.json out.docx
```

`spec.json` 的字段：`title` / `subtitle` / `page` / `fonts` / `blocks[]`
(`heading`|`para`|`bullet`|`numbered`|`page_break`|`table`|`image`) /
`footer_text` / `page_numbers`。写 spec 比写脚本快，也不会漏掉字体规则。

只有当文档需要生成器表达不了的结构（分栏、图文环绕、复杂页眉）时，才在
工作区写一次性脚本。参考骨架（生成器已内化这些规则，此处仅作扩展起点）：

```python
from docx import Document
from docx.shared import Pt, Cm, RGBColor
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.enum.table import WD_TABLE_ALIGNMENT
from docx.oxml.ns import qn

doc = Document()

# --- baseline styles: Chinese fonts must be set on BOTH ascii and eastAsia,
# otherwise Word falls back to a font the reader may not have. ---
style = doc.styles["Normal"]
style.font.name = "Times New Roman"
style.font.size = Pt(10.5)
style.element.rPr.rFonts.set(qn("w:eastAsia"), "宋体")

for name, size in (("Heading 1", 16), ("Heading 2", 14), ("Heading 3", 12)):
    h = doc.styles[name]
    h.font.name = "Times New Roman"
    h.font.size = Pt(size)
    h.font.bold = True
    h.element.rPr.rFonts.set(qn("w:eastAsia"), "黑体")

# --- page setup: A4 with sane margins ---
sec = doc.sections[0]
sec.page_width, sec.page_height = Cm(21.0), Cm(29.7)
sec.left_margin = sec.right_margin = Cm(2.54)
sec.top_margin = sec.bottom_margin = Cm(2.54)

doc.add_heading("文档标题", level=1)
p = doc.add_paragraph("正文段落。")
p.paragraph_format.first_line_indent = Pt(21)   # 中文首行缩进两字符
p.alignment = WD_ALIGN_PARAGRAPH.JUSTIFY

# --- table: fixed widths + header row so it cannot spill ---
table = doc.add_table(rows=1, cols=3)
table.style = "Table Grid"
table.alignment = WD_TABLE_ALIGNMENT.CENTER
for cell, text in zip(table.rows[0].cells, ("列A", "列B", "列C")):
    cell.text = text

doc.save("out.docx")
```

Hard-won rules (each of these has broken a real delivery):

1. **Fonts**: set `w:eastAsia` for every Chinese-used style; body 宋体/正文,
   headings 黑体. Never rely on the default.
2. **No manual spacing hacks** (empty paragraphs to push a page). Use
   `add_page_break()`.
3. **Page numbers/TOC** are fields: insert them, then tell the user to press
   Ctrl+A → F9 (or "更新域") in Word once. Do not promise a pre-rendered TOC
   unless the field is updated.
4. **Tables**: fixed column widths in cm, header row bold; verify nothing
   exceeds the text width (page width − margins).
5. **Images**: `add_picture(..., width=Cm(15))` at most; never exceed text
   width.
6. **File naming**: `<topic>_<YYYYMMDD>.docx`; never overwrite the user's file
   without a `_backup` copy first.

## Verification loop (mandatory before saying "done")

1. **Structural**: reopen with python-docx and assert what you promised:
   paragraph count > 0, expected headings exist, tables have the right shape.
2. **Visual**: render page 1 and one inner page to PNG with the conversion
   engine (`file_converter` pdf→png; no LibreOffice install needed). The engine
   is fetched on first use (~71MB, one-off) and is offline afterwards — if a
   conversion is slow the first time, that is the download, not a hang:
   ```bat
   :: via the app: ask the agent to run file_converter pdf→png after docx→pdf
   ```
   Then `read_image` the PNG and actually look at it: margins, table borders,
   Chinese glyphs (tofu boxes = missing font), title alignment.
3. Report: file path, page count, and the one thing you could NOT verify
   (e.g. "TOC field needs F9 in Word").

## Anti-patterns

- Generating HTML and renaming it `.docx` — the user's Word will refuse it.
- Building tables by tab-separated text.
- Ignoring Chinese font fallback, then blaming Word.
- Shipping without rendering: layout bugs (overflow, orphan headings) are
  invisible in code review and obvious in a PNG.
- Reaching for python-docx for a 5-paragraph document that `html_to_docx`
  already handles.
