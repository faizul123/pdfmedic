//! Stage 4: blank page detection.
//!
//! Hybrid: a page is blank when the render has (almost) no ink AND the text
//! layer has no visible characters. Pages that only hold a white image are
//! still reported blank, with `image_objects` recorded for transparency.

use pipeline_core::{BlankInfo, PageContext, PdfDoc, Result, Stage};

pub struct BlankStage {
    /// Pixel luma below this counts as ink (0..255).
    pub ink_luma: u8,
    /// Ink ratio at or below this is "empty" (default 0.05%: tolerates specks).
    pub max_ink_ratio: f32,
}

impl Default for BlankStage {
    fn default() -> Self {
        Self { ink_luma: 200, max_ink_ratio: 0.0005 }
    }
}

impl Stage for BlankStage {
    fn name(&self) -> &'static str {
        "blank"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        let summary = doc.content_summary(ctx.page.number - 1)?;
        let ink_ratio = match ctx.gray() {
            Some(gray) => {
                let raw = gray.as_raw();
                let ink = raw.iter().filter(|&&p| p < self.ink_luma).count();
                ink as f32 / raw.len().max(1) as f32
            }
            // No render available: fall back to the content summary only.
            None => {
                if summary.text_chars == 0 && summary.image_objects == 0 && summary.path_objects == 0 {
                    0.0
                } else {
                    1.0
                }
            }
        };
        let is_blank = ink_ratio <= self.max_ink_ratio && summary.text_chars == 0;
        ctx.page.blank = Some(BlankInfo {
            is_blank,
            ink_ratio,
            text_chars: summary.text_chars,
            image_objects: summary.image_objects,
        });
        ctx.content = Some(summary);
        if is_blank {
            ctx.skip_rest = true;
        }
        Ok(())
    }
}
