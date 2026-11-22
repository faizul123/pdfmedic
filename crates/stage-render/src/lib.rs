//! Stage 1: render pages to images (large PDFs: one page at a time, size-capped).

use pipeline_core::{PageContext, PdfDoc, RenderInfo, Result, Stage};
use std::path::PathBuf;

pub struct RenderStage {
    /// Analysis render resolution. Low DPI is enough for blank/quality checks.
    pub dpi: f32,
    /// Cap on the longest side in pixels (protects memory on huge pages).
    pub max_dim: u32,
    /// If set, each page is also written as `page-NNNN.png` under `<dir>/assets`.
    pub save_dir: Option<PathBuf>,
}

impl Default for RenderStage {
    fn default() -> Self {
        Self { dpi: 100.0, max_dim: 4096, save_dir: None }
    }
}

impl Stage for RenderStage {
    fn name(&self) -> &'static str {
        "render"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        let img = doc.render_page(ctx.page.number - 1, self.dpi, self.max_dim)?;
        let mut info =
            RenderInfo { dpi: self.dpi, width_px: img.width(), height_px: img.height(), image_path: None };
        if let Some(dir) = &self.save_dir {
            let assets = dir.join("assets");
            std::fs::create_dir_all(&assets)?;
            let rel = format!("assets/page-{:04}.png", ctx.page.number);
            img.save(dir.join(&rel)).map_err(|e| pipeline_core::Error::Other(e.to_string()))?;
            info.image_path = Some(rel);
        }
        ctx.page.render = Some(info);
        ctx.set_image(Some(img));
        Ok(())
    }
}
