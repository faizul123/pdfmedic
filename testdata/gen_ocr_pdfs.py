#!/usr/bin/env python3
"""Build testdata/ocr_eval.pdf + ocr_expected.json: scans of known text under degradations.

Sources are the English / French / Arabic / bilingual pages of profile_mixed.pdf rendered at 150 dpi
(testdata/ocr/page{1..4}-*.png) and a dense English page. Each variant is saved as a JPEG scan page.
"""
import glob, io, json, random, sys
from pathlib import Path
from PIL import Image, ImageFilter

sys.path.insert(0, str(Path(__file__).parent))
from gen_pdfs import build_pdf  # noqa: E402

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else "testdata")
random.seed(11)
EN = ("The quarterly report shows steady growth across all regions. Revenue increased while operating "
      "costs remained stable, and the board expects the trend to continue into the next financial year.")
FR = ("Le rapport trimestriel montre une croissance régulière dans toutes les régions. Les revenus ont "
      "augmenté tandis que les coûts sont restés stables, et le conseil prévoit que cette tendance se poursuivra.")
AR = ("يظهر التقرير ربع السنوي نموا مستقرا في جميع المناطق. ارتفعت الإيرادات بينما بقيت التكاليف "
      "التشغيلية مستقرة ويتوقع المجلس استمرار هذا الاتجاه خلال السنة المالية القادمة.")
SOURCES = {
    "en": (glob.glob(str(OUT / "ocr/page1-*.png"))[0], f"Quarterly Report {EN} {EN}"),
    "fr": (glob.glob(str(OUT / "ocr/page2-*.png"))[0], f"Rapport trimestriel {FR} {FR}"),
    "ar": (glob.glob(str(OUT / "ocr/page3-*.png"))[0], f"التقرير الفصلي {AR} {AR}"),
    "en+ar": (glob.glob(str(OUT / "ocr/page4-*.png"))[0], f"{EN} {AR}"),
    "dense": (glob.glob(str(OUT / "ocr/dense-*.png"))[0],
              " ".join(f"Scanned page line {i}: the quick brown fox jumps over the lazy dog" for i in range(40))),
}


def noisy(im, q=30):
    n = Image.effect_noise(im.size, 18).convert("RGB")
    return Image.blend(im, n, 0.12)


VARIANTS = {
    "clean": lambda im: im,
    "blur1.0": lambda im: im.filter(ImageFilter.GaussianBlur(1.0)),
    "blur1.6": lambda im: im.filter(ImageFilter.GaussianBlur(1.6)),
    "blur2.4": lambda im: im.filter(ImageFilter.GaussianBlur(2.4)),
    "noise_q30": noisy,
    "faint": lambda im: im.point(lambda v: 170 + v * 85 // 255),
    "skew1.5": lambda im: im.rotate(1.5, resample=Image.BICUBIC, fillcolor="white"),
    "rot90": lambda im: im.rotate(-90, expand=True),    # content rotated 90 deg clockwise
    "rot180": lambda im: im.rotate(180),
    "rot270": lambda im: im.rotate(-270, expand=True),
    "res100dpi": lambda im: im.resize((int(im.width / 1.5), int(im.height / 1.5)), Image.LANCZOS),
    "res75dpi": lambda im: im.resize((int(im.width / 2), int(im.height / 2)), Image.LANCZOS),
}
PLAN = {  # which variants for which source (dense: only the speed/accuracy-relevant ones)
    "en": list(VARIANTS), "fr": list(VARIANTS), "ar": list(VARIANTS), "en+ar": ["clean", "res100dpi"],
    "dense": ["clean", "res100dpi", "res75dpi"],
}

pages, expected = [], []
for src, (png, truth) in SOURCES.items():
    base = Image.open(png).convert("RGB")
    for var in PLAN[src]:
        im = VARIANTS[var](base).convert("RGB")
        b = io.BytesIO()
        im.save(b, "JPEG", quality=30 if var == "noise_q30" else 88)
        w, h = (595, 842) if im.width < im.height else (842, 595)
        pages.append(dict(w=w, h=h, jpeg=(b.getvalue(), im.width, im.height, "RGB")))
        expected.append({"page": len(pages), "source": src, "variant": var, "truth": truth})

(OUT / "ocr_eval.pdf").write_bytes(build_pdf(pages))
(OUT / "ocr_expected.json").write_text(json.dumps(expected, ensure_ascii=False, indent=1))
print(f"wrote {OUT / 'ocr_eval.pdf'} ({len(pages)} pages) and ocr_expected.json")
