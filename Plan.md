# Plan: Rust PDF Document-Intelligence Pipeline

## 1. Goal
Evaluate Rust PDF libraries on each required capability by building a **chain of
stages** (a pipeline). Each stage performs one capability. The pipeline output
feeds a document-intelligence / Q&A system, so it must preserve **structure,
provenance (page + bbox) and confidence**.

Source requirements: `Requirement.md`.

## 2. Capabilities (each = one stage)
| # | Capability | Stage crate |
|---|---|---|
| 7 | Extract metadata | `stage-metadata` |
| 1 | Large PDF pages to images | `stage-render` |
| 4 | Blank page detection | `stage-blank` |
| 2 | Quality checks (blur, visibility, readability) | `stage-quality` |
| 3 | Auto orientation / layout correction | `stage-orient` |
| 5 | Extract text, images, charts, tables (+ OCR fallback) | `stage-extract` |
| 8 | Bounding boxes for extracted elements | `stage-bbox` (types in core) |
| 6 | Multi-page table grouping | `stage-tables-merge` |
| - | Export JSON / Markdown | `export` |

## 3. Pipeline order
```
PDF -> [metadata] -> [render] -> [blank] -> [quality] -> [orient]
    -> [extract (+OCR fallback)] -> [bbox normalize] -> [tables-merge]
    -> [exporters: JSON, Markdown]
```
Rationale:
- Metadata first: cheap; gives page count, `/Rotate`, encryption for planning.
- Render early: quality/blank/orientation work on pixels (shared low-DPI render).
- Blank before quality: avoid false alarms; skip extraction on blank pages.
- Orientation before extraction: coordinates are only correct on upright pages.
- Tables-merge last: needs per-page tables with bboxes and headers.

## 4. Architecture
Cargo workspace (`*` = implemented in M0/M1, others planned):
```
rust-workspace/
  Cargo.toml
  crates/
    pipeline-core/     * PdfBackend/PdfDoc/Stage traits, Pipeline runner,
                         Document model, BBox, metrics (RSS, CPU via /proc)
    backend-pdfium/    * PdfBackend impl on pdfium-render
    backend-mupdf/       (planned, M5)   backend-lopdf/ (planned, M5)
    stage-metadata/ *  stage-render/ *  stage-blank/ *
    stage-profile/ *   (fast triage pass, see section 15)
    stage-pixels/ *    (low-res pixel pass for flagged pages, see section 16)
    ocr-oar/ *         (OCR engine: PP-OCRv5 via oar-ocr, section 18)   stage-ocr/ * (OCR stage)
    stage-quality/ *   stage-orient/ *  stage-extract/ *   stage-tables-merge/ (M3)
    export/            * JSON + Markdown exporters
    bench-harness/       (planned, M6)
  cli/                 * `pdfpipe` binary
  libs/pdfium/         * libpdfium.so (pdfium-binaries, not committed)
  testdata/            * gen_pdfs.py generates sample PDFs
```
- **Backend abstraction:** stages talk only to `dyn PdfDoc`
  (`page_count`, `metadata`, `page_geometry`, `render_page`, `content_summary`,
  more added per milestone). A new library = one new `backend-*` crate; stages
  and exporters are untouched, so every library runs the identical pipeline.
- **Stage trait:** `run_document` (once) and `run_page` (per page);
  `PageContext` carries the rendered image and flags; `skip_rest` lets a stage
  (blank) short-circuit later stages.
- **Streaming:** pages are processed one at a time and the raster is dropped
  after each page, so memory stays flat regardless of page count.
- **Coordinates:** PDF points, origin top-left, y down (normalised for all
  backends).
- Per-stage timings, wall time, CPU time and peak RSS are recorded into
  `Document.stats` and printed by the CLI.

## 5. Library candidates
| Capability | Candidates |
|---|---|
| Render | `pdfium-render`, `mupdf`, `hayro`, `poppler-rs` |
| Metadata | `lopdf`, `pdfium-render`, `mupdf`, `pdf` |
| Text / images / bbox | `pdfium-render`, `mupdf`, `lopdf`, `pdf_oxide`, `pdf-extract` |
| Quality | `image`, `imageproc` (Laplacian variance), optional `opencv` |
| OCR fallback | **PaddleOCR (PP-OCR det + cls + rec, NOT PaddleOCR-VL)** via ONNX (`ort`) |
| Tables / charts | custom heuristics over text bboxes + ruling lines; optional ONNX layout model |

