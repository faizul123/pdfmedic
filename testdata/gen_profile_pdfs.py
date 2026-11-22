#!/usr/bin/env python3
"""Build testdata/profile_mixed.pdf (16 pages) + profile_expected.json (ground truth).

Needs: LibreOffice (soffice), pycairo, Pillow, poppler-utils (pdfunite, pdftoppm),
Ghostscript (gs). Fonts: Noto Sans Arabic. Pages come from real producers on
purpose (LibreOffice, cairo, JPEG scans) so triage is tested on realistic objects.
"""
import io
import json
import math
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import cairo
from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).parent))
from gen_pdfs import build_pdf  # noqa: E402

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else "testdata")
TMP = Path(tempfile.mkdtemp(prefix="profile_gen_"))
W, H = 595, 842

STYLE = "body{font-family:'Liberation Sans','Noto Sans Arabic',sans-serif;font-size:12pt}"
EN = ("The quarterly report shows steady growth across all regions. Revenue increased while operating "
      "costs remained stable, and the board expects the trend to continue into the next financial year.")
FR = ("Le rapport trimestriel montre une croissance régulière dans toutes les régions. Les revenus ont "
      "augmenté tandis que les coûts sont restés stables, et le conseil prévoit que cette tendance se poursuivra.")
AR = ("يظهر التقرير ربع السنوي نموا مستقرا في جميع المناطق. ارتفعت الإيرادات بينما بقيت التكاليف "
      "التشغيلية مستقرة ويتوقع المجلس استمرار هذا الاتجاه خلال السنة المالية القادمة.")


def soffice_pdf(name, body):
    html = TMP / f"{name}.html"
    html.write_text(f"<html><head><meta charset='utf-8'><style>{STYLE}</style></head><body>{body}</body></html>",
                    encoding="utf-8")
    subprocess.run(["soffice", "--headless", "--convert-to", "pdf:writer_web_pdf_Export", "--outdir", str(TMP),
                    str(html)], check=True, capture_output=True, env={"HOME": str(TMP), "PATH": "/usr/bin:/bin"})
    pdf = TMP / f"{name}.pdf"
    # Keep only page 1 (HTML export can spill to a second page).
    one = TMP / f"{name}_1.pdf"
    subprocess.run(["pdfseparate", "-f", "1", "-l", "1", str(pdf), str(one)], check=True)
    return one


def cairo_pdf(name, draw):
    path = TMP / f"{name}.pdf"
    surf = cairo.PDFSurface(str(path), W, H)
    ctx = cairo.Context(surf)
    ctx.select_font_face("DejaVu Sans")
    draw(ctx)
    ctx.show_page()
    surf.finish()
    return path


def text(ctx, x, y, s, size=12, rgb=(0, 0, 0)):
    ctx.set_source_rgb(*rgb)
    ctx.set_font_size(size)
    ctx.move_to(x, y)
    ctx.show_text(s)


def bar_chart(ctx):
    text(ctx, 72, 80, "Quarterly revenue by region", 16)
    base, x = 400, 110
    ctx.set_source_rgb(0, 0, 0)
    ctx.set_line_width(1)
    ctx.move_to(100, 150); ctx.line_to(100, base); ctx.line_to(480, base); ctx.stroke()
    for i, (h, rgb) in enumerate([(120, (0.2, 0.4, 0.8)), (200, (0.9, 0.5, 0.1)), (160, (0.2, 0.7, 0.3)),
                                  (240, (0.8, 0.2, 0.2)), (90, (0.5, 0.3, 0.7))]):
        ctx.set_source_rgb(*rgb)
        ctx.rectangle(x + i * 70, base - h, 45, h)
        ctx.fill()
        text(ctx, x + i * 70, base + 18, f"Q{i + 1}", 10)
    text(ctx, 72, 470, "Figure 1: revenue by quarter (USD millions)", 10)


def pie_chart(ctx):
    text(ctx, 72, 80, "Market share", 16)
    cx, cy, r = 300, 300, 110
    a = -math.pi / 2
    for frac, rgb in [(0.4, (0.2, 0.4, 0.8)), (0.25, (0.9, 0.5, 0.1)), (0.2, (0.2, 0.7, 0.3)), (0.15, (0.8, 0.2, 0.2))]:
        b = a + 2 * math.pi * frac
        ctx.set_source_rgb(*rgb)
        ctx.move_to(cx, cy); ctx.arc(cx, cy, r, a, b); ctx.close_path(); ctx.fill()
        a = b


