# convert.py — docx -> PDF 转换器 (word2pdf 自定义应用内嵌)
# 用法: python convert.py <in.docx> <out.pdf>
#
# 主路径: reportlab + 系统微软雅黑(msyh.ttc) 渲染中文 (CJK 友好).
# 回退: 若环境无 reportlab, 退回手写最小 PDF (仅 Latin-1, 非拉丁字符降级为 '?').
# 这是可行性测试用的轻量转换器; 生产环境应替换为 LibreOffice/Word 无头转换.

import os
import sys

from docx import Document


def _sanitize_latin1(s):
    # 最小转换器用: 非 Latin-1 字符降级为 '?', 避免崩溃.
    return s.encode("latin-1", "replace").decode("latin-1")


def convert_reportlab(src, dst):
    from reportlab.lib.pagesizes import A4
    from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.ttfonts import TTFont
    from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle
    from xml.sax.saxutils import escape

    font_path = "C:/Windows/Fonts/msyh.ttc"
    if not os.path.exists(font_path):
        raise RuntimeError("缺少 CJK 字体: " + font_path)
    try:
        pdfmetrics.registerFont(TTFont("YaHei", font_path, subfontIndex=0))
    except Exception as e:  # noqa: BLE001
        raise RuntimeError("注册字体失败: %s" % e)

    styles = getSampleStyleSheet()
    base = ParagraphStyle("base", parent=styles["Normal"], fontName="YaHei", fontSize=11, leading=16)
    h1 = ParagraphStyle("h1", parent=styles["Heading1"], fontName="YaHei", fontSize=16, leading=22)
    h2 = ParagraphStyle("h2", parent=styles["Heading2"], fontName="YaHei", fontSize=13, leading=18)

    doc = Document(src)
    story = []
    for p in doc.paragraphs:
        text = p.text or ""
        if p.style and p.style.name.startswith("Heading 1"):
            story.append(Paragraph(escape(text) or "&nbsp;", h1))
        elif p.style and p.style.name.startswith("Heading 2"):
            story.append(Paragraph(escape(text) or "&nbsp;", h2))
        else:
            story.append(Paragraph(escape(text) or "&nbsp;", base))
        story.append(Spacer(1, 4))
    for t in doc.tables:
        data = [[cell.text for cell in row.cells] for row in t.rows]
        tbl = Table(data, hAlign="LEFT")
        tbl.setStyle(
            TableStyle(
                [
                    ("FONTNAME", (0, 0), (-1, -1), "YaHei"),
                    ("FONTSIZE", (0, 0), (-1, -1), 10),
                    ("GRID", (0, 0), (-1, -1), 0.5, (0.6, 0.6, 0.6)),
                    ("VALIGN", (0, 0), (-1, -1), "TOP"),
                    ("ROWBACKGROUNDS", (0, 0), (-1, -1), [(0.97, 0.97, 0.97), (1, 1, 1)]),
                ]
            )
        )
        story.append(Spacer(1, 6))
        story.append(tbl)
        story.append(Spacer(1, 6))

    SimpleDocTemplate(dst, pagesize=A4).build(story)


def convert_minimal(src, dst):
    # 手写最小 PDF (Helvetica / Latin-1), 仅作无 reportlab 时的回退.
    doc = Document(src)
    lines = [p.text for p in doc.paragraphs]
    for t in doc.tables:
        for row in t.rows:
            for cell in row.cells:
                lines.append(cell.text)

    page_w, page_h = 595, 842
    margin, font_size, leading = 50, 11, 14
    max_lines = max(1, int((page_h - 2 * margin) / leading))
    pages = [lines[i : i + max_lines] for i in range(0, max(len(lines), 1), max_lines)] or [[""]]

    def esc(s):
        return _sanitize_latin1(s).replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")

    objs = []
    objs.append("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")
    kids = " ".join("%d 0 R" % (4 + 2 * i + 1) for i in range(len(pages)))
    objs.append("2 0 obj\n<< /Type /Pages /Kids [%s] /Count %d >>\nendobj\n" % (kids, len(pages)))
    objs.append("3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n")
    for idx, plines in enumerate(pages):
        content_num = 4 + 2 * idx
        page_num = 4 + 2 * idx + 1
        stream = "BT\n/F1 %d Tf\n%d %d Td\n%d TL\n" % (font_size, margin, page_h - margin, leading)
        for ln in plines:
            stream += "(%s) Tj T*\n" % (esc(ln) if ln else " ")
        stream += "ET"
        objs.append("%d 0 obj\n<< /Length %d >>\nstream\n%s\nendstream\nendobj\n" % (content_num, len(stream), stream))
        objs.append(
            "%d 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d] "
            "/Resources << /Font << /F1 3 0 R >> >> /Contents %d 0 R >>\nendobj\n"
            % (page_num, page_w, page_h, content_num)
        )

    pdf = b"%PDF-1.4\n"
    offsets = []
    for obj in objs:
        offsets.append(len(pdf))
        pdf += obj.encode("latin-1", "replace")
    xref_pos = len(pdf)
    n = len(objs) + 1
    xref = "xref\n0 %d\n" % n
    xref += "0000000000 65535 f \n"
    for off in offsets:
        xref += "%010d 00000 n \n" % off
    trailer = "trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n, xref_pos)
    pdf += xref.encode("latin-1", "replace") + trailer.encode("latin-1", "replace")
    with open(dst, "wb") as f:
        f.write(pdf)


def main():
    if len(sys.argv) < 3:
        sys.stderr.write("usage: convert.py <in.docx> <out.pdf>\n")
        sys.exit(2)
    src, dst = sys.argv[1], sys.argv[2]
    try:
        convert_reportlab(src, dst)
        print("OK (reportlab) -> %s" % dst)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write("reportlab 路径失败, 回退最小转换器: %s\n" % e)
        convert_minimal(src, dst)
        print("OK (minimal) -> %s" % dst)


if __name__ == "__main__":
    main()
