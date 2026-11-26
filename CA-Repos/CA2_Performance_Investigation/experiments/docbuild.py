"""Shared markdown -> docx -> pdf pipeline (cover page, contents with real page numbers)."""
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
ROOT = HERE.parent
DOCS = ROOT / "docs"


def _pdf_pages(pdf):
    if not shutil.which("pdftotext"):
        return []
    txt = subprocess.run(["pdftotext", "-layout", str(pdf), "-"], capture_output=True, text=True).stdout
    return txt.split("\f")


def _docx(md, out, toc, title_lines, student="Siril Saran B", usn="25BTRCL254"):
    from format_docx import format_docx
    cmd = ["pandoc", str(md), "-o", str(out), "--resource-path", str(DOCS), "--resource-path", str(md.parent),
           "--from", "markdown+pipe_tables+task_lists+fenced_divs"]
    ref = DOCS / "reference.docx"
    if ref.exists():
        cmd += ["--reference-doc", str(ref)]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode:
        print("pandoc failed:", r.stderr[:400])
        return False
    cover = dict(logo=DOCS / "logo.png", toc_entries=toc, student=student, usn=usn,
                 faculty="Dr. Sujeet S Jagtap", title_lines=title_lines)
    format_docx(out, cover)
    return True


def _pdf(out):
    if not shutil.which("soffice"):
        return False
    return subprocess.run(["soffice", "--headless", "--convert-to", "pdf", "--outdir", str(out.parent), str(out)],
                          capture_output=True, text=True).returncode == 0


def build(md_path, title_lines, student="Siril Saran B", usn="25BTRCL254"):
    """Build <name>.docx and <name>.pdf next to md_path; contents page numbers are looked up in the PDF."""
    md = Path(md_path)
    out = md.with_suffix(".docx")
    text = md.read_text()
    heads = [h[2:].strip() for h in text.splitlines() if h.startswith("# ")]
    if not shutil.which("pandoc"):
        print("pandoc not found - skipped .docx")
        return
    if not _docx(md, out, [(h, "00") for h in heads], title_lines, student, usn) or not _pdf(out):
        return
    pages = _pdf_pages(md.with_suffix(".pdf"))
    found = [(h, next((i + 1 for i, t in enumerate(pages) if i >= 2 and any(l.strip().replace("\u2019", "'") == h for l in t.splitlines())), "?"))
             for h in heads]
    _docx(md, out, found, title_lines, student, usn)
    _pdf(out)
    print("wrote", out, "and", md.with_suffix(".pdf"))
