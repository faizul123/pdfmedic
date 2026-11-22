#!/usr/bin/env python3
"""Generate small PDFs for pipeline tests.

Vector PDFs are written by hand (no dependencies). Scan-like PDFs (image only,
no text layer) need Pillow and `pdftoppm` (poppler-utils); they are skipped if
either is missing.
"""
import subprocess
import sys
from pathlib import Path


def build_pdf(pages, info=None):
    """pages: list of dicts with keys
    w,h            page size in points
    rotate         /Rotate value
    text           list of str or (size, str); ("", gap) adds vertical space
    text_rot90     list of str drawn bottom-to-top (glyphs rotated 90 deg ccw)
    rect           draw a filled rectangle
    image          draw a 32x32 gradient image XObject at (100,400) 200x200
    jpeg           (bytes, w_px, h_px, "RGB"|"L"): full-page JPEG image
    hidden         list of str drawn with text render mode 3 (invisible OCR layer)
    stamp          (x, y, w, h): a red-rectangle Stamp annotation (annotation-only content)
    """
    objs = []

    def add(b):
        objs.append(b)
        return len(objs)

    add(b"")  # 1 catalog
    add(b"")  # 2 pages
    font = add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    page_ids = []
    for p in pages:
        stream = []
        xobj = ""
        if p.get("image"):
            n = 32
            raw = bytearray()
            for y in range(n):
                for x in range(n):
                    raw += bytes([x * 8, y * 8, 160])
            iid = add(
                b"<< /Type /XObject /Subtype /Image /Width 32 /Height 32 /ColorSpace /DeviceRGB "
                b"/BitsPerComponent 8 /Length %d >>\nstream\n" % len(raw) + bytes(raw) + b"\nendstream"
            )
            xobj = "/XObject << /Im1 %d 0 R >>" % iid
            stream.append("q 200 0 0 200 100 400 cm /Im1 Do Q")
        if p.get("jpeg"):
            jb, jw, jh, cs = p["jpeg"]
            jid = add(
                b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /Device%s "
                b"/BitsPerComponent 8 /Filter /DCTDecode /Length %d >>\nstream\n"
                % (jw, jh, b"RGB" if cs == "RGB" else b"Gray", len(jb))
                + jb
                + b"\nendstream"
            )
            xobj = "/XObject << /Im1 %d 0 R >>" % jid
            stream.append("q %d 0 0 %d 0 0 cm /Im1 Do Q" % (p["w"], p["h"]))
        if p.get("hidden"):
            y = p["h"] - 72
            for line in p["hidden"]:
                esc = line.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
                stream.append("BT 3 Tr /F1 12 Tf 72 %.1f Td (%s) Tj ET" % (y, esc))
                y -= 16
        annots = ""
        if p.get("stamp"):
            sx, sy, sw, sh = p["stamp"]
            fdata = ("1 0 0 RG 3 w 4 4 %d %d re S" % (sw - 8, sh - 8)).encode()
            fid = add(
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 %d %d] /Length %d >>\nstream\n" % (sw, sh, len(fdata))
                + fdata
                + b"\nendstream"
            )
            aid = add(
                (
                    "<< /Type /Annot /Subtype /Stamp /Rect [%d %d %d %d] /F 4 /Name /Approved "
                    "/AP << /N %d 0 R >> >>" % (sx, sy, sx + sw, sy + sh, fid)
                ).encode()
            )
            annots = " /Annots [%d 0 R]" % aid
        if p.get("rect"):
            stream.append("0.2 0.2 0.8 rg 100 100 300 150 re f")
        if p.get("text"):
            y = p["h"] - 72
            stream.append("0 g")
            for item in p["text"]:
                size, line = item if isinstance(item, tuple) else (14, item)
                if line == "":
                    y -= size
                    continue
                esc = line.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
                stream.append("BT /F1 %d Tf 72 %.1f Td (%s) Tj ET" % (size, y, esc))
                y -= size * 1.3
        if p.get("text_rot90"):
            # Text matrix [0 1 -1 0 x y]: glyphs run bottom-to-top.
            x = 100
            stream.append("0 g")
            for line in p["text_rot90"]:
                stream.append("BT /F1 14 Tf 0 1 -1 0 %d 100 Tm (%s) Tj ET" % (x, line))
                x += 20
        data = "\n".join(stream).encode()
        cid = add(b"<< /Length %d >>\nstream\n" % len(data) + data + b"\nendstream")
        rot = " /Rotate %d" % p["rotate"] if p.get("rotate") else ""
        pid = add(
            (
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d]%s "
                "/Resources << /Font << /F1 %d 0 R >> %s >> /Contents %d 0 R%s >>"
                % (p["w"], p["h"], rot, font, xobj, cid, annots)
            ).encode()
        )
        page_ids.append(pid)
    objs[0] = b"<< /Type /Catalog /Pages 2 0 R >>"
    kids = " ".join("%d 0 R" % i for i in page_ids)
    objs[1] = ("<< /Type /Pages /Kids [%s] /Count %d >>" % (kids, len(page_ids))).encode()
    info_ref = ""
    if info:
        body = " ".join("/%s (%s)" % (k, v) for k, v in info.items())
        iid = add(("<< %s >>" % body).encode())
        info_ref = " /Info %d 0 R" % iid

    out = bytearray(b"%PDF-1.7\n")
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    for off in offsets:
        out += b"%010d 00000 n \n" % off
    out += (
        "trailer\n<< /Size %d /Root 1 0 R%s >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, info_ref, xref)
    ).encode()
    return bytes(out)


