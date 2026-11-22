#!/usr/bin/env python3
"""Build testdata/pixel_mixed.pdf + pixel_expected.json for the pixel pass.

Pages are image-only scans (JPEG) made from a real text page, with raster seals,
charts and tables pasted in, plus negatives (photo, solid logo, signature, plain
text) and hard cases (skewed table, heavy JPEG compression). One digital page keeps
live text and holds an image chart, to test region restriction.
"""
import io
import json
import math
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

sys.path.insert(0, str(Path(__file__).parent))
from gen_pdfs import build_pdf  # noqa: E402

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else "testdata")
TMP = Path(tempfile.mkdtemp(prefix="pixel_gen_"))
W, H = 595, 842
DPI = 150
S = DPI / 72.0  # pixels per point
random.seed(7)

# Base text page: use the English page from profile_mixed.pdf (real LibreOffice text).
subprocess.run(["pdftoppm", "-r", str(DPI), "-png", "-f", "1", "-l", "1", str(OUT / "profile_mixed.pdf"),
                str(TMP / "base")], check=True)
BASE = Image.open(next(TMP.glob("base*.png"))).convert("RGB")


def px(v):  # points -> pixels
    return int(round(v * S))


def new_page(text=True):
    return BASE.copy() if text else Image.new("RGB", BASE.size, "white")


RED, BLUE = (200, 25, 35), (30, 60, 190)


def seal_round(d, cx, cy, r, col=RED, label="APPROVED"):
    d.ellipse([px(cx - r), px(cy - r), px(cx + r), px(cy + r)], outline=col, width=px(3))
    d.ellipse([px(cx - r + 9), px(cy - r + 9), px(cx + r - 9), px(cy + r - 9)], outline=col, width=px(1.5))
    d.text((px(cx - 28), px(cy - 6)), label, fill=col)
    d.text((px(cx - 22), px(cy + 10)), "2025-03", fill=col)


def seal_square(d, x, y, size, col=RED):
    d.rectangle([px(x), px(y), px(x + size), px(y + size)], outline=col, width=px(3))
    d.rectangle([px(x + 8), px(y + 8), px(x + size - 8), px(y + size - 8)], outline=col, width=px(1.5))
    d.line([px(x + 20), px(y + size / 2), px(x + size - 20), px(y + size / 2)], fill=col, width=px(2))


def bar_chart(d, x, y, w=230, h=150):
    d.line([px(x), px(y), px(x), px(y + h), px(x + w), px(y + h)], fill="black", width=px(1.2))
    for i, (bh, c) in enumerate([(60, "#3366cc"), (110, "#dc3912"), (85, "#109618"), (130, "#ff9900"), (45, "#990099")]):
        d.rectangle([px(x + 12 + i * 42), px(y + h - bh), px(x + 42 + i * 42), px(y + h)], fill=c)


def pie_chart(d, x, y, r=75):
    a = 0
    for frac, c in [(0.4, "#3366cc"), (0.25, "#dc3912"), (0.2, "#109618"), (0.15, "#ff9900")]:
        d.pieslice([px(x - r), px(y - r), px(x + r), px(y + r)], a, a + 360 * frac, fill=c)
        a += 360 * frac


def table(d, x, y, cols=4, rows=5, cw=70, rh=24):
    for r in range(rows + 1):
        d.line([px(x), px(y + r * rh), px(x + cols * cw), px(y + r * rh)], fill="black", width=px(1))
    for c in range(cols + 1):
        d.line([px(x + c * cw), px(y), px(x + c * cw), px(y + rows * rh)], fill="black", width=px(1))
    for r in range(rows):
        for c in range(cols):
            d.text((px(x + c * cw + 6), px(y + r * rh + 6)), f"{(r + 1) * (c + 3) * 7}", fill="black")


def photo(x, y, w=220, h=150):
    """Smooth gradients + noise: lots of distinct colors, like a photograph."""
    im = Image.new("RGB", (px(w), px(h)))
    pix = im.load()
    for yy in range(im.height):
        for xx in range(im.width):
            r = int(120 + 100 * math.sin(xx / 37.0) + random.randint(-25, 25))
            g = int(110 + 90 * math.sin(yy / 29.0 + xx / 90.0) + random.randint(-25, 25))
            b = int(100 + 100 * math.cos((xx + yy) / 53.0) + random.randint(-25, 25))
            pix[xx, yy] = tuple(max(0, min(255, v)) for v in (r, g, b))
    return im.filter(ImageFilter.GaussianBlur(1.2))


def jpeg_page(img, quality=85):
    b = io.BytesIO()
    img.save(b, "JPEG", quality=quality)
    return dict(w=W, h=H, jpeg=(b.getvalue(), img.width, img.height, "RGB"))


pages, expected = [], []


def add(label, page_dict, **exp):
    pages.append(page_dict)
    expected.append({"page": len(pages), "label": label, **exp})