Notes: PDFium is not thread-safe by default (use `thread_safe` feature or
processes); MuPDF needs one context per thread; MuPDF is AGPL, Poppler GPL.

## 6. OCR fallback (PaddleOCR)
- Triggered only for pages with images but no usable text layer, or garbled text.
- Flow: detect (DB) -> optional angle classifier -> recognize (CTC) ->
  text + bbox + confidence; pixel coords mapped back to PDF page coords.
- Mean confidence feeds readability score.
- Models: PP-OCR ONNX, downloaded to a cache dir with checksum. Version
  (v4/v5) and languages TBD.
- Crate choice (community PP-OCR wrappers vs own `ort` pre/post-processing) to
  be verified during implementation.

## 7. Output
Document model is the single source of truth, rendered by exporters.

**JSON** (full fidelity): metadata, pages, blocks with type
(heading/paragraph/table/figure/chart/list), text, bbox, confidence, table
cells/header rows/continuation links, image refs.

**Markdown** (README style, a *view*):
- YAML front matter (title, author, pages, dates)
- `#` headings, paragraphs in reading order, lists, captions as quotes
- GFM tables; cross-page tables emitted once with a continuation note
- figures saved to `assets/` and referenced; charts get description/data table
- page markers `<!-- page N -->`
- low-confidence warnings
- `--with-provenance` adds bbox HTML comments; merged-cell tables fall back to
  HTML `<table>`

CLI: `pdfpipe run file.pdf --format json,md --out ./out`

## 8. Evaluation / scoring
Per library, per capability, record: supported?, setup effort, usage example,
and measured:
- **Speed** (wall time; criterion)
- **Memory** (peak RSS)
- **CPU** (user+sys time, utilisation)
- **Threading / concurrency** (scaling at 1,2,4,8 workers; thread-safety)
- **Accuracy** (on small set with known answers: text, tables, bboxes, blank,
  orientation)
Results go to `Scorecard.md` (feature matrix + weighted scores).

## 9. Milestones
1. **M0 Setup**: toolchain check, workspace skeleton, core types, CLI.
2. **M1 Vertical slice** (`pdfium-render`): metadata, render, blank, JSON +
   Markdown export.
3. **M2**: quality, orientation, text/image/bbox extraction.
4. **M3**: tables, multi-page merge, charts (crop + description hook).
5. **M4**: PaddleOCR fallback.
6. **M5**: additional backends (`mupdf`, `lopdf`, pure-Rust) behind features.
7. **M6**: bench-harness, test set, Scorecard.

## 10. Open questions
- Licensing tolerance (AGPL/GPL backends)?
- Sample PDFs available? Otherwise generate a test set.
- PP-OCR version and languages; model packaging.
- Chart handling: heuristic text only, or vision-model description?

## 11. Status
- [x] M0 Setup: Rust 1.100.0 via rustup, workspace, core types, CLI
- [x] M1 Vertical slice on `pdfium-render`: metadata, render (size-capped),
      blank detection, JSON + Markdown export
- [x] M2 quality (blur/contrast/readability), orientation (text layer + scan
      heuristic), text/heading/list/image extraction with bboxes, rotation-aware
      coordinates, 15 unit tests, clippy clean
- [x] M2.5 fast per-page triage (`pdfpipe profile`): text, script/language, scanned,
      images, table/chart/seal candidates with positions, route hint; 160 pages in
      ~115 ms; 14 unit tests + 16-page ground-truth document
- [x] M2.6 pixel pass for scans / raster charts / raster seals / tables in scans (section 16): 14/14
      on its ground-truth document, ~20 ms/page, 10 unit tests
- [ ] M3 tables (full extraction), multi-page merge, charts
- [x] M4 PaddleOCR (PP-OCRv5) fallback via oar-ocr + ONNX Runtime (section 18): 41-page eval set
      mean CER 1.1%, rotation 41/41, 358 MiB peak; routed from the triage
- [ ] M5 more backends (mupdf, lopdf, pure Rust)
- [ ] M6 bench-harness, real-world test set, Scorecard

