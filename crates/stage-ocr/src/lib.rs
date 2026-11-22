//! OCR stage: runs an [`OcrEngine`] on pages the profile routes to OCR and merges the result
//! into the document model as blocks tagged `source: ocr` with a confidence.
//!
//! Steps per page: render at `dpi` -> OCR (if the first pass looks bad, retry the other
//! rotations, most likely first, keep the best) -> map boxes to PDF points in the *displayed*
//! frame -> group lines into headings/paragraphs/lists with the same grouping as the text-layer
//! extractor -> drop the page-sized image block the scan produced -> record `OcrInfo`.

use pipeline_core::geometry::rotate_bbox_cw;
use pipeline_core::image::{imageops, RgbImage};
use pipeline_core::{
    BBox, Block, BlockKind, BlockSource, OcrEngine, OcrInfo, OcrOutput, PageContext, PdfDoc, Result,
    RouteHint, Stage, TextLine,
};
use std::collections::BTreeMap;
use std::time::Instant;

pub struct OcrStage {
    pub engine: Box<dyn OcrEngine>,
    /// Render resolution for OCR (points -> pixels = dpi / 72).
    pub dpi: f32,
    pub max_dim: u32,
    /// A first pass at or above this mean confidence (with enough text) is accepted as is.
    pub good_conf: f32,
    /// Run on every page that has no text blocks, not only on `route_hint == Ocr`.
    pub force: bool,
}

impl OcrStage {
    pub fn new(engine: Box<dyn OcrEngine>) -> Self {
        Self { engine, dpi: 150.0, max_dim: 3200, good_conf: 0.85, force: false }
    }
}

fn rotate_cw(img: &RgbImage, deg: u16) -> RgbImage {
    match deg % 360 {
        90 => imageops::rotate90(img),
        180 => imageops::rotate180(img),
        270 => imageops::rotate270(img),
        _ => img.clone(),
    }
}