def vector_seal(ctx):
    for i, line in enumerate([EN[:70], EN[70:140], EN[140:]]):
        text(ctx, 72, 100 + i * 18, line, 11)
    cx, cy = 430, 600
    ctx.set_source_rgb(0.8, 0.1, 0.15)
    ctx.new_path()  # without this, arc() draws a line from the last text position
    ctx.set_line_width(3); ctx.arc(cx, cy, 55, 0, 2 * math.pi); ctx.stroke()
    ctx.new_path()
    ctx.set_line_width(1.5); ctx.arc(cx, cy, 46, 0, 2 * math.pi); ctx.stroke()
    text(ctx, cx - 32, cy - 4, "APPROVED", 11, (0.8, 0.1, 0.15))
    text(ctx, cx - 22, cy + 14, "2025-03", 9, (0.8, 0.1, 0.15))


def png_bytes(img):
    b = io.BytesIO(); img.save(b, "PNG"); return b.getvalue()


def raster_chart_png():
    img = Image.new("RGB", (480, 300), "white")
    d = ImageDraw.Draw(img)
    d.line([(40, 20), (40, 260), (450, 260)], fill="black", width=2)
    for i, (h, c) in enumerate([(100, "#3366cc"), (180, "#dc3912"), (140, "#109618"), (210, "#ff9900")]):
        d.rectangle([70 + i * 95, 260 - h, 120 + i * 95, 260], fill=c)
    p = TMP / "raster_chart.png"; img.save(p); return p


def raster_seal_png():
    img = Image.new("RGBA", (180, 180), (255, 255, 255, 0))
    d = ImageDraw.Draw(img)
    d.ellipse([4, 4, 176, 176], outline=(200, 20, 30, 255), width=6)
    d.ellipse([20, 20, 160, 160], outline=(200, 20, 30, 255), width=3)
    d.text((55, 82), "APPROVED", fill=(200, 20, 30, 255))
    p = TMP / "raster_seal.png"; img.save(p); return p


pages = []  # (label, pdf path, expectations)


def add(label, path, **exp):
    pages.append((label, Path(path), exp))


# 1-4 languages
add("en text", soffice_pdf("en", f"<h2>Quarterly Report</h2><p>{EN}</p><p>{EN}</p>"),
    language="en", scan="no", route="RustOnly", tables=0, charts=0, seals=0)
add("fr text", soffice_pdf("fr", f"<h2>Rapport trimestriel</h2><p>{FR}</p><p>{FR}</p>"),
    language="fr", scan="no", route="RustOnly", tables=0, charts=0, seals=0)
add("ar text", soffice_pdf("ar", f"<h2 dir='rtl'>التقرير الفصلي</h2><p dir='rtl' lang='ar'>{AR}</p><p dir='rtl' lang='ar'>{AR}</p>"),
    language="ar", scan="no", route="RustOnly", tables=0, charts=0, seals=0)
add("bilingual en+ar", soffice_pdf("bi", f"<p>{EN}</p><p dir='rtl' lang='ar'>{AR}</p>"),
    scripts=["latin", "arabic"], scan="no", route="RustOnly", tables=0, charts=0, seals=0)

# 5-6 tables
rows = [("Region", "Q1", "Q2", "Q3"), ("North", "120", "135", "150"), ("South", "98", "101", "110"),
        ("East", "143", "150", "162"), ("West", "87", "92", "99")]


def table_html(border):
    # LibreOffice's HTML import ignores the border attribute here; CSS borders work.
    b = "border:1px solid black;" if border else ""
    cells = "".join("<tr>" + "".join(f"<td style='padding:4pt 14pt;{b}'>{c}</td>" for c in r) + "</tr>" for r in rows)
    return (f"<p>Sales by region and quarter.</p>"
            f"<table style='border-collapse:collapse;{b}' cellspacing='0'>{cells}</table>")


