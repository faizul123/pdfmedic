#!/usr/bin/env python3
"""Score OCR output (`pdfpipe run --ocr --format json`) against ocr_expected.json by CER.

CER = Levenshtein(pred, truth) / len(truth), on NFKC-normalised text with all whitespace removed
(spacing and line-wrap differences are not recognition errors). Prints per-page rows and
aggregates by variant and by source language.
"""
import json, sys, unicodedata, collections

def norm(s):
    return "".join(unicodedata.normalize("NFKC", s).split())

def lev(a, b, band=400):
    if len(a) < len(b): a, b = b, a            # a longer
    n, m = len(a), len(b)
    if m == 0: return n
    prev = list(range(m + 1))
    for i in range(1, n + 1):
        cur = [i] + [0] * m
        lo, hi = max(1, i - band), min(m, i + band)
        if lo > 1: cur[lo - 1] = 10 ** 9
        ai = a[i - 1]
        for j in range(lo, hi + 1):
            c = prev[j - 1] + (ai != b[j - 1])
            d = prev[j] + 1
            e = cur[j - 1] + 1
            cur[j] = c if c < d and c < e else (d if d < e else e)
        prev = cur
    return prev[m]

res = json.load(open(sys.argv[1])); exp = json.load(open(sys.argv[2]))
pages = res["pages"]
rows = []
for e in exp:
    p = pages[e["page"] - 1]
    text = " ".join(b["text"] for b in p["blocks"] if b["kind"] != "figure")
    pred, truth = norm(text), norm(e["truth"])
    cer = lev(pred, truth) / max(1, len(truth))
    o = p.get("ocr") or {}
    q = p.get("quality") or {}
    want_rot = {"rot90": 270, "rot180": 180, "rot270": 90}.get(e["variant"], 0)
    rows.append(dict(page=e["page"], src=e["source"], var=e["variant"], cer=min(cer, 1.0), conf=o.get("mean_confidence", 0),
                     models=",".join(sorted(o.get("models", {}))), rot=o.get("rotated_cw"), want_rot=want_rot,
                     ms=(o.get("render_ms", 0) + o.get("detect_ms", 0) + o.get("recognize_ms", 0)),
                     sharp=q.get("sharpness", 0), lines=o.get("lines", 0)))
print(f"{'pg':>2} {'src':<6} {'variant':<10} {'CER':>6} {'conf':>5} {'models':<18} {'rot':>3}/{'want':<4} {'ms':>6} {'sharp':>6}")
for r in rows:
    flag = "" if r["rot"] == r["want_rot"] else "  <- ROTATION"
    print(f"{r['page']:>2} {r['src']:<6} {r['var']:<10} {r['cer']*100:5.1f}% {r['conf']:5.2f} {r['models']:<18} {str(r['rot']):>3}/{r['want_rot']:<4} {r['ms']:6.0f} {r['sharp']:6.0f}{flag}")
def agg(key):
    g = collections.defaultdict(list)
    for r in rows: g[r[key]].append(r)
    print(f"\nby {key}:")
    for k, v in g.items():
        print(f"  {k:<10} n={len(v):>2}  mean CER {sum(x['cer'] for x in v)/len(v)*100:5.1f}%  worst {max(x['cer'] for x in v)*100:5.1f}%  mean {sum(x['ms'] for x in v)/len(v):6.0f} ms")
agg("var"); agg("src")
print(f"\noverall: mean CER {sum(r['cer'] for r in rows)/len(rows)*100:.1f}%, rotation correct on {sum(r['rot']==r['want_rot'] for r in rows)}/{len(rows)} pages")
