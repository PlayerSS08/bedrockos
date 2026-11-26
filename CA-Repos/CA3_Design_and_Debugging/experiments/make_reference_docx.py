"""Create docs/reference.docx: the Word template that gives the CA-2 report the same
look as the research-activity paper (US Letter, 1-inch margins, bold Arial running
header on every page, thin page border, Calibri 11 body, Calibri bold headings).

Measured from the research PDF: margins 72 pt; header at 36 pt; right header text
starts 3.55 in from the left margin; page border 24 pt from the top/left edge and
~32 pt from the right edge (no bottom rule); body Calibri 11; headings Calibri bold 13;
title Calibri bold 16.

Run once (the generated file is committed):  python experiments/make_reference_docx.py
"""
import shutil
import subprocess
import sys
from pathlib import Path

from docx import Document
from docx.enum.text import WD_LINE_SPACING
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Inches, Pt, RGBColor

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "reference.docx"

HEADER_LEFT = ("JAIN, FET CAMPUS", "CA-2: OS Performance Investigation")
HEADER_RIGHT = "25PCCS201: Operating Systems"


def set_font(style, name, size=None, bold=None, italic=None, color=RGBColor(0, 0, 0)):
    f = style.font
    f.name = name
    if size:
        f.size = Pt(size)
    if bold is not None:
        f.bold = bold
    if italic is not None:
        f.italic = italic
    if color is not None:
        f.color.rgb = color
    rpr = style.element.get_or_add_rPr()
    rf = rpr.find(qn("w:rFonts"))
    if rf is None:
        rf = OxmlElement("w:rFonts")
        rpr.insert(0, rf)
    for a in ("w:asciiTheme", "w:hAnsiTheme", "w:eastAsiaTheme", "w:cstheme"):
        if rf.get(qn(a)) is not None:
            del rf.attrib[qn(a)]
    for a in ("w:ascii", "w:hAnsi", "w:cs", "w:eastAsia"):
        rf.set(qn(a), name)


def spacing(style, before, after, line=1.08, keep_next=None):
    pf = style.paragraph_format
    pf.space_before = Pt(before)
    pf.space_after = Pt(after)
    pf.line_spacing = line
    if keep_next is not None:
        pf.keep_with_next = keep_next


def get_or_add(doc, name, base="Normal"):
    for x in doc.styles:
        if x.name.lower() == name.lower():
            return x
    else:
        from docx.enum.style import WD_STYLE_TYPE
        st = doc.styles.add_style(name, WD_STYLE_TYPE.PARAGRAPH)
        for x in doc.styles:
            if x.name.lower() == base.lower():
                st.base_style = x
        return st