fn nonws(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// (non-whitespace chars, length-weighted mean confidence)
fn quality(o: &OcrOutput) -> (usize, f32) {
    let chars: usize = o.lines.iter().map(|l| nonws(&l.text)).sum();
    if chars == 0 {
        return (0, 0.0);
    }
    let w: f32 = o.lines.iter().map(|l| l.confidence * nonws(&l.text) as f32).sum();
    (chars, w / chars as f32)
}

/// Share of recognized characters sitting in horizontal (wider than tall) line boxes. Sideways
/// pages are read fine by the recognizer (tall crops are auto-rotated), with high confidence, so
/// confidence alone cannot tell they are sideways; the box shape can.
fn horizontal_share(o: &OcrOutput) -> f32 {
    let total: usize = o.lines.iter().map(|l| nonws(&l.text)).sum();
    if total == 0 {
        return 0.0;
    }
    let h: usize = o.lines.iter().filter(|l| l.bbox.width() >= l.bbox.height()).map(|l| nonws(&l.text)).sum();
    h as f32 / total as f32
}

/// OCR with rotation search. Returns (clockwise rotation applied, output, rotated image).
fn best_orientation(
    engine: &dyn OcrEngine,
    img: &RgbImage,
    sideways_hint: bool,
    good: f32,
) -> Result<(u16, OcrOutput, RgbImage)> {
    let first = engine.recognize(img)?;
    let (chars, mean) = quality(&first);
    // No lines at all: nothing to rotate toward (blank page, photo). Enough confidently read,
    // horizontal text: done.
    if first.lines.is_empty() || (chars >= 15 && mean >= good && horizontal_share(&first) >= 0.6) {
        return Ok((0, first, img.clone()));
    }
    let order: [u16; 3] = if sideways_hint { [270, 90, 180] } else { [180, 90, 270] };
    // Score = confident characters in horizontal lines: the right rotation reads the most text
    // well *and* lays it out horizontally.
    let score = |o: &OcrOutput| {
        let (c, m) = quality(o);
        c as f32 * m * horizontal_share(o)
    };
    let mut best = (0u16, first, img.clone());
    let mut best_score = score(&best.1);
    for rot in order {
        let r = rotate_cw(img, rot);
        let o = engine.recognize(&r)?;
        let s = score(&o);
        let (c, m) = quality(&o);
        let ok = c >= 15 && m >= good && horizontal_share(&o) >= 0.6;
        if s > best_score {
            best = (rot, o, r);
            best_score = s;
        }
        if ok {
            break; // found a clearly readable, horizontal orientation
        }
    }
    Ok(best)
}

impl Stage for OcrStage {
    fn name(&self) -> &'static str {
        "ocr"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        let wanted = match &ctx.page.profile {
            Some(p) => p.route_hint == RouteHint::Ocr,
            None => false,
        } || (self.force && ctx.page.blocks.iter().all(|b| b.kind == BlockKind::Figure));
        if !wanted {
            return Ok(());
        }
        let idx = ctx.page.number - 1;
        let t = Instant::now();
        let img = doc.render_page(idx, self.dpi, self.max_dim)?.to_rgb8();
        let render_ms = t.elapsed().as_secs_f64() * 1000.0;

        let (pw, ph) = (ctx.page.geometry.width_pt, ctx.page.geometry.height_pt);
        let k = img.width() as f32 / pw; // pixels per point
        let sideways = ctx
            .page
            .orientation
            .as_ref()
            .map(|o| o.ambiguous || o.correction_cw % 180 == 90)
            .unwrap_or(false);
        let (rot, out, rimg) = best_orientation(self.engine.as_ref(), &img, sideways, self.good_conf)?;
        let (uw, uh) = (rimg.width() as f32 / k, rimg.height() as f32 / k); // upright frame, points

        // Lines in the upright frame, in points.
        let lines: Vec<TextLine> = out
            .lines
            .iter()
            .map(|l| TextLine {
                text: l.text.clone(),
                bbox: BBox { x0: l.bbox.x0 / k, y0: l.bbox.y0 / k, x1: l.bbox.x1 / k, y1: l.bbox.y1 / k },
                // Glyph height is ~0.75 of the line box; only relative sizes matter for headings.
                font_size: (l.bbox.height() / k) * 0.75,
            })
            .collect();
        let mut blocks = stage_extract::group_blocks_with(
            &lines,
            stage_extract::OCR_PARA_GAP,
            stage_extract::OCR_SIZE_TOL,
        );
        for b in &mut blocks {
            b.source = BlockSource::Ocr;
            let inside: Vec<(f32, usize)> = out
                .lines
                .iter()
                .zip(&lines)
                .filter(|(_, tl)| {
                    let (cx, cy) = ((tl.bbox.x0 + tl.bbox.x1) / 2.0, (tl.bbox.y0 + tl.bbox.y1) / 2.0);
                    cx >= b.bbox.x0 && cx <= b.bbox.x1 && cy >= b.bbox.y0 && cy <= b.bbox.y1
                })
                .map(|(l, _)| (l.confidence, nonws(&l.text).max(1)))
                .collect();
            let w: usize = inside.iter().map(|x| x.1).sum();
            b.confidence = (w > 0).then(|| inside.iter().map(|(c, n)| c * *n as f32).sum::<f32>() / w as f32);
            b.bbox = rotate_bbox_cw(&b.bbox, uw, uh, (360 - rot) % 360);
        }

        // The scan itself came through extraction as a page-sized figure: drop it.
        let page_area = pw * ph;
        ctx.page.blocks.retain(|b| {
            !(b.kind == BlockKind::Figure && b.bbox.width() * b.bbox.height() >= 0.85 * page_area)
        });
        let mut all: Vec<Block> = blocks;
        all.append(&mut ctx.page.blocks);
        ctx.page.blocks = all;

        let (_, mean) = quality(&out);
        let mut models: BTreeMap<String, usize> = BTreeMap::new();
        for l in &out.lines {
            *models.entry(l.model.clone()).or_default() += 1;
        }
        let text: String = out.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
        ctx.page.ocr = Some(OcrInfo {
            engine: self.engine.name(),
            dpi: self.dpi,
            lines: out.lines.len(),
            mean_confidence: mean,
            low_confidence_lines: out.lines.iter().filter(|l| l.confidence < 0.8).count(),
            models,
            language: stage_profile::detect_language(&text),
            render_ms,
            detect_ms: out.detect_ms,
            recognize_ms: out.recognize_ms,
            rotated_cw: rot,
        });
        // Orientation is now resolved by reading, not guessed.
        if let Some(o) = ctx.page.orientation.as_mut() {
            if rot != 0 || o.ambiguous {
                o.correction_cw = rot;
                o.ambiguous = false;
                o.method = "ocr_retry".into();
                o.confidence = mean;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::OcrLine;

    /// Engine that only "reads" when the image has been rotated to the expected orientation
    /// (identified by image width: portrait = upright for this fake).
    struct Fake;
    impl OcrEngine for Fake {
        fn name(&self) -> String {
            "fake".into()
        }
        fn recognize(&self, img: &RgbImage) -> Result<OcrOutput> {
            let upright =
                img.get_pixel(0, 0).0[0] == 255 && img.get_pixel(img.width() - 1, img.height() - 1).0[0] == 0;
            let c = if upright { 0.97 } else { 0.2 };
            Ok(OcrOutput {
                lines: vec![OcrLine {
                    text: "this is some readable text here".into(),
                    bbox: BBox { x0: 0.0, y0: 0.0, x1: 10.0, y1: 5.0 },
                    confidence: c,
                    model: "fake".into(),
                }],
                ..Default::default()
            })
        }
    }

    fn marked(w: u32, h: u32) -> RgbImage {
        let mut im = RgbImage::from_pixel(w, h, pipeline_core::image::Rgb([128, 128, 128]));
        im.put_pixel(0, 0, pipeline_core::image::Rgb([255, 255, 255]));
        im.put_pixel(w - 1, h - 1, pipeline_core::image::Rgb([0, 0, 0]));
        im
    }

    /// Reads every orientation with high confidence (like the real recognizer on sideways
    /// pages) but reports vertical boxes unless the image is upright.
    struct ConfidentButVertical;
    impl OcrEngine for ConfidentButVertical {
        fn name(&self) -> String {
            "fake2".into()
        }
        fn recognize(&self, img: &RgbImage) -> Result<OcrOutput> {
            let upright =
                img.get_pixel(0, 0).0[0] == 255 && img.get_pixel(img.width() - 1, img.height() - 1).0[0] == 0;
            let bbox = if upright {
                BBox { x0: 0.0, y0: 0.0, x1: 100.0, y1: 10.0 }
            } else {
                BBox { x0: 0.0, y0: 0.0, x1: 10.0, y1: 100.0 }
            };
            Ok(OcrOutput {
                lines: vec![OcrLine {
                    text: "this is some readable text here".into(),
                    bbox,
                    confidence: 0.97,
                    model: "f".into(),
                }],
                ..Default::default()
            })
        }
    }

    #[test]
    fn confident_sideways_reading_is_not_accepted() {
        // Confidence is 0.97 in every orientation; only the box shape reveals the rotation.
        let page = rotate_cw(&marked(20, 30), 90);
        let (rot, _, _) = best_orientation(&ConfidentButVertical, &page, true, 0.85).unwrap();
        assert_eq!(rot, 270);
    }

    #[test]
    fn upright_page_needs_no_retry() {
        let (rot, _, _) = best_orientation(&Fake, &marked(20, 30), false, 0.85).unwrap();
        assert_eq!(rot, 0);
    }

    #[test]
    fn rotated_pages_are_found_by_retry() {
        for k in [90u16, 180, 270] {
            // page rotated by k clockwise; the stage must rotate it back by (360 - k)
            let page = rotate_cw(&marked(20, 30), k);
            let (rot, _, back) = best_orientation(&Fake, &page, k % 180 == 90, 0.85).unwrap();
            assert_eq!(rot, (360 - k) % 360, "page rotated {k}");
            assert_eq!((back.width(), back.height()), (20, 30));
        }
    }
}
