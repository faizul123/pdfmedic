//! OCR backend on `oar-ocr` (PaddleOCR PP-OCRv5 models through ONNX Runtime).
//!
//! Pipeline: detect text lines once -> crop each line (perspective-corrected) -> run a
//! cascade of recognizers, retrying only the lines whose confidence is still below
//! `accept_conf` with the next model, keeping the best result per line.
//!
//! Known behaviours of the PP-OCRv5 recognizers (measured, see Plan.md section 18):
//! * the Arabic model emits characters in *visual* order, so Arabic lines are reordered
//!   to logical order here;
//! * a recognizer fed the wrong script returns low confidence (0.0-0.55), which is what the
//!   cascade relies on;
//! * confidence does not reveal accent errors of the general model (0.97 on `coùts`).

use oar_ocr::core::config::OrtSessionConfig;
use oar_ocr::predictors::{DocumentOrientationPredictor, TextDetectionPredictor, TextRecognitionPredictor};
use oar_ocr::utils::BBoxCrop;
use pipeline_core::image::RgbImage;
use pipeline_core::{BBox, Error, OcrEngine, OcrLine, OcrOutput, OrientationGuess, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn oe<E: std::fmt::Display>(e: E) -> Error {
    Error::Backend(format!("ocr: {e}"))
}

/// Tuning overrides for experiments and operations: `OCR_REC_BATCH`, and per recognizer
/// `OCR_PAD_LEFT_<NAME>` / `OCR_PAD_RIGHT_<NAME>` (e.g. `OCR_PAD_LEFT_ARABIC`).
fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A recognizer: model file + character dictionary.
#[derive(Debug, Clone)]
pub struct RecSpec {
    pub name: &'static str,
    pub model: &'static str,
    pub dict: &'static str,
    /// White margins added to each crop for *this* recognizer, as fractions of the crop width.
    /// The recognizers pad short lines to the widest line of a batch and that changes results:
    /// Arabic gains, Latin is hurt (right padding took English from 0.2% to 17.8% CER), so the
    /// margins are per model.
    pub pad_left: f32,
    pub pad_right: f32,
}

/// Default cascade. `general` first (English/French/Chinese...), `arabic` for what it cannot read.
/// The `latin` model is available as [`LATIN_REC`] but is not in the default cascade: on the
/// evaluation set it never won a line and cost a full extra pass on every Arabic page.
pub const DEFAULT_RECS: &[RecSpec] = &[
    RecSpec {
        name: "general",
        model: "pp-ocrv5_mobile_rec.onnx",
        dict: "ppocrv5_dict.txt",
        pad_left: 0.0,
        pad_right: 0.0,
    },
    RecSpec {
        name: "arabic",
        model: "arabic_pp-ocrv5_mobile_rec.onnx",
        dict: "ppocrv5_arabic_dict.txt",
        // Measured on the Arabic eval pages: none 10.4% CER, 0.25 -> 3.7%, 0.5 -> 2.4%, 1.0 -> 2.4%
        // (slower); see Plan.md section 18.
        pad_left: 0.5,
        pad_right: 0.5,
    },
];

pub const LATIN_REC: RecSpec = RecSpec {
    name: "latin",
    model: "latin_pp-ocrv5_mobile_rec.onnx",
    dict: "ppocrv5_latin_dict.txt",
    pad_left: 0.0,
    pad_right: 0.0,
};

#[derive(Debug, Clone)]
pub struct OarConfig {
    pub models_dir: PathBuf,
    pub det_model: String,
    /// Optional whole-page orientation classifier. Off by default: measured unreliable on
    /// light-text pages (see Plan.md section 18); the OCR stage uses confidence-based retries.
    pub doc_orientation_model: Option<String>,
    /// Recognizers in cascade order. Missing model files are skipped.
    pub recs: Vec<RecSpec>,
    /// A line at or above this confidence is accepted without trying further models.
    pub accept_conf: f32,
    /// Crops per recognizer call (sorted by width first). Measured on a 40-line page: batch 1 = 2.2 s /
    /// 239 MB, batch 64 = 3.5 s / 1.55 GB; keep it at 1.
    pub rec_batch: usize,
    /// ONNX Runtime memory-pattern optimisation. Off by default: with dynamic input widths it
    /// caches an allocation plan per shape.
    pub memory_pattern: bool,
    /// ONNX Runtime intra-op threads. Measured: 1 = 3.7 s, 2 = 2.4 s, 4 = 2.5 s (55% more CPU time).
    pub intra_threads: Option<usize>,
}

impl OarConfig {
    pub fn new(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_dir: models_dir.into(),
            det_model: "pp-ocrv5_mobile_det.onnx".into(),
            doc_orientation_model: None,
            recs: DEFAULT_RECS.to_vec(),
            accept_conf: 0.90,
            rec_batch: env_or("OCR_REC_BATCH", 1usize),
            memory_pattern: false,
            intra_threads: Some(2),
        }
    }
}