## 12. How to run
```bash
. ~/.cargo/env
# PDFium binary (one-time): libs/pdfium/lib/libpdfium.so
#   from https://github.com/bblanchon/pdfium-binaries (pdfium-linux-x64.tgz)
python3 testdata/gen_pdfs.py testdata 500       # sample PDFs
cargo build --release
./target/release/pdfpipe run testdata/mixed.pdf --out out/mixed \
    --format json,md --save-images --with-provenance
```
Options: `--dpi`, `--max-dim`, `--pdfium-dir` (or env `PDFIUM_LIB_DIR`).
Triage: `./target/release/pdfpipe profile file.pdf [--pages 1-50] [--verbose] [--json out.json] [--pixels [--px-dpi 72]]`.
Pixel-pass regression: `python3 testdata/gen_pixel_pdfs.py testdata` then
`pdfpipe profile testdata/pixel_mixed.pdf --pixels --json out/pixel.json` and
`python3 testdata/check_profile.py out/pixel.json testdata/pixel_expected.json`
(add `pixels` as 3rd argument when checking `profile_expected.json` with `--pixels`).
Regression: `python3 testdata/gen_profile_pdfs.py testdata` then
`python3 testdata/check_profile.py out/profile.json testdata/profile_expected.json`.
Tests / lint: `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
`cargo fmt --all`. Test PDFs: `mixed`, `structured` (headings, list, image,
sideways and /Rotate 90/180/270 pages), `huge_page`, `many_pages`, and
image-only scans `scan_sharp|blurry|faint|sideways` (need Pillow + pdftoppm).

## 13. Findings log (feeds the Scorecard)
**pdfium-render 0.9.6**
- Works well for metadata, rendering, per-page content summary.
- **Pitfall:** `PdfPageText::all()` is `inside_rect(page_size())`. On pages with
  `/Rotate 90/270`, `page_size()` is the rotated box but glyph coordinates are
  unrotated, so most text is silently dropped (returned `""` while the text
  layer had 24 chars). Use the per-char iterator (`text.chars()`) instead.
  Any text-extraction stage must be tested on rotated pages.
- Page geometry already reflects `/Rotate` (width/height swapped), and the
  `/Rotate` value is available separately.
- Default features include `thread_safe` (calls serialised behind a lock), so
  real parallelism needs multiple processes or separate bindings: to verify in
  the concurrency benchmarks (M6).
- Metadata dates come back as raw PDF date strings (`D:2025...`); not parsed.

**Measured (4 vCPU, release build, 100 dpi analysis render)**
| Case | Result |
|---|---|
| 5-page mixed doc | 65 ms wall, 15.7 MiB peak RSS |
| 1 page 6000x8000 pt | render capped to 3072x4096 px, 104 MiB peak RSS |
| 500 pages (10% blank) | 3.8 s wall (~7.5 ms/page), 19.3 MiB peak RSS (flat), 99% of one core |
- Blank detection: 50/50 blank pages found, 0 false positives on test set.
- In the 500-page run the blank stage (2.2 s) costs more than rendering
  (1.4 s). Suspects: per-character FFI calls in `content_summary` and the
  full `to_luma8()` copy. Candidate optimisation: stop counting characters at
  the first visible one, and threshold directly on RGBA bytes.
- Current runs are single-threaded; no concurrency numbers yet.

**Known limitations of M1**
- Markdown has no text content yet (text extraction is M2).
- Blank test data is synthetic (clean vector pages); scanned pages with noise
  or a white image need real samples.

**M2 findings**
- **Glyph angle sign:** `PdfPageTextChar::angle_degrees()` is *clockwise*
  (text drawn bottom-to-top with a +90 deg CCW matrix returns 270). Page
  `/Rotate` is clockwise too: displayed direction = glyph + rotate; correction to
  upright = 360 - that. A wrong-sign version passed pages with glyph angle 0, so
  it needed a test page with sideways glyphs, and one with sideways glyphs on a
  `/Rotate 90` page (cancels to 0).
- **Bboxes on rotated pages:** pdfium reports text/object bounds in unrotated user
  space (origin bottom-left). `geometry::rect_to_display` maps to displayed
  top-left space; verified visually by overlaying boxes on renders for
  0/90/180/270 and sideways text (all tight).
- **Reading order must be computed upright:** grouping in the displayed frame
  merged a heading and its body on a `/Rotate 270` page. Extraction now rotates
  into the upright frame (orientation stage's correction), groups, then maps
  boxes back. Bboxes in the output stay in the *displayed* frame.
- **Contrast metric:** p2/p98 luma range reported 0 for sparse text pages (ink
  under 2% of pixels) and falsely flagged them. Now p98 - p0.1.
- **Sharpness calibration** (Laplacian-variance p90 over content tiles, 150 dpi
  scan rendered at 100 dpi): sigma 0.5 -> 8120, 1.0 -> 3202, 1.3 -> 1556,
  1.6 -> 792, 2.0 -> 312, 3.0 -> 47. Threshold 600 flags blur from about
  sigma 1.8. Vector text pages score 3.3k-12.9k. **Unvalidated against OCR
  accuracy**: M4 should correlate sharpness with OCR confidence and retune.
- **Orientation from pixels** cannot distinguish 90 from 270 (or 0 from 180);
  reported with `ambiguous: true` and not applied. The ink threshold is
  relative to each page's own paper/ink levels so faint scans work.
- **Scans extract as one page-sized Figure block** (image object, no text layer):
  M4 should treat a page-sized image with no text as the OCR trigger.

**Performance work (500-page run, 4 vCPU, single thread)**
| Step | Wall | Notes |
|---|---|---|
| M1 pipeline (metadata, render, blank) | 3.8 s | |
| + quality, orient, extract | 8.2 s | quality 3.7 s, blank 2.3 s |
| cache one luma copy per page, raw-slice Laplacian | 6.4 s | quality 3.7 -> 2.1 s |
| `fast_luma` (integer Rec.709) replaces `to_luma8` | 4.75 s | blank 2.3 -> 0.58 s |
- A hypothesis that per-glyph FFI calls dominated `blank` was **wrong**
  (measured 108 ms of 2259 ms); the real cost was `DynamicImage::to_luma8` on a
  ~1 MP RGBA raster (1.9 s / 450 pages). Measure before optimising.
- Output was byte-identical in all analysis fields after each optimisation.
- Peak RSS stays ~20 MiB (flat) for 500 pages.
- Remaining per-page costs: quality 4.6 ms (full-res Laplacian), render 3.2 ms.
  Candidates: render at lower DPI for quality, or SIMD Laplacian.

**Known limitations after M2**
- Single-column reading order only (multi-column pages interleave).
- Headings come from font size only (no bold/style); lists from text markers.
- Tables and charts are not detected yet (M3); no OCR (M4).
- Blank/quality thresholds tuned on synthetic and PIL-made scans, not real
  documents.
- `/Rotate` pages whose MediaBox origin is not (0,0), or that have a CropBox,
  are not handled in the bbox mapping yet.

## 14. Developer environment (VS Code + Rust language server)
| Task | Status |
|---|---|
| Install Rust via rustup (stable, 1.100.0) | done |
| rustup components: `rust-analyzer`, `rustfmt`, `clippy`, `rust-src` | done |
| `rust-toolchain.toml` pins channel + components so every machine matches | done |
| `rustfmt.toml` (max_width 110) + format-on-save in VS Code | done |
| `.vscode/extensions.json` recommends: rust-analyzer, CodeLLDB, Even Better TOML, Dependi (crates), Error Lens, Markdown All in One | done (VS Code prompts to install on open) |
| `.vscode/settings.json`: clippy as check command, inlay hints, `PDFIUM_LIB_DIR` for runnables, exclude `target/`, `out/`, `libs/` from watcher/search | done |
| `.vscode/tasks.json`: build, test, clippy, fmt check, generate test PDFs, run sample | done |
| `.vscode/launch.json`: CodeLLDB debug config for `pdfpipe` | done |
| `.gitignore` (`target/`, `out/`, `libs/`) | done |
| Install the VS Code extensions themselves | **manual**: `code` CLI is not on this machine; open the folder in VS Code and accept the recommendations, or repeat the flag per extension: `code --install-extension rust-lang.rust-analyzer --install-extension vadimcn.vscode-lldb --install-extension tamasfe.even-better-toml --install-extension fill-labs.dependi --install-extension usernamehw.errorlens --install-extension yzhang.markdown-all-in-one` |
| Verify rust-analyzer in the editor (hover types, clippy squiggles, run/debug lenses) | **manual**, after extensions install |
| Optional: `cargo-nextest`, `cargo-flamegraph`, `cargo-criterion` for the M6 benchmarks | todo (M6) |
| Optional: pre-commit / CI running `fmt --check`, `clippy -D warnings`, `test` | todo |

## 15. Fast per-page triage ("profile")
**Requirement:** for 100+ page PDFs, find per page - has text, language
(en/ar/fr/...), scanned, images, charts, tables, seals, and their positions -
very quickly, *before* full parsing. Output feeds the router (see routing scenarios).

**Design.** A separate cheap pass: no rendering, no full text extraction.
One walk over each page's objects (text, path, image) plus annotations, then a
pure classifier (`stage-profile::classify`) over the raw data. Backend contract:
`PdfDoc::page_scan -> RawPageScan`. Candidates carry a score (0..1, heuristic) and
a bbox in the same displayed top-left frame as everything else.

| Signal | How | Status |
|---|---|---|
| Has text / hidden text layer | text objects, render mode 3 = hidden | works |
| Script | Unicode blocks over a sampled text (latin, arabic, cyrillic, han...) | works |
| Language | `whatlang` on <= ~600 sampled chars; needs >= 25 letters; `reliable` flag | works; unreliable on short/numeric text, flagged `?` |
| Scanned | image coverage >= 0.85 (or >= 0.5 with no text) and < 20 visible chars | works |
| Scan + OCR layer | scanned + hidden text -> `verify_text_layer` | works |
| Images + positions | image objects, coverage | works |
| Ruled tables | thin line / cell-rect clusters, grid test | works on test set |
| Borderless tables | aligned text columns (needs cell-level text objects) | works for LibreOffice output; producer-dependent |
| Vector charts | baseline-aligned bars; multi-color touching shapes (pie/area) | works on test set |
| Seals | vector rings with saturated stroke; stamp annotations; stamp-sized square images | works on test set; square-image rule has false positives (score 0.4) |
| Blank | no text, no images, no annotations, no ink paths | exact for vector pages |
| Outlined text | no text objects but >= 150 tiny paths -> OCR | works |
| Annotation-only content | stamp/widget/ink annotations are not page objects, so counted separately | works |
| Raster charts / raster seals / tables inside scans | **not visible at object level** -> `needs_pixel_pass` | **open (M2.6)** |

**Measured (4 vCPU, release, single thread, includes open):**
| Document | Pages | Wall | Per page |
|---|---|---|---|
| mixed content (10x the 16-page test doc) | 160 | 115 ms | 0.7 ms |
| dense text (many_pages.pdf) | 500 | 171 ms | 0.34 ms |
| same 500 pages, full pipeline | 500 | 4.8 s | 9.5 ms |
Profiling is ~2% of the full pipeline's time on the 16-page doc. Peak RSS 11-23 MiB.
Parallelism is unnecessary at this speed (and pdfium-render serialises calls
anyway); if ever needed, use worker processes over page ranges (`--pages`).

**Accuracy:** 16/16 pages match ground truth on `profile_mixed.pdf` (English,
French, Arabic, bilingual, ruled + borderless tables, vector bar + pie charts,
raster chart, vector + raster seals, scan, scan with hidden text, annotation-only
stamp, blank, outlined text), and 160/160 on 10 repetitions. **Caveat:** this is a
regression suite, not an accuracy estimate: the heuristics and the test data were
both adjusted after the first failures, and every page comes from LibreOffice,
cairo, Pillow or hand-written PDFs. Real documents will be worse. Positions were
checked visually by overlaying candidate boxes on renders (tables, bars, pie,
rings, stamps, images all landed on target; boxes are tight to ink, so pad them
when cropping, and the chart box covers the bars only, not axes or labels).

**Findings**
- **`PdfPageTextObject::text()` is very slow:** ~170 us per object (5.3 ms for a
  31-object page) while every other object call is < 0.1 ms/page. The first version
  of the profiler spent 5.4 ms/page on dense pages (57% of the full pipeline!).
  Fix: estimate per-object character counts from box width / font size, and get the
  language sample and an exact "any visible text?" check from one capped
  character walk of the text page (~0.25 us per char). 5.4 -> 0.34 ms/page, results
  unchanged. Found by timing each call separately; my first guess was wrong.
- First use of a non-embedded standard font costs ~7 ms once (font init), then
  nothing. Pages with thousands of paths (outlined text) cost 2+ ms in PDFium's own
  content parsing; the profiler's walk adds ~0.1 ms.
- `whatlang`: short pages mislabel (Estonian/Dutch/Catalan for tiny English
  snippets), but its `reliable` flag marks exactly those pages. Consumers must use
  language only when `reliable` (CLI prints `xx?` otherwise). Text order for Arabic
  was fine here (logical order); other producers may emit visual order, which would
  hurt detection - untested.
- A row-gap limit based on glyph height rejected a real table (28 pt row pitch, 9 pt
  glyphs); it now scales with font size. A "bar chart" detector based on touching
  shapes missed real bars (10-30 pt gaps); it now chains rectangles on a shared
  baseline.
- LibreOffice's HTML import ignores the `border` attribute (use CSS borders);
  cairo's `arc()` adds a line from the previous point unless `new_path()` is called.
  Both silently corrupted test data first.

**Open / not done**
- Pixel pass (M2.6): low-resolution render of pages flagged `needs_pixel_pass`
  for raster seals (saturated ring blobs), raster charts (colored bars/slices), and
  table rules inside scans. Until then these are flagged, not detected.
- Real-document validation, especially other producers (Word, Chrome, InDesign,
  scanners) and non-Latin scripts beyond Arabic; Persian/Urdu vs Arabic.
- Multi-column and RTL table layouts are untested.
- Seals that are plain images with a transparent background, or in non-red/blue inks,
  are only caught by the weak square-image rule.

## 16. Pixel pass (`stage-pixels`, `pdfpipe profile --pixels`)
**Why:** object-level triage cannot see inside a raster: seals, charts and table rules in a scan
(or in an embedded image) are just "one image". Pages flagged `needs_pixel_pass` (scans, outlined
text, images >= 120x80 pt, weak stamp-sized square images) get one low-resolution render (72 dpi
default) and three detectors. On pages with live content only the image regions are analysed.
Candidates are heuristic, scored, and tagged `pixels:` in `why`.

| Detector | Method | Rejects |
|---|---|---|
| Seals | saturated-ink blobs (spread >= 60), dilated, connected components; 30-230 pt, aspect 0.7-1.45, fill 0.04-0.4, ink near 3+ sides, **empty center**; nested rings merged | solid logos (fill), signatures (aspect), photos (center not empty) |
| Raster charts | 8 px grid of colored cells, components; >= 2 dominant flat colors (top-6 of 512 bins >= 75%) **and** pixel-level uniformity (>= 85% of neighbors within 12/channel) | photos (many bins or jitter), single-color blobs |
| Tables | thin dark runs along tilted scanlines (+-1 px tolerance), skew estimated from the short pieces (+-3.5 deg, 0.1 steps), grid test shared with the vector detector in the straightened frame, box mapped back | filled regions/photos (thin test), page borders, > 80 lines |
| Weak object-level seal guesses | confirmed by a pixel seal -> replaced; not confirmed -> demoted to <= 0.25 and labelled | - |

**Measured (4 vCPU, release, single thread):**
| Case | Result |
|---|---|
| Pixel suite (14 pages: 13 scans + 1 digital) | **14/14** match ground truth |
| Original triage suite with `--pixels` | 16/16 (the raster chart on p9 is now found) |
| Skew sweep 0-4 deg, real JPEG scans | table found at all 9 angles, box within ~3 pt, angle estimate within 0.1 deg |
| Speed, 72 dpi | **19 ms/page** (render ~9-12 ms, tables ~8 ms, seals ~3 ms, charts ~1 ms) |
| 160 mixed pages, `--pixels` | 0.83 s total; pixel pass ran on 50 pages at 14 ms avg; triage alone 0.11 s |
| Resolution sweep (pixel suite) | 110 dpi 34 ms, 90 dpi 27 ms, 72 dpi 20 ms, 60 dpi 18 ms (all 14/14), **50 dpi 14 ms fails** (small table lost) |
| Peak RSS | 24-36 MiB |

**Findings**
- Negatives written first (photo, solid logo, signature, text-only scan) found three real
  false-positive modes: a photo passing the seal fill test (fixed with the empty-center test), the
  same photo producing "table rules" (fixed with the thin-line test), and a narrow-palette photo
  passing the color-bin chart test (fixed with pixel-level uniformity). A palette histogram alone
  cannot separate a narrow-palette photo from a chart.
- **Skew was the hardest part and took four attempts.** (1) a row-projection over all dark pixels
  was fooled by text (at the true 1 deg the "no skew" score won); (2) a half-resolution run-length
  sweep was unreliable; (3) scoring only the thin line pieces worked, but (4) a 0.1 deg angle error
  still let a scanline drift off a 1 px rule and lose its far end, so runs are found on a mask dilated
  1 px perpendicular to the scanline. Unit tests on synthetic pages passed while real scans failed
  twice: synthetic images are not a substitute for real JPEG scans.
- A stored "known limitation" expectation went stale when the limitation was fixed
  (`*_with_pixels` overrides in `profile_expected.json`).
- Optimisation by measurement again: the table pass was 21 ms because of four full scans; one scan
  for short pieces (long rules are the long ones among them), a pre-dilated mask (one lookup instead
  of three) and skipping ink-free rows/columns made it ~8 ms. Render (JPEG decode) is now the largest
  single cost.

**Limits / not done**
- Gray or black-only charts, and single-color bar charts, are not detected (color-based). Black or gray
  seals are only caught by the weak object-level rule.
- Skew beyond +-3.5 deg (the 4 deg case passed only because the estimate saturates at 3.5), rotated
  (90/180/270) scans (orientation stage's job first), curved/warped pages.
- Table pass finds ruled grids only; borderless tables inside scans need OCR (M4).
- Thresholds (30-230 pt seals, 8 px chart cells, 12 channel jitter) were tuned on synthetic and
  PIL-made scans; real scans (coffee stains, bleed-through, colored paper, stamps over text) are untested.
  Colored paper would make the whole page "saturated"; not handled.
- 72 dpi default is the measured floor + margin on this set; real fainter rules may need 90+.

## 17. Inventory: what is built, with which tools, and measured performance
| Crate / command | Purpose | Tests |
|---|---|---|
| `pipeline-core` | PdfDoc/Stage traits, model, geometry, metrics | 5 |
| `backend-pdfium` | PDFium via `pdfium-render` 0.9.6 (build 8165) | - |
| `stage-metadata/render/blank/quality/orient/extract` | the pipeline stages | quality 4, orient 4, extract 4 (metadata, render, blank: none) |
| `stage-profile` | fast triage, `pdfpipe profile` | 15 |
| `ocr-oar` / `stage-ocr` | OCR engine (PP-OCRv5) and stage | 3 / 3 |
| `stage-pixels` | pixel pass, `--pixels` | 10 |
| `export` | JSON + Markdown | - |
Libraries: pdfium-render 0.9.6, image 0.25.10, whatlang 0.18.0, clap 4.6.9, serde(+json), thiserror,
anyhow. `lopdf` was added early and never used. Test-data tools only: LibreOffice, pycairo, Pillow,
poppler-utils, Ghostscript. Total 50 unit tests, 0 clippy warnings, 3 ground-truth suites.

| Run (4 vCPU, single thread) | Result |
|---|---|
| Full pipeline, 500 pages | 4.87-4.90 s (9.8 ms/page), 19 MiB; quality 2075, render 1420, blank 583, extract 386, profile 142, orient 122 ms |
| Triage only, 500 dense pages | 0.17 s (0.33 ms/page), 10.8 MiB |
| Triage only, 160 mixed pages | 0.115 s (0.7 ms/page) |
| Triage + pixel pass, 160 mixed pages | 0.83 s |
| Pixel pass alone | ~19 ms per flagged page at 72 dpi |
| One 6000x8000 pt page, full pipeline | 130 ms, 105 MiB |
No concurrency numbers yet; PDFium calls are serialised by `pdfium-render`.

## 18. OCR (M4): PP-OCRv5 through `oar-ocr` + ONNX Runtime
**Decision.** Classic PaddleOCR (detection + recognition), not PaddleOCR-VL, as requested. Rust
crate `oar-ocr` 0.11 (Apache-2.0) wraps the PP-OCR ONNX models; it also ships layout, table
(SLANet, wired/wireless cell detection), formula and seal models: **relevant to M3**.
Build: `oar-ocr` default features off, `ort` with `download-binaries` + `tls-rustls` (the default
`native-tls` needs OpenSSL headers, absent here). Models: `scripts/fetch_models.sh` (~38 MB).
Run: `pdfpipe run file.pdf --ocr [--models-dir models] [--ocr-dpi 150]`.

**Flow.** triage -> pages with `route_hint == Ocr` (scans, outlined text) -> render 150 dpi ->
detect lines once -> recognizer cascade (`general` then `arabic`; only lines below 0.90 confidence
are retried) -> rotation search if the first pass is not confidently horizontal -> boxes mapped to
PDF points in the displayed frame -> grouped into headings/paragraphs -> blocks tagged
`source: ocr` with confidence; the page-sized scan figure is dropped; `Page.ocr` records engine,
mean confidence, per-model line counts, language, timings. `verify_text_layer` pages (scan + hidden
text) are not OCR'd yet.

**Evaluation** (`testdata/gen_ocr_pdfs.py` -> `ocr_eval.pdf`, 41 scan pages of known text;
`testdata/check_ocr.py` scores CER on whitespace-free NFKC text):
| Variant | Mean CER | Worst |
|---|---|---|
| clean | 0.6% | 1.3% |
| blur sigma 1.0 / 1.6 | 0.6% / 1.2% | 1.3% / 2.2% |
| blur sigma 2.4 (sharpness 59-80) | **6.3%** | 11.6% |
| JPEG q30 + noise, faint, skew 1.5 deg | 0.6-0.7% | 1.7% |
| 100 dpi / 75 dpi source | 0.6% / 0.6% | 1.3% / 2.0% |
| rotated 90 / 180 / 270 | 0.7% | 1.7% |
| by language: en / fr / ar / en+ar / dense | 0.2 / 1.1 / 2.4 / 1.1 / 0.0% | 2.3 / 5.0 / 11.6 / 1.3 / 0.0% |
Overall mean CER **1.1%**; rotation correct on **41/41** pages.

**Performance** (4 vCPU, no GPU, 2 ORT threads): English/French page ~0.5 s, Arabic ~1.2 s, dense
40-line page ~2.2 s; 41 pages 53 s wall, **358 MiB peak**, 177% CPU. Detection ~0.2 s/page, then
~50 ms per line (mobile recognizer). The 16-page mixed PDF with `--ocr`: 1.3 s, 270 MiB, OCR on
2 pages only (the scan and the outlined-text page).

**Findings (each measured)**
- **Batching hurts.** All 40 lines in one recognizer call: 3.5 s and **1.55 GB**; one line at a
  time: 2.2 s and **239 MB**. Memory-pattern on is slightly worse. First eval run used batch 8 and
  hit 3 GB peak; with batch 1 the same set peaks at 358 MiB (engine alone plateaus ~250-310 MiB, no leak).
- **Threads:** 1 = 3.7 s, 2 = 2.4 s, 4 = 2.5 s (+55% CPU time). Use 2; for throughput prefer several
  single-thread workers (not built yet; the pipeline is sequential).
- **Arabic output is in visual order** (reversed). Fixed with PaddleOCR's token-reversal rule, only on
  mostly-Arabic lines. Without it every Arabic line is unusable text.
- **Padding changes recognition.** Batches pad short lines to the widest line and that helps Arabic
  (batch 8: 0.87 vs batch 1: 0.80 confidence) but right-padding Latin took English from 0.2% to 17.8%
  CER. Fix: per-recognizer crop margins, Arabic only (none 10.4% -> 0.25: 3.7% -> **0.5: 2.4%** -> 1.0: 2.4%
  but slower). English/French unchanged. (A first `sed` of this default silently missed; caught by the
  grep count and by results matching the old setting.)
- **The `latin` recognizer never won a line** and cost a full extra pass on Arabic pages: removed from the
  default cascade (`LATIN_REC` available).
- **Arabic model reads Latin letters** (good for bilingual pages) but drops French accents.
- **Wrong-script models fail visibly:** English model on Arabic = confidence 0.00 / empty; general model
  on Arabic 0.37-0.55. Confidence does **not** reveal the general model's accent errors (0.97 on `coùts`)
  nor English/French errors under heavy blur (0.96 at 5% CER).
- **Document orientation classifier (PP-LCNet) is unreliable here**: correct on English only with margin
  0.05-0.12, wrong on French 180 deg, constant 0.26/margin 0.00 on Arabic and bilingual pages. Real dense
  pages may do better (untested). Replaced by OCR-as-oracle: if the first pass is not confidently
  *horizontal*, try the other rotations (order from our sideways heuristic) and keep the best.
- **Confidence cannot detect a sideways page**: the crop helper auto-rotates tall boxes, so a page
  rotated 90 deg was read at 0.97 confidence but in reversed line order (CER 50%). Box shape
  (share of text in horizontal boxes) is the signal.
- **OCR boxes are inflated** (~1.5x line pitch, heights vary 38-47 px for one font): grouping needs its
  own thresholds (paragraph gap 0.25 of box height, relative font-size tolerance 0.35, weighted-median
  body size). Text-layer grouping is unchanged (checked).
- **Blur threshold retuned 600 -> 150** from this data: at sharpness 454-475 OCR was still 0-2% CER;
  at 59-80 it was 5-12%. Low OCR confidence is a second readability signal (it fired only for Arabic).

**Limits / not done**
- All evaluation images are generated from clean LibreOffice pages; no real scans yet (stains,
  bleed-through, handwriting, stamps over text, columns, tables).
- Reading order is top-to-bottom, left-to-right on line boxes: multi-column pages interleave; RTL
  multi-column untested. Tables in scans are OCR'd as lines, not structure (M3; `oar-ocr` has SLANet).
- Other languages: only English, French, Arabic measured. Models for Cyrillic, Devanagari, Greek, Thai,
  Korean etc. exist but are not in the cascade; no script probe yet, so unknown scripts cost a failed
  pass per model.
- `verify_text_layer` pages and scans' hidden text are not compared with OCR.
- Sequential: no page-level parallelism (several 1-thread workers would raise throughput).
- Mobile models only (server variants are ~10x larger); accuracy on hard documents untested.