# 1 round red seal on a scan
im = new_page(); seal_round(ImageDraw.Draw(im), 430, 600, 55)
add("scan + round red seal", jpeg_page(im), seals=1, charts=0, tables=0)
# 2 square red seal (stamp-style)
im = new_page(); seal_square(ImageDraw.Draw(im), 380, 560, 100)
add("scan + square red seal", jpeg_page(im), seals=1, charts=0, tables=0)
# 3 blue round seal
im = new_page(); seal_round(ImageDraw.Draw(im), 150, 640, 50, BLUE, "PAID")
add("scan + blue round seal", jpeg_page(im), seals=1, charts=0, tables=0)
# 4 bar chart on scan
im = new_page(); bar_chart(ImageDraw.Draw(im), 100, 400)
add("scan + bar chart", jpeg_page(im), charts=1, seals=0, tables=0)
# 5 pie chart on scan
im = new_page(); pie_chart(ImageDraw.Draw(im), 300, 500)
add("scan + pie chart", jpeg_page(im), charts=1, seals=0, tables=0)
# 6 ruled table on scan
im = new_page(); table(ImageDraw.Draw(im), 100, 400)
add("scan + ruled table", jpeg_page(im), tables=1, charts=0, seals=0)
# 7 negative: plain text scan
add("scan, text only", jpeg_page(new_page()), tables=0, charts=0, seals=0)
# 8 negative: photo
im = new_page(); im.paste(photo(0, 0), (px(100), px(380)))
add("scan + photo (negative)", jpeg_page(im), charts=0, seals=0, tables=0)
# 9 negative: solid red logo disc
im = new_page(); ImageDraw.Draw(im).ellipse([px(380), px(560), px(470), px(650)], fill=RED)
add("scan + solid logo (negative)", jpeg_page(im), seals=0, charts=0, tables=0)
# 10 negative: blue ink signature (thin wide strokes)
im = new_page(); d = ImageDraw.Draw(im)
pts = [(px(330 + i * 4), px(640 + 18 * math.sin(i / 2.5) + (i % 7))) for i in range(48)]
d.line(pts, fill=BLUE, width=px(1.6))
add("scan + signature (negative)", jpeg_page(im), seals=0, charts=0, tables=0)
# 11 hard: table skewed 1.5 degrees
im = new_page(); layer = Image.new("RGB", im.size, "white")
table(ImageDraw.Draw(layer), 100, 400)
layer = layer.rotate(-1.5, resample=Image.BICUBIC, fillcolor="white")
im = Image.composite(layer, im, Image.eval(layer.convert("L"), lambda v: 255 if v < 200 else 0))
add("scan + skewed table 1.5 deg (hard)", jpeg_page(im), tables=1, charts=0, seals=0, hard=True)
# 12 hard: seal + heavy JPEG compression + noise
im = new_page(); seal_round(ImageDraw.Draw(im), 430, 600, 55)
noise = Image.effect_noise(im.size, 18).convert("RGB")
im = Image.blend(im, noise, 0.12)
add("scan + seal, noisy, JPEG q30 (hard)", jpeg_page(im, quality=30), seals=1, charts=0, tables=0, hard=True)
# 13 all three on one scan
im = new_page(); d = ImageDraw.Draw(im)
table(d, 80, 300, cols=3, rows=3, cw=70, rh=22); bar_chart(d, 320, 300, 200, 120); seal_round(d, 450, 620, 50)
add("scan + table + chart + seal", jpeg_page(im), tables=1, charts=1, seals=1)

# 14 digital page (live text) with an image chart inside: restriction to image region
chart_img = Image.new("RGB", (px(240), px(160)), "white")
bar_chart(ImageDraw.Draw(chart_img), 10, 0, 210, 140)
cb = io.BytesIO(); chart_img.save(cb, "PNG")
png = TMP / "chart.png"; png.write_bytes(cb.getvalue())
(TMP / "dig.html").write_text(
    "<html><head><meta charset='utf-8'></head><body style=\"font-family:'Liberation Sans'\">"
    "<p>The quarterly report shows steady growth across all regions.</p>"
    f"<img src='{png}' width='240' height='160'><p>Figure 1.</p></body></html>")
subprocess.run(["soffice", "--headless", "--convert-to", "pdf:writer_web_pdf_Export", "--outdir", str(TMP),
                str(TMP / "dig.html")], check=True, capture_output=True, env={"HOME": str(TMP), "PATH": "/usr/bin:/bin"})
subprocess.run(["pdfseparate", "-f", "1", "-l", "1", str(TMP / "dig.pdf"), str(TMP / "dig1.pdf")], check=True)

scan_pdf = TMP / "scans.pdf"
scan_pdf.write_bytes(build_pdf(pages))
OUT.mkdir(parents=True, exist_ok=True)
subprocess.run(["pdfunite", str(scan_pdf), str(TMP / "dig1.pdf"), str(OUT / "pixel_mixed.pdf")], check=True)
expected.append({"page": len(pages) + 1, "label": "digital page + image chart (region restriction)",
                 "charts": 1, "seals": 0, "tables": 0, "live_text": True})
(OUT / "pixel_expected.json").write_text(json.dumps(expected, indent=1))
print(f"wrote {OUT / 'pixel_mixed.pdf'} ({len(expected)} pages) and pixel_expected.json")
shutil.rmtree(TMP, ignore_errors=True)