add("ruled table", soffice_pdf("tbl_ruled", table_html(1)),
    scan="no", route="RustOnly", tables=1, charts=0, seals=0)
add("borderless table", soffice_pdf("tbl_plain", table_html(0)),
    scan="no", route="RustOnly", tables=1, charts=0, seals=0)

# 7-8 vector charts
add("vector bar chart", cairo_pdf("bars", bar_chart), scan="no", route="RustOnly", tables=0, charts=1, seals=0)
add("vector pie chart", cairo_pdf("pie", pie_chart), scan="no", route="RustOnly", tables=0, charts=1, seals=0)

# 9 raster chart (cannot be seen at object level: expect pixel-pass flag, no chart candidate)
rc = raster_chart_png()
add("raster chart", soffice_pdf("rchart", f"<p>{EN}</p><img src='{rc}' width='480' height='300'>"),
    scan="no", route="RustOnly", tables=0, charts=0, seals=0, pixel_pass=True, known_miss="raster chart")

# 10-11 seals
add("vector seal", cairo_pdf("vseal", vector_seal), scan="no", route="RustOnly", tables=0, charts=0, seals=1)
rs = raster_seal_png()
add("raster seal", soffice_pdf("rseal", f"<p>{EN}</p><p><img src='{rs}' width='120' height='120'></p>"),
    scan="no", route="RustOnly", tables=0, charts=0, seals=1)

# 12-13 scans from page 1
src = pages[0][1]
subprocess.run(["pdftoppm", "-r", "150", "-jpeg", "-jpegopt", "quality=85", "-f", "1", "-l", "1", str(src),
                str(TMP / "scan")], check=True)
jpg = next(TMP.glob("scan*.jpg"))
jb = jpg.read_bytes()
with Image.open(jpg) as im:
    jw, jh = im.size
scan_page = dict(w=W, h=H, jpeg=(jb, jw, jh, "RGB"))
scan_pdf = TMP / "scan.pdf"; scan_pdf.write_bytes(build_pdf([scan_page]))
add("scan (image only)", scan_pdf, scan="scanned", route="Ocr", tables=0, charts=0, seals=0, pixel_pass=True)
ocr_pdf = TMP / "scan_ocr.pdf"
ocr_pdf.write_bytes(build_pdf([dict(scan_page, hidden=[EN[:90], EN[90:180], EN[180:]])]))
add("scan + hidden text layer", ocr_pdf, scan="scanned_with_text_layer", route="VerifyTextLayer",
    language="en", tables=0, charts=0, seals=0, pixel_pass=True)

# 14 annotation-only stamp page
stamp_pdf = TMP / "stamp.pdf"
stamp_pdf.write_bytes(build_pdf([dict(w=W, h=H, stamp=(200, 400, 160, 90))]))
add("stamp annotation only", stamp_pdf, scan="no", route="RustOnly", tables=0, charts=0, seals=1, blank=False)

# 15 blank
blank_pdf = TMP / "blank.pdf"; blank_pdf.write_bytes(build_pdf([dict(w=W, h=H)]))
add("blank", blank_pdf, scan="no", route="Skip", tables=0, charts=0, seals=0, blank=True)

# 16 text converted to outlines (no text objects at all)
outl = TMP / "outlined.pdf"
subprocess.run(["gs", "-q", "-dNOPAUSE", "-dBATCH", "-sDEVICE=pdfwrite", "-dNoOutputFonts",
                f"-sOutputFile={outl}", str(src)], check=True)
add("outlined text", outl, scan="no", route="Ocr", tables=0, charts=0, seals=0, pixel_pass=True)

OUT.mkdir(parents=True, exist_ok=True)
subprocess.run(["pdfunite", *[str(p) for _, p, _ in pages], str(OUT / "profile_mixed.pdf")], check=True)
exp = [{"page": i + 1, "label": lbl, **e} for i, (lbl, _, e) in enumerate(pages)]
(OUT / "profile_expected.json").write_text(json.dumps(exp, indent=1, ensure_ascii=False))
print(f"wrote {OUT / 'profile_mixed.pdf'} ({len(pages)} pages) and profile_expected.json")
shutil.rmtree(TMP, ignore_errors=True)
