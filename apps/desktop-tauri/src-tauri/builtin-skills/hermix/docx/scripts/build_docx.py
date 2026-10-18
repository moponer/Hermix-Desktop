#!/usr/bin/env python3
"""Generate a .docx from a JSON spec, with the hard-won layout defaults baked in.

Why a spec instead of free-form python: the failure modes in Word documents are
almost never logical — they are 中文字体回退, 表格溢出, 手工空行分页. Encoding
those once here means the agent only has to describe the *content*.

Usage:
    python build_docx.py spec.json out.docx
    python build_docx.py --check          # verify python-docx is available

Spec shape (all keys optional except `blocks`):
    {
      "title": "文档标题",            # rendered as Heading 1
      "subtitle": "副标题",            # centered, small
      "page": {"width_cm": 21.0, "height_cm": 29.7, "margin_cm": 2.54},
      "fonts": {"body": "宋体", "heading": "黑体"},
      "blocks": [
        {"type": "heading", "level": 1, "text": "一级标题"},
        {"type": "para", "text": "正文段落", "indent": true},
        {"type": "bullet", "text": "列表项"},
        {"type": "numbered", "text": "编号项"},
        {"type": "page_break"},
        {"type": "table", "header": ["列A", "列B"], "rows": [["1", "2"]],
         "widths_cm": [8.0, 8.0]},
        {"type": "image", "path": "chart.png", "width_cm": 14.0}
      ],
      "footer_text": "内部资料",
      "page_numbers": true
    }

Exit codes: 0 ok, 2 bad spec, 3 python-docx missing, 4 write failure.
"""

import json
import sys


def die(code: int, msg: str) -> None:
    print(f"build_docx: {msg}", file=sys.stderr)
    sys.exit(code)


def main() -> None:
    if "--check" in sys.argv:
        try:
            import docx  # noqa: F401
        except ImportError:
            die(3, "python-docx is not installed (pip install python-docx)")
        print("python-docx OK")
        return

    if len(sys.argv) != 3:
        die(2, "usage: build_docx.py spec.json out.docx | build_docx.py --check")

    spec_path, out_path = sys.argv[1], sys.argv[2]
    try:
        with open(spec_path, "r", encoding="utf-8") as fh:
            spec = json.load(fh)
    except (OSError, json.JSONDecodeError) as exc:
        die(2, f"cannot read spec: {exc}")

    try:
        from docx import Document
        from docx.enum.text import WD_ALIGN_PARAGRAPH
        from docx.enum.table import WD_TABLE_ALIGNMENT
        from docx.oxml.ns import qn
        from docx.shared import Cm, Pt
    except ImportError:
        die(3, "python-docx is not installed (pip install python-docx)")

    blocks = spec.get("blocks")
    if not isinstance(blocks, list) or not blocks:
        die(2, "spec.blocks must be a non-empty array")

    body_font = spec.get("fonts", {}).get("body", "宋体")
    head_font = spec.get("fonts", {}).get("heading", "黑体")

    doc = Document()

    def set_fonts(style, latin: str, east_asia: str, size: float, bold=None):
        style.font.name = latin
        style.font.size = Pt(size)
        if bold is not None:
            style.font.bold = bold
        rpr = style.element.get_or_add_rPr()
        fonts = rpr.find(qn("w:rFonts"))
        if fonts is None:
            fonts = rpr.makeelement(qn("w:rFonts"), {})
            rpr.append(fonts)
        # Both attributes are required: Word uses `w:eastAsia` for CJK runs and
        # silently falls back to a font the reader may not have otherwise.
        fonts.set(qn("w:eastAsia"), east_asia)
        fonts.set(qn("w:ascii"), latin)
        fonts.set(qn("w:hAnsi"), latin)

    set_fonts(doc.styles["Normal"], "Times New Roman", body_font, 10.5)
    for name, size in (("Heading 1", 16), ("Heading 2", 14), ("Heading 3", 12)):
        set_fonts(doc.styles[name], "Times New Roman", head_font, size, bold=True)

    page = spec.get("page", {})
    sec = doc.sections[0]
    sec.page_width = Cm(page.get("width_cm", 21.0))
    sec.page_height = Cm(page.get("height_cm", 29.7))
    margin = Cm(page.get("margin_cm", 2.54))
    sec.left_margin = sec.right_margin = sec.top_margin = sec.bottom_margin = margin
    text_width_cm = page.get("width_cm", 21.0) - 2 * page.get("margin_cm", 2.54)

    if spec.get("title"):
        doc.add_heading(str(spec["title"]), level=1)
    if spec.get("subtitle"):
        p = doc.add_paragraph(str(spec["subtitle"]))
        p.alignment = WD_ALIGN_PARAGRAPH.CENTER

    numbering = {"bullet": "List Bullet", "numbered": "List Number"}

    for i, block in enumerate(blocks):
        kind = block.get("type", "para")
        if kind == "heading":
            level = int(block.get("level", 1))
            doc.add_heading(str(block.get("text", "")), level=max(1, min(level, 4)))
        elif kind in ("para", "bullet", "numbered"):
            text = str(block.get("text", ""))
            if kind == "para":
                p = doc.add_paragraph(text)
                p.alignment = WD_ALIGN_PARAGRAPH.JUSTIFY
                if block.get("indent", True):
                    p.paragraph_format.first_line_indent = Pt(21)  # 中文两字符
            else:
                try:
                    doc.add_paragraph(text, style=numbering[kind])
                except KeyError:
                    doc.add_paragraph(f"• {text}")
        elif kind == "page_break":
            doc.add_page_break()
        elif kind == "table":
            header = block.get("header", [])
            rows = block.get("rows", [])
            cols = max(len(header), max((len(r) for r in rows), default=0))
            if cols == 0:
                die(2, f"blocks[{i}]: table without columns")
            table = doc.add_table(rows=1 if header else 0, cols=cols)
            table.style = "Table Grid"
            table.alignment = WD_TABLE_ALIGNMENT.CENTER
            if header:
                for cell, text in zip(table.rows[0].cells, header):
                    cell.text = str(text)
                    for run in cell.paragraphs[0].runs:
                        run.bold = True
            for row in rows:
                cells = table.add_row().cells
                for cell, text in zip(cells, row):
                    cell.text = str(text)
            widths = block.get("widths_cm") or [text_width_cm / cols] * cols
            for col, w in zip(table.columns, widths):
                for cell in col.cells:
                    cell.width = Cm(float(w))
        elif kind == "image":
            path = block.get("path")
            if not path:
                die(2, f"blocks[{i}]: image without path")
            width = min(float(block.get("width_cm", 14.0)), text_width_cm)
            try:
                doc.add_picture(str(path), width=Cm(width))
            except Exception as exc:  # noqa: BLE001 — missing file / bad format
                die(2, f"blocks[{i}]: cannot embed image {path}: {exc}")
        else:
            die(2, f"blocks[{i}]: unknown type {kind!r}")

    if spec.get("page_numbers"):
        para = sec.footer.paragraphs[0]
        para.alignment = WD_ALIGN_PARAGRAPH.CENTER
        run = para.add_run()
        fld = run._r.makeelement(qn("w:fldSimple"), {qn("w:instr"): "PAGE"})
        run._r.addnext(fld)
    if spec.get("footer_text"):
        sec.footer.paragraphs[0].text = "" if spec.get("page_numbers") else str(spec["footer_text"])

    try:
        doc.save(out_path)
    except OSError as exc:
        die(4, f"cannot write {out_path}: {exc}")
    print(f"wrote {out_path} ({len(blocks)} blocks)")


if __name__ == "__main__":
    main()
