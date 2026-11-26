"""Post-process the pandoc .docx so tables, figures and markers match the research-paper look.

* tables: black 0.5 pt grid, light-grey bold header row (repeats across pages), column widths
  proportional to content, smaller font for wide tables, rows never split across pages
* figures: scaled to the 6.5-inch text width, centred, kept with their caption
* [STUDENT: ...] markers: yellow highlight so they cannot be missed
"""
import re
import sys

from docx import Document
from docx.enum.text import WD_ALIGN_PARAGRAPH, WD_COLOR_INDEX
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Emu, Inches, Pt

TEXT_WIDTH_TW = 9360  # 6.5 inch in twentieths of a point


def _set(el, tag, **attrs):
    e = el.find(qn(tag))
    if e is None:
        e = OxmlElement(tag)
        el.append(e)
    for k, v in attrs.items():
        e.set(qn(k), str(v))
    return e


def _cell_text(cell):
    return " ".join(p.text for p in cell.paragraphs)


def column_widths(tbl):
    ncol = len(tbl.columns)
    weights = [0.0] * ncol
    for row in tbl.rows:
        for j, cell in enumerate(row.cells[:ncol]):
            txt = _cell_text(cell)
            longest_word = max((len(w) for w in re.split(r"\s+", txt) if w), default=1)
            # width driven by the text length, but a column must at least fit its longest word
            weights[j] = max(weights[j], min(len(txt), 46) * 0.55 + longest_word * 0.45)
    weights = [max(w, 6.0) for w in weights]
    tot = sum(weights)
    w = [int(TEXT_WIDTH_TW * x / tot) for x in weights]
    w[-1] += TEXT_WIDTH_TW - sum(w)
    return w


def style_table(tbl):
    ncol = len(tbl.columns)
    tblPr = tbl._tbl.tblPr
    for old in tblPr.findall(qn("w:tblBorders")):
        tblPr.remove(old)
    borders = OxmlElement("w:tblBorders")
    for side in ("top", "left", "bottom", "right", "insideH", "insideV"):
        e = OxmlElement(f"w:{side}")
        e.set(qn("w:val"), "single")
        e.set(qn("w:sz"), "4")
        e.set(qn("w:space"), "0")
        e.set(qn("w:color"), "000000")
        borders.append(e)
    tblPr.append(borders)
    _set(tblPr, "w:tblW", **{"w:w": TEXT_WIDTH_TW, "w:type": "dxa"})
    _set(tblPr, "w:tblLayout", **{"w:type": "fixed"})
    for old in tblPr.findall(qn("w:tblCellMar")):
        tblPr.remove(old)
    mar = OxmlElement("w:tblCellMar")
    for side, v in (("top", 25), ("left", 85), ("bottom", 25), ("right", 85)):
        e = OxmlElement(f"w:{side}")
        e.set(qn("w:w"), str(v))
        e.set(qn("w:type"), "dxa")
        mar.append(e)
    tblPr.append(mar)

    widths = column_widths(tbl)
    grid = tbl._tbl.tblGrid
    for gc, w in zip(grid.findall(qn("w:gridCol")), widths):
        gc.set(qn("w:w"), str(w))

    size = 8 if ncol >= 7 else (9 if ncol >= 5 else 10)
    for ri, row in enumerate(tbl.rows):
        trPr = row._tr.get_or_add_trPr()
        _set(trPr, "w:cantSplit")
        if ri == 0:
            _set(trPr, "w:tblHeader")
        for j, cell in enumerate(row.cells[:ncol]):
            tcPr = cell._tc.get_or_add_tcPr()
            _set(tcPr, "w:tcW", **{"w:w": widths[j], "w:type": "dxa"})
            if ri == 0:
                for old in tcPr.findall(qn("w:shd")):
                    tcPr.remove(old)
                shd = OxmlElement("w:shd")
                shd.set(qn("w:val"), "clear")
                shd.set(qn("w:color"), "auto")
                shd.set(qn("w:fill"), "F2F2F2")
                tcPr.append(shd)
            for p in cell.paragraphs:
                pf = p.paragraph_format
                pf.space_before = Pt(1)
                pf.space_after = Pt(1)
                pf.line_spacing = 1.0
                p.alignment = WD_ALIGN_PARAGRAPH.LEFT
                for r in p.runs:
                    r.font.size = Pt(size)
                    r.font.name = "Calibri"
                    if ri == 0:
                        r.bold = True


def style_figures(doc):
    max_w = Inches(6.5)
    for shape in doc.inline_shapes:
        w, h = shape.width, shape.height
        if w > max_w:
            shape.height = Emu(int(h * max_w / w))
            shape.width = max_w
    for p in doc.paragraphs:
        if p._p.xpath(".//pic:pic"):
            p.alignment = WD_ALIGN_PARAGRAPH.CENTER
            p.paragraph_format.keep_with_next = True
            p.paragraph_format.space_before = Pt(6)
            p.paragraph_format.space_after = Pt(2)


def highlight_markers(doc):
    def paras():
        yield from doc.paragraphs
        for t in doc.tables:
            for row in t.rows:
                for cell in row.cells:
                    yield from cell.paragraphs

    for p in paras():
        if "[STUDENT" not in p.text:
            continue
        on = False
        for r in p.runs:
            if "[STUDENT" in r.text:
                on = True
            if on:
                r.font.highlight_color = WD_COLOR_INDEX.YELLOW
            if on and "]" in r.text and "[STUDENT" not in r.text.split("]", 1)[1]:
                on = False


def space_after_tables(doc):
    """Give the paragraph that follows a table some air (Word glues it to the grid)."""
    from docx.text.paragraph import Paragraph
    for t in doc.tables:
        nxt = t._tbl.getnext()
        if nxt is not None and nxt.tag == qn("w:p"):
            Paragraph(nxt, t._parent).paragraph_format.space_before = Pt(8)


