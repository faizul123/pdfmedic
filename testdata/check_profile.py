#!/usr/bin/env python3
"""Compare `pdfpipe profile --json` output with profile_expected.json."""
import json, sys
import re
snake = lambda x: re.sub(r"(?<!^)(?=[A-Z])", "_", x).lower() if isinstance(x, str) else x
prof = {r["page"]: r["profile"] for r in json.load(open(sys.argv[1]))}
exp = json.load(open(sys.argv[2]))
pixels = len(sys.argv) > 3 and sys.argv[3] == "pixels"  # apply *_with_pixels overrides
if pixels:
    for e in exp:
        for k in list(e):
            if k.endswith("_with_pixels"):
                e[k[:-len("_with_pixels")]] = e[k]
ok = bad = 0
for e in exp:
    p = prof[e["page"]]
    got = {
        "language": p["language"]["code"] if p["language"] else None,
        "scan": p["scan"], "route": p["route_hint"],
        "tables": len(p["table_candidates"]), "charts": len(p["chart_candidates"]),
        "seals": len(p["seal_candidates"]), "pixel_pass": p["needs_pixel_pass"],
        "blank": p["likely_blank"], "scripts": [s for s, _ in p["scripts"][:2]],
    }
    problems = []
    for k in ("language", "scan", "route", "tables", "charts", "seals", "blank"):
        if k in e and snake(e[k]) != snake(got[k]):
            problems.append(f"{k}: expected {e[k]!r} got {got[k]!r}")
    if "pixel_pass" in e and e["pixel_pass"] and not got["pixel_pass"]:
        problems.append("pixel_pass: expected flag")
    if "scripts" in e and not all(s in got["scripts"] for s in e["scripts"]):
        problems.append(f"scripts: expected {e['scripts']} got {got['scripts']}")
    status = "ok  " if not problems else "FAIL"
    if problems: bad += 1
    else: ok += 1
    note = f"  (known limit: {e['known_miss']})" if problems and e.get("known_miss") else ""
    print(f"{status} p{e['page']:<2} {e['label']:<26} {'; '.join(problems)}{note}")
print(f"\n{ok} ok, {bad} mismatching of {len(exp)}")