def main():
    tmp = OUT.with_name("_default_reference.docx")
    OUT.parent.mkdir(exist_ok=True)
    with open(tmp, "wb") as f:
        subprocess.run(["pandoc", "--print-default-data-file", "reference.docx"],
                       stdout=f, check=True)
    doc = Document(str(tmp))

    class _Styles:
        """Case-insensitive style lookup (pandoc names them 'Heading 1', python-docx
        expects 'heading 1')."""

        def __init__(self, styles):
            self._s = styles

        def __getitem__(self, name):
            for x in self._s:
                if x.name.lower() == name.lower():
                    return x
            raise KeyError(name)

        def __iter__(self):
            return iter(self._s)

    st = _Styles(doc.styles)

    set_font(st["Normal"], "Calibri", 11, color=RGBColor(0, 0, 0))
    spacing(st["Normal"], 0, 8)
    for name in ("Body Text", "First Paragraph"):
        set_font(st[name], "Calibri", 11)
        spacing(st[name], 0, 8)
    set_font(st["Compact"], "Calibri", 11)
    spacing(st["Compact"], 0, 5)
    for name, size in (("Heading 1", 13), ("Heading 2", 12), ("Heading 3", 11), ("Heading 4", 11)):
        set_font(st[name], "Calibri", size, bold=True, italic=False)
        spacing(st[name], 16 if name == "Heading 1" else 10, 4, keep_next=True)
    set_font(st["Block Text"], "Calibri", 10.5, italic=True, color=RGBColor(0x40, 0x40, 0x40))
    spacing(st["Block Text"], 4, 8)
    try:
        set_font(st["Source Code"], "Consolas", 9)
    except KeyError:
        pass
    for nm in ("Verbatim Char",):
        try:
            set_font(st[nm], "Consolas", 9.5)
        except KeyError:
            pass
    for nm in ("Image Caption", "Table Caption", "Captioned Figure"):
        try:
            set_font(st[nm], "Calibri", 10, italic=True, color=RGBColor(0x40, 0x40, 0x40))
            spacing(st[nm], 2, 8)
        except KeyError:
            pass
    try:
        hl = st["Hyperlink"]
        hl.font.color.rgb = RGBColor(0x05, 0x63, 0xC1)
        hl.font.underline = True
    except KeyError:
        pass

    # custom styles for the first-page title block (like the research paper)
    t = get_or_add(doc, "ReportTitle")
    set_font(t, "Calibri", 16, bold=True)
    spacing(t, 12, 8, keep_next=True)
    m = get_or_add(doc, "ReportMeta")
    set_font(m, "Calibri", 11)
    spacing(m, 0, 8)
    f = get_or_add(doc, "FooterNote")
    set_font(f, "Calibri", 10, italic=True, color=RGBColor(0x40, 0x40, 0x40))
    spacing(f, 0, 6)

    # page geometry
    sec = doc.sections[0]
    sec.page_width, sec.page_height = Inches(8.5), Inches(11)
    sec.left_margin = sec.right_margin = sec.top_margin = sec.bottom_margin = Inches(1)
    sec.header_distance = Inches(0.5)
    sec.footer_distance = Inches(0.5)

    # page border: top, left, right measured from the page edge (as in the research paper)
    sect = sec._sectPr
    for old in sect.findall(qn("w:pgBorders")):
        sect.remove(old)
    pg = OxmlElement("w:pgBorders")
    pg.set(qn("w:offsetFrom"), "page")
    for side, space in (("top", 24), ("left", 24), ("right", 31)):
        e = OxmlElement(f"w:{side}")
        e.set(qn("w:val"), "single")
        e.set(qn("w:sz"), "4")
        e.set(qn("w:space"), str(space))
        e.set(qn("w:color"), "000000")
        pg.append(e)
    pgmar = sect.find(qn("w:pgMar"))
    pgmar.addnext(pg)

    # running header on every page: bold Arial, two lines left + course on the right
    hdr = sec.header
    hdr.is_linked_to_previous = False
    for p in list(hdr.paragraphs)[1:]:
        p._element.getparent().remove(p._element)
    p1 = hdr.paragraphs[0]
    for r in list(p1.runs):
        r._element.getparent().remove(r._element)
    hstyle = get_or_add(doc, "Header")
    p1.style = hstyle
    p1.paragraph_format.tab_stops.add_tab_stop(Inches(3.55))
    p1.paragraph_format.space_after = Pt(0)
    for text in (HEADER_LEFT[0], "\t", HEADER_RIGHT):
        r = p1.add_run(text)
        r.bold = True
        r.font.name = "Arial"
        r.font.size = Pt(11)
    p2 = hdr.add_paragraph()
    p2.style = hstyle
    p2.paragraph_format.space_after = Pt(0)
    r = p2.add_run(HEADER_LEFT[1])
    r.bold = True
    r.font.name = "Arial"
    r.font.size = Pt(11)
    # tidy header style: no centered / right tabs inherited from the default template
    hs = hstyle
    hs.paragraph_format.space_after = Pt(0)
    hs.paragraph_format.line_spacing = 1.0
    ppr = hs.element.get_or_add_pPr()
    for tabs in ppr.findall(qn("w:tabs")):
        ppr.remove(tabs)

    doc.save(str(OUT))
    tmp.unlink()
    print("wrote", OUT)


if __name__ == "__main__":
    if not shutil.which("pandoc"):
        sys.exit("pandoc is required to build the reference document")
    main()