def make_scans(out: Path, source_pdf: Path):
    try:
        from PIL import Image, ImageFilter
    except ImportError:
        print("Pillow missing: skipping scan PDFs")
        return
    png_prefix = out / "_scan_src"
    try:
        subprocess.run(["pdftoppm", "-r", "150", "-f", "1", "-l", "1", "-png", str(source_pdf), str(png_prefix)], check=True)
    except (OSError, subprocess.CalledProcessError):
        print("pdftoppm missing: skipping scan PDFs")
        return
    src = next(out.glob("_scan_src*.png"))
    base = Image.open(src).convert("L")

    def save(name, img):
        img.convert("RGB").save(out / name, "PDF", resolution=150.0)

    save("scan_sharp.pdf", base)
    save("scan_blurry.pdf", base.filter(ImageFilter.GaussianBlur(3)))
    # Faint: squash contrast into 190..255 (washed-out photocopy).
    save("scan_faint.pdf", base.point(lambda v: 190 + v * 65 // 255))
    save("scan_sideways.pdf", base.rotate(90, expand=True))
    src.unlink()


def main(outdir, n_many=500):
    out = Path(outdir)
    out.mkdir(parents=True, exist_ok=True)
    A4 = dict(w=595, h=842)
    text = lambda *l: dict(**A4, text=list(l))

    mixed = [
        text("Annual Report 2025", "", "Revenue grew 12% year over year.", "Q3 revenue: 4.2M USD"),
        dict(**A4),  # blank
        dict(**A4, text=["Chart page"], rect=True),
        dict(**A4, rotate=90, text=["This page has /Rotate 90"]),
        dict(**A4),  # blank
    ]
    (out / "mixed.pdf").write_bytes(
        build_pdf(
            mixed,
            info={
                "Title": "Mixed Test Document",
                "Author": "Test Author",
                "Subject": "Pipeline test",
                "Keywords": "pdf, rust, test",
                "Producer": "gen_pdfs.py",
                "CreationDate": "D:20250101120000Z",
            },
        )
    )

    # Structure: heading, paragraph, list, subheading; plus an image page and
    # sideways text drawn inside an upright page.
    structured = [
        dict(
            **A4,
            text=[
                (28, "Quarterly Results"),
                (8, ""),
                (18, "Summary"),
                (12, "Revenue grew strongly in the third quarter and"),
                (12, "margins improved across all regions."),
                (12, ""),
                (12, "- Revenue: 4.2M USD"),
                (12, "- Costs: 3.1M USD"),
                (12, "- Profit: 1.1M USD"),
                (12, ""),
                (12, "The outlook for the next quarter remains positive."),
            ],
        ),
        dict(**A4, text=[(18, "Figure page"), (12, "Image below.")], image=True),
        dict(**A4, text_rot90=["Sideways text line one", "Sideways text line two"]),
        dict(**A4, rotate=180, text=[(18, "Upside down via Rotate"), (12, "Body text on a 180 page.")]),
        dict(**A4, rotate=270, text=[(18, "Rotate 270 page"), (12, "Body text on a 270 page.")]),
        # Sideways glyphs on a /Rotate 90 page: the two rotations cancel (upright).
        dict(**A4, rotate=90, text_rot90=["Cancelling rotations line one", "Cancelling rotations line two"]),
    ]
    (out / "structured.pdf").write_bytes(build_pdf(structured, info={"Title": "Structured Test"}))

    (out / "huge_page.pdf").write_bytes(build_pdf([dict(w=6000, h=8000, text=["Huge page"], rect=True)]))

    many = []
    for i in range(n_many):
        if i % 10 == 9:
            many.append(dict(**A4))
        else:
            many.append(text("Page %d of %d" % (i + 1, n_many), *["Lorem ipsum dolor sit amet %d" % k for k in range(30)]))
    (out / "many_pages.pdf").write_bytes(build_pdf(many))

    # Scan-like (image only) PDFs from a text-dense page.
    dense = dict(**A4, text=[(14, "Scanned page line %d: the quick brown fox jumps over the lazy dog" % i) for i in range(40)])
    (out / "_dense.pdf").write_bytes(build_pdf([dense]))
    make_scans(out, out / "_dense.pdf")
    (out / "_dense.pdf").unlink()
    print("generated in", out)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "testdata", int(sys.argv[2]) if len(sys.argv) > 2 else 500)
