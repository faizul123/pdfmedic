#!/usr/bin/env bash
# Download the PP-OCRv5 ONNX models and dictionaries used by `pdfpipe run --ocr` into ./models.
# Source: https://github.com/GreatV/oar-ocr/releases (v0.3.0), PaddleOCR models, Apache-2.0.
set -euo pipefail
DIR="${1:-models}"
BASE="https://github.com/GreatV/oar-ocr/releases/download/v0.3.0"
mkdir -p "$DIR"
FILES=(
  pp-ocrv5_mobile_det.onnx            # text detection (4.6 MiB)
  pp-ocrv5_mobile_rec.onnx            # general recognizer: English, French, Chinese, ... (16 MiB)
  ppocrv5_dict.txt
  arabic_pp-ocrv5_mobile_rec.onnx     # Arabic (+ Latin letters) recognizer (7.7 MiB)
  ppocrv5_arabic_dict.txt
)
OPTIONAL=(                            # not in the default cascade; see Plan.md section 18
  latin_pp-ocrv5_mobile_rec.onnx ppocrv5_latin_dict.txt
  en_pp-ocrv5_mobile_rec.onnx ppocrv5_en_dict.txt
  pp-lcnet_x1_0_doc_ori.onnx          # page orientation classifier: unreliable on light-text pages
)
for f in "${FILES[@]}"; do f="${f%% *}"; [ -s "$DIR/$f" ] || { echo "fetching $f"; curl -sSfL -o "$DIR/$f" "$BASE/$f"; }; done
if [ "${WITH_OPTIONAL:-0}" = "1" ]; then
  for f in "${OPTIONAL[@]}"; do f="${f%% *}"; [ -s "$DIR/$f" ] || { echo "fetching $f"; curl -sSfL -o "$DIR/$f" "$BASE/$f"; }; done
fi
echo "models in $DIR:"; ls -la "$DIR" | awk 'NR>1 {print "  " $5 "\t" $9}'