def format_docx(path, cover=None):
    doc = Document(str(path))
    for t in doc.tables:
        style_table(t)
    space_after_tables(doc)
    style_figures(doc)
    highlight_markers(doc)
    if cover:
        add_cover_and_contents(doc, **cover)
    doc.save(str(path))


if __name__ == "__main__":
    format_docx(sys.argv[1])
    print("formatted", sys.argv[1])


# ------------------------------------------------------------------ cover + contents
SERIF = "Times New Roman"


def _run(p, text, size, bold=False, font=SERIF):
    r = p.add_run(text)
    r.font.name = font
    r._element.rPr.rFonts.set(qn("w:eastAsia"), font)
    r.font.size = Pt(size)
    r.bold = bold
    return r


def _field(p, instr, size=11, font=SERIF):
    for kind, txt in (("begin", None), (None, instr), ("separate", None), ("t", "1"), ("end", None)):
        r = p.add_run()
        r.font.name, r.font.size = font, Pt(size)
        if kind in ("begin", "separate", "end"):
            e = OxmlElement("w:fldChar")
            e.set(qn("w:fldCharType"), kind)
            r._r.append(e)
        elif kind is None:
            e = OxmlElement("w:instrText")
            e.set(qn("xml:space"), "preserve")
            e.text = f" {txt} "
            r._r.append(e)
        else:
            t = OxmlElement("w:t")
            t.text = txt
            r._r.append(t)


def _bordered_table(doc, rows):
    t = doc.add_table(rows=len(rows), cols=2)
    tblPr = t._tbl.tblPr
    b = OxmlElement("w:tblBorders")
    for side in ("top", "left", "bottom", "right", "insideH", "insideV"):
        e = OxmlElement(f"w:{side}")
        e.set(qn("w:val"), "single"); e.set(qn("w:sz"), "6"); e.set(qn("w:color"), "000000")
        b.append(e)
    tblPr.append(b)
    for row, (k, v) in zip(t.rows, rows):
        for cell, txt, bold in ((row.cells[0], k, True), (row.cells[1], v, False)):
            cell.width = Inches(3.25)
            p = cell.paragraphs[0]
            p.paragraph_format.space_before = p.paragraph_format.space_after = Pt(3)
            p.paragraph_format.left_indent = Pt(4)
            _run(p, txt, 12, bold)
    return t


def add_cover_and_contents(doc, logo, toc_entries, student, usn, faculty, title_lines=None):
    """Cover page in the style of the department lab record + a contents page (static, with page numbers)."""
    body = doc.element.body
    start = len(body)  # elements appended below are moved to the front afterwards
    new = []

    def para(text="", size=12, bold=False, align=WD_ALIGN_PARAGRAPH.CENTER, before=0, after=0):
        p = doc.add_paragraph()
        p.alignment = align
        p.paragraph_format.space_before, p.paragraph_format.space_after = Pt(before), Pt(after)
        if text:
            _run(p, text, size, bold)
        new.append(p._p)
        return p

    p = para(before=0, after=0)
    p.add_run().add_picture(str(logo), width=Inches(4.47))
    para("OPERATING SYSTEMS", 16, True, before=36)
    for text, size, bold, before in (title_lines or [
            ("CONTINUOUS ASSESSMENT CA-2", 16, True, 6),
            ("OS PERFORMANCE INVESTIGATION", 13, False, 6),
            ("Page Replacement for AI/ML-Like Access Patterns (Research Topic R19)", 12, False, 4)]):
        para(text, size, bold, before=before)
    para("Submitted by", 12, True, WD_ALIGN_PARAGRAPH.LEFT, before=28, after=4)
    t = _bordered_table(doc, [("STUDENT NAME", student), ("USN", usn)])
    new.append(t._tbl)
    para("Submitted to", 12, True, WD_ALIGN_PARAGRAPH.LEFT, before=28, after=4)
    t = _bordered_table(doc, [("FACULTY NAME", faculty)])
    new.append(t._tbl)
    para("Course code: 25PCCS201", 12, False, before=28)
    para(f"JAIN (Deemed-to-be University), FET Campus", 12, False, before=2)
    pb = para()
    pb.add_run().add_break(__import__("docx").enum.text.WD_BREAK.PAGE)

    # contents page
    para("CONTENTS", 16, True, before=0, after=14)
    for text, page in toc_entries:
        p = doc.add_paragraph()
        pf = p.paragraph_format
        pf.space_after = Pt(6)
        pf.tab_stops.add_tab_stop(Inches(6.5), alignment=2, leader=1)  # right tab, dot leader
        _run(p, text, 12)
        _run(p, "\t" + str(page), 12)
        new.append(p._p)
    pb = para()
    pb.add_run().add_break(__import__("docx").enum.text.WD_BREAK.PAGE)

    first = body[0]
    for el in new:
        body.remove(el)
        first.addprevious(el)

    sec = doc.sections[0]
    # no running header / border on the cover (as in the lab record); header + border from page 2 on
    sec.different_first_page_header_footer = True
    pb_el = sec._sectPr.find(qn("w:pgBorders"))
    if pb_el is not None:
        pb_el.set(qn("w:display"), "notFirstPage")
    # no running header text on any page
    for hd in (sec.header, sec.first_page_header):
        for hp in hd.paragraphs:
            for r in list(hp.runs):
                r._element.getparent().remove(r._element)
    # page number, bottom centre
    for ft in (sec.footer, sec.first_page_footer):
        ft.is_linked_to_previous = False
        fp = ft.paragraphs[0]
        fp.alignment = WD_ALIGN_PARAGRAPH.CENTER
        _field(fp, "PAGE")
