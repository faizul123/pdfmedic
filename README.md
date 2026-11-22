# pdfmedic

A Rust pre-flight router for PDFs. It looks at every page first, cheaply, then decides what each page
needs: skip it, extract it with Rust, or send it to OCR. The result is one canonical document object
that exports to JSON or Markdown, with page numbers, bounding boxes and confidence on every block.

The CLI binary is `pdfpipe`.

> Status: working prototype. Triage, extraction, quality checks, orientation, the pixel pass and OCR are
> built and measured. Table structure, multi-page tables, other PDF backends and the benchmark scorecard
> are not done yet (see [Status](#status)). Details and every measurement live in [`Plan.md`](Plan.md).

## What it does

| Capability | How |
|---|---|
| Per-page triage in milliseconds | One pass over page objects: text, script/language, scanned, images, table/chart/seal candidates with positions, route hint |
| Pixel pass for scans and images | Low-resolution render of flagged pages: seals, raster charts, tables in scans (handles skew up to ~4 deg) |
| Text, headings, lists, images | From the PDF text layer with exact bounding boxes, in upright reading order |
| Quality checks | Blur, contrast, readability verdict per page |
| Orientation | Exact from the text layer; scans resolved by OCR retry |
| Blank page detection | Ink ratio plus text layer, annotation-aware |
| Metadata | Title, author, dates, version, page count |
| OCR fallback | PaddleOCR PP-OCRv5 (not the VL model) through ONNX Runtime, English / French / Arabic measured |
| Export | JSON (full detail) and README-style Markdown |

Pages are processed one at a time, so memory stays flat regardless of page count.

## Pipeline

```
PDF -> metadata -> profile (triage) -> [pixel pass] -> render -> blank -> quality -> orient -> extract -> [OCR] -> JSON / Markdown
```

Triage decides the route per page (`skip`, `rust_only`, `ocr`, `verify_text_layer`). OCR runs only on pages
routed to it; text-layer pages keep their exact text.

## Quick start

Requirements: Linux, a C compiler, Rust (the toolchain is pinned in `rust-toolchain.toml`).

```bash
# 1. PDFium (PDF engine): download libpdfium.so into libs/pdfium/lib/
#    from https://github.com/bblanchon/pdfium-binaries  (pdfium-linux-x64.tgz)
mkdir -p libs/pdfium && tar -xzf pdfium-linux-x64.tgz -C libs/pdfium

# 2. Build
cargo build --release

# 3. OCR models (only needed for --ocr), ~38 MB
scripts/fetch_models.sh
```

## Usage

```bash
# Full pipeline: JSON + Markdown next to each other in out/
./target/release/pdfpipe run file.pdf --out out --format json,md

# Everything: pixel pass, OCR, saved page images, bbox comments in the Markdown
./target/release/pdfpipe run file.pdf --out out --format json,md \
    --pixels --ocr --save-images --with-provenance

# Fast triage only (no rendering): what is on each page, and where
./target/release/pdfpipe profile file.pdf --verbose
./target/release/pdfpipe profile file.pdf --pages 1-50 --pixels --json profile.json

# A whole folder
for f in invoices/*.pdf; do
  ./target/release/pdfpipe run "$f" --out out/invoices --format json,md --ocr --pixels
done
```

Useful options: `--dpi`, `--max-dim` (render size cap), `--pdfium-dir` (or `PDFIUM_LIB_DIR`),
`--models-dir` (or `OCR_MODELS_DIR`), `--ocr-dpi`, `--px-dpi`.

## Measured performance

4 vCPU, no GPU, release build, single document at a time. Test documents are generated; see the caveats below.

| Run | Result |
|---|---|
| Triage, 500 dense-text pages | 0.17 s (0.33 ms/page), 11 MiB |
| Triage, 160 mixed pages | 0.115 s |
| Full pipeline, 500 pages | 4.9 s (~10 ms/page), 19 MiB |
| Pixel pass | ~20 ms per flagged page |
| OCR, English / French page | ~0.5 s; Arabic ~1.2 s; 40-line page ~2.2 s |
| OCR evaluation set (41 scan pages) | mean character error rate 1.1%, rotation recovered on 41/41, 358 MiB peak |

## Repository layout

```
cli/                  pdfpipe binary
crates/
  pipeline-core/      traits, document model, geometry, metrics
  backend-pdfium/     PDF backend on pdfium-render
  stage-metadata/ stage-render/ stage-blank/ stage-quality/ stage-orient/ stage-extract/
  stage-profile/      fast triage
  stage-pixels/       pixel pass
  ocr-oar/            OCR engine (PP-OCRv5 via oar-ocr + ONNX Runtime)
  stage-ocr/          OCR stage
  export/             JSON and Markdown exporters
scripts/              model download
testdata/             generators for test PDFs and ground-truth checkers
Plan.md               design, findings, measurements, open items
```

PDF access goes through one `PdfDoc` trait, so other PDF libraries can be added as new backends without
touching the stages.

## Tests

```bash
cargo test --workspace
cargo clippy --workspace --all-targets

# Ground-truth suites (generate the PDFs first, see Plan.md section 12)
python3 testdata/check_profile.py out/profile.json testdata/profile_expected.json
python3 testdata/check_ocr.py out/ocr_eval/ocr_eval.json testdata/ocr_expected.json
```

## Status

Done: triage, pixel pass, extraction, quality, orientation, blank detection, metadata, OCR, JSON/Markdown export.

Not done:
- Table structure (rows and columns), multi-page table merging, chart extraction (tables are located, not parsed)
- Other PDF backends (MuPDF, lopdf, a pure-Rust one) and the benchmark scorecard
- A route-logging stage, a VL-model fallback, and an offset index for LLM field extraction
- Parallel page processing

## Limitations

- All accuracy numbers come from generated test documents. Real scans (stains, handwriting, stamps over
  text, multi-column pages) are untested, and thresholds were tuned on synthetic data.
- Language detection is unreliable on short or number-heavy pages; trust it only when `reliable` is true.
- Reading order is single-column; multi-column pages interleave.
- OCR is measured for English, French and Arabic only.

## Notes

- PDFium and the OCR models are not stored in the repository (`libs/`, `models/` are git-ignored).
- The ONNX Runtime binaries are fetched at build time over rustls, so no system OpenSSL is needed.
- OCR models are PaddleOCR PP-OCRv5 (Apache-2.0), obtained via the `oar-ocr` project's releases.
