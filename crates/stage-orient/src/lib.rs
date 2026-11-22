//! Stage 3: orientation detection / correction.
//!
//! 1. If the page has a text layer, the dominant glyph direction (relative to
//!    what is displayed) gives the correction exactly: method `text_layer`.
//! 2. Otherwise (scans) use a projection-profile heuristic on the render: upright
//!    text has strongly banded rows (lines vs gaps) while its columns are flat.
//!    This separates upright from *sideways* text only. 0 vs 180 and 90 vs 270
//!    cannot be told apart this way, so sideways text is reported as
//!    `ambiguous` and not rotated; the OCR angle classifier (M4) resolves it.
//!
//! Bboxes produced by later stages stay in the *displayed* (uncorrected) space;
//! `correction_cw` tells a consumer how to rotate the page for reading.

use pipeline_core::image::{DynamicImage, GrayImage};
use pipeline_core::{OrientationInfo, PageContext, PdfDoc, Result, Stage};

pub struct OrientStage {
    /// Minimum share of glyphs agreeing on a direction to trust the text layer.
    pub min_text_share: f32,
    /// Rotate `ctx.image` when the correction is certain.
    pub apply: bool,
}

impl Default for OrientStage {
    fn default() -> Self {
        Self { min_text_share: 0.6, apply: true }
    }
}

/// Share of row-profile variance in total (row + column) variance inside the
/// ink bounding box. ~1 = text lines run horizontally, ~0 = vertically.
/// Returns `None` when there is too little ink to judge.
pub fn horizontal_text_score(gray: &GrayImage) -> Option<f32> {
    let (w, h) = gray.dimensions();
    // Downscale so the profile is cheap and noise-tolerant.
    let scale = (800.0 / w.max(h) as f32).min(1.0);
    let small = if scale < 1.0 {
        pipeline_core::image::imageops::resize(
            gray,
            ((w as f32 * scale) as u32).max(1),
            ((h as f32 * scale) as u32).max(1),
            pipeline_core::image::imageops::FilterType::Triangle,
        )
    } else {
        gray.clone()
    };
    let (w, h) = small.dimensions();
    // Ink threshold relative to this page's own paper and ink levels, so faint
    // scans (ink only slightly darker than paper) still work.
    let mut hist = [0u64; 256];
    for p in small.as_raw() {
        hist[*p as usize] += 1;
    }
    let total = small.as_raw().len() as u64;
    let pct = |q: f64| {
        let target = (total as f64 * q) as u64;
        let mut acc = 0;
        for (v, c) in hist.iter().enumerate() {
            acc += c;
            if acc > target {
                return v as i32;
            }
        }
        255
    };
    let (paper, darkest) = (pct(0.98), pct(0.001));
    if paper - darkest < 25 {
        return None; // no visible ink at all
    }
    let thr = ((paper + darkest) / 2) as u8;
    let ink = |x: u32, y: u32| small.get_pixel(x, y).0[0] < thr;

    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    let mut count = 0u64;
    for y in 0..h {
        for x in 0..w {
            if ink(x, y) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
                count += 1;
            }
        }
    }
    if count < 200 || x1 <= x0 + 8 || y1 <= y0 + 8 {
        return None;
    }
    let (cw, ch) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    let mut rows = vec![0f64; ch];
    let mut cols = vec![0f64; cw];
    for y in y0..=y1 {
        for x in x0..=x1 {
            if ink(x, y) {
                rows[(y - y0) as usize] += 1.0;
                cols[(x - x0) as usize] += 1.0;
            }
        }
    }
    // Normalise to fractions so row and column variances are comparable.
    let var = |v: &[f64], len: f64| {
        let f: Vec<f64> = v.iter().map(|c| c / len).collect();
        let m = f.iter().sum::<f64>() / f.len() as f64;
        f.iter().map(|x| (x - m).powi(2)).sum::<f64>() / f.len() as f64
    };
    let (vr, vc) = (var(&rows, cw as f64), var(&cols, ch as f64));
    if vr + vc <= 0.0 {
        return None;
    }
    Some((vr / (vr + vc)) as f32)
}

impl OrientStage {
    fn classify_image(gray: &GrayImage) -> (u16, f32, bool) {
        match horizontal_text_score(gray) {
            Some(s) if s >= 0.6 => (0, s, false),
            Some(s) if s <= 0.4 => (90, 1.0 - s, true),
            _ => (0, 0.0, false),
        }
    }
}

impl Stage for OrientStage {
    fn name(&self) -> &'static str {
        "orient"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        let mut info = OrientationInfo {
            page_rotate: ctx.page.geometry.rotation,
            correction_cw: 0,
            method: "none".into(),
            confidence: 0.0,
            ambiguous: false,
            applied: false,
        };
        match doc.text_orientation(ctx.page.number - 1)? {
            Some((corr, share)) if share >= self.min_text_share => {
                info.method = "text_layer".into();
                info.correction_cw = corr;
                info.confidence = share;
            }
            _ => {
                if let Some(gray) = ctx.gray() {
                    let (corr, conf, ambiguous) = Self::classify_image(gray);
                    if conf > 0.0 {
                        info.method = "image_projection".into();
                        info.correction_cw = corr;
                        info.confidence = conf;
                        info.ambiguous = ambiguous;
                    }
                }
            }
        }
        if self.apply && info.correction_cw != 0 && !info.ambiguous {
            if let Some(img) = ctx.image.take() {
                ctx.set_image(Some(rotate_cw(img, info.correction_cw)));
                info.applied = true;
            }
        }
        ctx.page.orientation = Some(info);
        Ok(())
    }
}

pub fn rotate_cw(img: DynamicImage, deg: u16) -> DynamicImage {
    match deg % 360 {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::image::{imageops, Luma};

    fn text_page(w: u32, h: u32) -> GrayImage {
        let mut img = GrayImage::from_pixel(w, h, Luma([255]));
        for line in 0..((h - 80) / 24) {
            let y = 40 + line * 24;
            for x in (40..w - 40).step_by(9) {
                for dy in 0..10 {
                    for dx in 0..5 {
                        img.put_pixel(x + dx, y + dy, Luma([0]));
                    }
                }
            }
        }
        img
    }

    #[test]
    fn upright_text_scores_high_sideways_low() {
        let up = text_page(600, 800);
        let side = imageops::rotate90(&up);
        assert!(horizontal_text_score(&up).unwrap() > 0.6, "{:?}", horizontal_text_score(&up));
        assert!(horizontal_text_score(&side).unwrap() < 0.4, "{:?}", horizontal_text_score(&side));
    }

    #[test]
    fn faint_text_still_oriented() {
        let mut up = text_page(600, 800);
        for p in up.pixels_mut() {
            p.0[0] = 200 + p.0[0] / 5; // ink 200, paper 251
        }
        let side = imageops::rotate90(&up);
        assert!(horizontal_text_score(&up).unwrap() > 0.6);
        assert!(horizontal_text_score(&side).unwrap() < 0.4);
    }

    #[test]
    fn nearly_blank_gives_no_opinion() {
        assert!(horizontal_text_score(&GrayImage::from_pixel(300, 300, Luma([255]))).is_none());
    }

    #[test]
    fn rotate_cw_swaps_dimensions() {
        let img = DynamicImage::new_luma8(10, 20);
        assert_eq!(rotate_cw(img.clone(), 90).width(), 20);
        assert_eq!(rotate_cw(img, 180).width(), 10);
    }
}