struct Rec {
    name: &'static str,
    pad: (f32, f32),
    pred: TextRecognitionPredictor,
}

pub struct OarEngine {
    rec_batch: usize,
    det: TextDetectionPredictor,
    recs: Vec<Rec>,
    doc_ori: Option<DocumentOrientationPredictor>,
    accept_conf: f32,
}

impl OarEngine {
    pub fn new(cfg: &OarConfig) -> Result<Self> {
        let path = |f: &str| cfg.models_dir.join(f);
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let det_path = path(&cfg.det_model);
        if !det_path.exists() {
            return Err(Error::Backend(format!("detection model not found: {}", det_path.display())));
        }
        let ort = || {
            let mut c = OrtSessionConfig::new().with_memory_pattern(cfg.memory_pattern);
            if let Some(n) = cfg.intra_threads {
                c = c.with_intra_threads(n);
            }
            c
        };
        let det = TextDetectionPredictor::builder()
            .with_ort_config(ort())
            .build(s(&det_path).as_str())
            .map_err(oe)?;
        let mut recs = Vec::new();
        for spec in &cfg.recs {
            let (m, d) = (path(spec.model), path(spec.dict));
            if !m.exists() || !d.exists() {
                continue;
            }
            let pred = TextRecognitionPredictor::builder()
                .with_ort_config(ort())
                .score_threshold(0.0)
                .dict_path(&d)
                .build(s(&m).as_str())
                .map_err(oe)?;
            recs.push(Rec {
                name: spec.name,
                pad: (
                    env_or(&format!("OCR_PAD_LEFT_{}", spec.name.to_uppercase()), spec.pad_left),
                    env_or(&format!("OCR_PAD_RIGHT_{}", spec.name.to_uppercase()), spec.pad_right),
                ),
                pred,
            });
        }
        if recs.is_empty() {
            return Err(Error::Backend(format!(
                "no recognition models found in {}",
                cfg.models_dir.display()
            )));
        }
        let doc_ori = match &cfg.doc_orientation_model {
            Some(f) if path(f).exists() => Some(
                DocumentOrientationPredictor::builder().topk(4).build(s(&path(f)).as_str()).map_err(oe)?,
            ),
            _ => None,
        };
        Ok(Self { rec_batch: cfg.rec_batch.max(1), det, recs, doc_ori, accept_conf: cfg.accept_conf })
    }

    pub fn recognizer_names(&self) -> Vec<&'static str> {
        self.recs.iter().map(|r| r.name).collect()
    }
}

/// Add white margins (fractions of the crop width) to the left and right.
fn pad_crop(c: RgbImage, left: f32, right: f32) -> RgbImage {
    if left <= 0.0 && right <= 0.0 {
        return c;
    }
    let (l, r) = ((c.width() as f32 * left) as u32, (c.width() as f32 * right) as u32);
    let mut out =
        RgbImage::from_pixel(c.width() + l + r, c.height(), pipeline_core::image::Rgb([255, 255, 255]));
    pipeline_core::image::imageops::overlay(&mut out, &c, l as i64, 0);
    out
}

fn arabic_letters_share(text: &str) -> f32 {
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return 0.0;
    }
    let ar = letters
        .iter()
        .filter(|&&c| matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF))
        .count();
    ar as f32 / letters.len() as f32
}

/// The PP-OCR Arabic recognizer returns characters left-to-right in *visual* order. Reverse
/// the token order to get logical order; runs of ASCII letters, digits and `:*./%+-` and
/// spaces stay intact (same rule as PaddleOCR's own `pred_reverse`). Only applied to lines
/// that are mostly Arabic, because it would scramble punctuation in Latin lines.
pub fn arabic_visual_to_logical(text: &str) -> String {
    if arabic_letters_share(text) < 0.3 {
        return text.to_string();
    }
    let is_run =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | ':' | '*' | '.' | '/' | '%' | '+' | '-');
    let mut tokens: Vec<String> = Vec::new();
    let mut run = String::new();
    for c in text.chars() {
        if is_run(c) {
            run.push(c);
        } else {
            if !run.is_empty() {
                tokens.push(std::mem::take(&mut run));
            }
            tokens.push(c.to_string());
        }
    }
    if !run.is_empty() {
        tokens.push(run);
    }
    tokens.reverse();
    tokens.concat()
}

