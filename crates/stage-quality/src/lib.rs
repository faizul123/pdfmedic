//! Stage 2: image-quality checks (blur, visibility/contrast, readability).
//!
//! All measures run on the analysis render, so thresholds assume the pipeline's
//! default 100 dpi render. They are heuristics, calibrated on the test set in
//! `testdata/` (see Plan.md findings); tune them for your own documents.
//!
//! * **Sharpness**: variance of the 4-neighbour Laplacian per 64x64 tile; only
//!   tiles that contain content (local luma range >= 30) count, and the score is
//!   the 90th percentile. A sharp page scores high in its text tiles no matter
//!   how much white margin it has; a blurred page is low everywhere.
//! * **Contrast / visibility**: paper-to-ink luma range, p98 - p0.1.
//! * **Readability**: `0.6 * clamp(sharpness / sharp_good) + 0.4 * clamp(contrast / 160)`.

use pipeline_core::image::GrayImage;
use pipeline_core::{PageContext, PdfDoc, QualityInfo, Result, Stage, Verdict};

pub struct QualityStage {
    /// Sharpness below this is flagged blurry. Calibrated against OCR (Plan.md section 18): at
    /// sharpness 454-475 (blur sigma 1.6) PP-OCR still reads English/French/Arabic at 0-2% CER;
    /// at 59-80 (sigma 2.4) errors reach 5-12%. 150 sits between the two. Sharpness depends on
    /// the page content and the 100 dpi analysis render, so treat it as a default to tune.
    pub blur_threshold: f32,
    /// Sharpness regarded as fully sharp for the readability score.
    pub sharp_good: f32,
    /// Luma range below this is flagged low contrast.
    pub min_contrast: f32,
}

impl Default for QualityStage {
    fn default() -> Self {
        Self { blur_threshold: 150.0, sharp_good: 3000.0, min_contrast: 90.0 }
    }
}

const TILE: u32 = 64;

/// 90th percentile of per-tile Laplacian variance over tiles with content.
pub fn sharpness(gray: &GrayImage) -> f32 {
    let (w, h) = gray.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let raw = gray.as_raw();
    let stride = w as usize;
    let px = |x: u32, y: u32| raw[y as usize * stride + x as usize] as f32;
    let mut scores = Vec::new();
    let mut ty = 1;
    while ty + 2 < h {
        let mut tx = 1;
        while tx + 2 < w {
            let (x1, y1) = ((tx + TILE).min(w - 1), (ty + TILE).min(h - 1));
            let (mut sum, mut sum2, mut n) = (0.0f64, 0.0f64, 0.0f64);
            let (mut lo, mut hi) = (255.0f32, 0.0f32);
            for y in ty..y1 {
                for x in tx..x1 {
                    let c = px(x, y);
                    lo = lo.min(c);
                    hi = hi.max(c);
                    let lap = 4.0 * c - px(x - 1, y) - px(x + 1, y) - px(x, y - 1) - px(x, y + 1);
                    sum += lap as f64;
                    sum2 += (lap * lap) as f64;
                    n += 1.0;
                }
            }
            if n > 0.0 && hi - lo >= 30.0 {
                let mean = sum / n;
                scores.push(((sum2 / n) - mean * mean) as f32);
            }
            tx += TILE;
        }
        ty += TILE;
    }
    if scores.is_empty() {
        return 0.0;
    }
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
    scores[((scores.len() - 1) as f32 * 0.9).round() as usize]
}

/// (paper-to-ink range p98 - p0.1, mean) of luma. The ink end uses a very low
/// percentile so sparse text (well under 2% of pixels) still registers.
pub fn contrast_and_brightness(gray: &GrayImage) -> (f32, f32) {
    let mut hist = [0u64; 256];
    let mut sum = 0u64;
    for p in gray.as_raw() {
        hist[*p as usize] += 1;
        sum += *p as u64;
    }
    let total = gray.as_raw().len().max(1) as u64;
    let pct = |q: f64| -> f32 {
        let target = (total as f64 * q) as u64;
        let mut acc = 0;
        for (v, c) in hist.iter().enumerate() {
            acc += c;
            if acc > target {
                return v as f32;
            }
        }
        255.0
    };
    (pct(0.98) - pct(0.001), sum as f32 / total as f32)
}

impl QualityStage {
    pub fn assess(&self, gray: &GrayImage) -> QualityInfo {
        let sharp = sharpness(gray);
        let (contrast, brightness) = contrast_and_brightness(gray);
        let is_blurry = sharp < self.blur_threshold;
        let is_low_contrast = contrast < self.min_contrast;
        let readability =
            0.6 * (sharp / self.sharp_good).clamp(0.0, 1.0) + 0.4 * (contrast / 160.0).clamp(0.0, 1.0);
        let verdict = if readability >= 0.7 && !is_blurry && !is_low_contrast {
            Verdict::Good
        } else if readability >= 0.4 {
            Verdict::Fair
        } else {
            Verdict::Poor
        };
        let mut notes = Vec::new();
        if is_blurry {
            notes.push(format!("blurry (sharpness {sharp:.0} < {:.0})", self.blur_threshold));
        }
        if is_low_contrast {
            notes.push(format!("low contrast (range {contrast:.0} < {:.0})", self.min_contrast));
        }
        if brightness < 60.0 {
            notes.push("very dark page".into());
        }
        QualityInfo {
            sharpness: sharp,
            contrast,
            brightness,
            is_blurry,
            is_low_contrast,
            readability,
            verdict,
            notes,
        }
    }
}

impl Stage for QualityStage {
    fn name(&self) -> &'static str {
        "quality"
    }

    fn run_page(&self, _doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        if let Some(gray) = ctx.gray() {
            let q = self.assess(gray);
            ctx.page.quality = Some(q);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::image::{imageops, Luma};

    /// White page with black "text rows" (stripes of small blocks).
    fn text_page(w: u32, h: u32) -> GrayImage {
        let mut img = GrayImage::from_pixel(w, h, Luma([255]));
        for line in 0..(h / 20) {
            let y = 10 + line * 20;
            for x in (20..w - 20).step_by(7) {
                for dy in 0..8 {
                    for dx in 0..4 {
                        if y + dy < h && x + dx < w {
                            img.put_pixel(x + dx, y + dy, Luma([0]));
                        }
                    }
                }
            }
        }
        img
    }

    #[test]
    fn blur_lowers_sharpness() {
        let sharp = text_page(400, 400);
        let blurred = imageops::blur(&sharp, 2.5);
        assert!(sharpness(&sharp) > 5.0 * sharpness(&blurred));
    }

    #[test]
    fn white_margins_do_not_dilute_sharpness() {
        let a = text_page(300, 300);
        let mut b = GrayImage::from_pixel(900, 900, Luma([255]));
        imageops::overlay(&mut b, &a, 300, 300);
        let (sa, sb) = (sharpness(&a), sharpness(&b));
        assert!((sa - sb).abs() / sa < 0.35, "{sa} vs {sb}");
    }

    #[test]
    fn faint_page_flagged_low_contrast() {
        let mut img = text_page(300, 300);
        for p in img.pixels_mut() {
            p.0[0] = 200 + p.0[0] / 8; // squash to 200..231
        }
        let q = QualityStage::default().assess(&img);
        assert!(q.is_low_contrast, "{q:?}");
        assert_ne!(q.verdict, Verdict::Good);
    }

    #[test]
    fn blank_page_has_zero_sharpness() {
        assert_eq!(sharpness(&GrayImage::from_pixel(200, 200, Luma([255]))), 0.0);
    }
}