impl OcrEngine for OarEngine {
    fn name(&self) -> String {
        format!("oar-ocr/pp-ocrv5[{}]", self.recognizer_names().join(">"))
    }

    fn recognize(&self, img: &RgbImage) -> Result<OcrOutput> {
        let t = Instant::now();
        let det = self.det.predict(vec![img.clone()]).map_err(oe)?;
        let mut dets: Vec<_> = det.detections.into_iter().next().unwrap_or_default();
        // Reading order: top to bottom, then left to right.
        dets.sort_by(|a, b| {
            let (ay, by) = (a.bbox.y_min() + a.bbox.y_max(), b.bbox.y_min() + b.bbox.y_max());
            ay.partial_cmp(&by).unwrap().then(a.bbox.x_min().partial_cmp(&b.bbox.x_min()).unwrap())
        });
        let boxes: Vec<_> = dets.iter().map(|d| d.bbox.clone()).collect();
        let crops: Vec<Option<RgbImage>> =
            BBoxCrop::batch_crop_rotated_bounding_boxes(img, &boxes).into_iter().map(|r| r.ok()).collect();
        let detect_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        // best[i] = (text, confidence, recognizer)
        let mut best: Vec<(String, f32, &'static str)> = vec![(String::new(), -1.0, ""); boxes.len()];
        for rec in &self.recs {
            let mut todo: Vec<usize> =
                (0..boxes.len()).filter(|&i| crops[i].is_some() && best[i].1 < self.accept_conf).collect();
            if todo.is_empty() {
                break;
            }
            // Similar widths together: less padding, and a bounded batch keeps memory flat.
            todo.sort_by_key(|&i| crops[i].as_ref().map(|c| c.width()).unwrap_or(0));
            for chunk in todo.chunks(self.rec_batch) {
                let batch: Vec<RgbImage> = chunk
                    .iter()
                    .map(|&i| pad_crop(crops[i].clone().unwrap(), rec.pad.0, rec.pad.1))
                    .collect();
                let out = rec.pred.predict(batch).map_err(oe)?;
                for (k, &i) in chunk.iter().enumerate() {
                    let (text, score) = (out.texts[k].clone(), out.scores[k]);
                    if !text.trim().is_empty() && score > best[i].1 {
                        best[i] = (text, score, rec.name);
                    }
                }
            }
        }
        let recognize_ms = t.elapsed().as_secs_f64() * 1000.0;

        let mut lines = Vec::new();
        for (i, b) in boxes.iter().enumerate() {
            let (text, conf, model) = &best[i];
            if *conf < 0.0 || text.trim().is_empty() {
                continue;
            }
            let text = if *model == "arabic" { arabic_visual_to_logical(text) } else { text.clone() };
            lines.push(OcrLine {
                text,
                bbox: BBox { x0: b.x_min(), y0: b.y_min(), x1: b.x_max(), y1: b.y_max() },
                confidence: *conf,
                model: (*model).to_string(),
            });
        }
        Ok(OcrOutput { lines, detect_ms, recognize_ms })
    }

    fn page_orientation(&self, img: &RgbImage) -> Result<Option<OrientationGuess>> {
        let Some(p) = &self.doc_ori else { return Ok(None) };
        let out = p.predict(vec![img.clone()]).map_err(oe)?;
        let Some(all) = out.orientations.first() else { return Ok(None) };
        let mut ranked: Vec<&_> = all.iter().collect();
        ranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        let Some(top) = ranked.first() else { return Ok(None) };
        let second = ranked.get(1).map(|c| c.score).unwrap_or(0.0);
        // Labels name the rotation the page *has*; the correction is the inverse turn.
        let has: u16 = top.label.trim().trim_end_matches('°').parse().unwrap_or(0);
        Ok(Some(OrientationGuess {
            correction_cw: (360 - has % 360) % 360,
            confidence: top.score,
            margin: top.score - second,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_heading_reordered() {
        // "التقرير الفصلي" as the Arabic model emits it (visual order)
        assert_eq!(arabic_visual_to_logical("يلصفلا ريرقتلا"), "التقرير الفصلي");
    }

    #[test]
    fn latin_lines_untouched_even_with_punctuation() {
        let t = "Hello, world: it works.";
        assert_eq!(arabic_visual_to_logical(t), t);
    }

    #[test]
    fn digits_and_latin_runs_keep_their_order_inside_arabic() {
        // Visual order of "في 2025-03 مرحبا": reversed tokens, digits intact.
        let visual = "ابحرم 2025-03 يف";
        assert_eq!(arabic_visual_to_logical(visual), "في 2025-03 مرحبا");
    }
}
