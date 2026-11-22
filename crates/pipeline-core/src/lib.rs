//! Core types for the PDF document-intelligence pipeline.
//!
//! * [`PdfBackend`] / [`PdfDoc`]: abstraction over a PDF library, so every stage
//!   can run on any backend (pdfium, mupdf, lopdf, ...) for fair comparison.
//! * [`Stage`]: one pipeline capability (metadata, render, blank, ...).
//! * [`Document`]: the document model (single source of truth) that exporters
//!   turn into JSON / Markdown.
//! * [`Pipeline`]: runs stages page by page (streaming, bounded memory) and
//!   records per-stage timings.

pub mod geometry;
pub mod metrics;
pub mod model;
pub mod ocr;
pub mod profile;

pub use model::*;
pub use ocr::*;
pub use profile::*;

pub use image;
use image::DynamicImage;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backend error: {0}")]
    Backend(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("stage `{stage}` failed on page {page}: {msg}")]
    Stage { stage: &'static str, page: usize, msg: String },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Cheap per-page facts straight from the PDF structure (no rendering).
#[derive(Debug, Clone, Default)]
pub struct PageContentSummary {
    /// Non-whitespace characters in the text layer.
    pub text_chars: usize,
    pub image_objects: usize,
    pub path_objects: usize,
}

/// One line/run of text from the text layer, in displayed (post-`/Rotate`)
/// top-left-origin point coordinates.
#[derive(Debug, Clone)]
pub struct TextLine {
    pub text: String,
    pub bbox: BBox,
    pub font_size: f32,
}

/// An image object placed on a page.
pub struct ImageObject {
    pub bbox: BBox,
    pub width_px: u32,
    pub height_px: u32,
    /// Decoded pixels, only when requested.
    pub image: Option<DynamicImage>,
}

/// A PDF library. Opening a document borrows the backend.
pub trait PdfBackend {
    fn name(&self) -> &'static str;
    fn open<'a>(&'a self, path: &Path) -> Result<Box<dyn PdfDoc + 'a>>;
}

/// An opened document. Page indexes are 0-based.
pub trait PdfDoc {
    fn page_count(&self) -> usize;
    fn metadata(&self) -> Result<Metadata>;
    fn page_geometry(&self, page: usize) -> Result<PageGeometry>;
    /// Render a page. `dpi` sets scale (72 dpi = 1 px per point); the longest
    /// side is capped at `max_dim` pixels so huge pages cannot exhaust memory.
    fn render_page(&self, page: usize, dpi: f32, max_dim: u32) -> Result<DynamicImage>;
    fn content_summary(&self, page: usize) -> Result<PageContentSummary>;
    /// Text runs with bboxes (displayed orientation, top-left origin, points).
    fn page_text_lines(&self, page: usize) -> Result<Vec<TextLine>>;
    /// Image objects with bboxes; decodes pixels only if `with_pixels`.
    fn page_images(&self, page: usize, with_pixels: bool) -> Result<Vec<ImageObject>>;
    /// Dominant text direction relative to the *displayed* page, as the extra
    /// clockwise rotation (0/90/180/270) that would make it upright, with the
    /// share of characters supporting it. `None` if the page has no text layer.
    fn text_orientation(&self, page: usize) -> Result<Option<(u16, f32)>>;
    /// One cheap walk over the page's objects (no render, no full text
    /// extraction): raw material for the fast triage profile.
    fn page_scan(&self, page: usize, opts: &ScanOptions) -> Result<RawPageScan>;
}

/// Mutable state passed through the stages for one page.
pub struct PageContext {
    pub page: Page,
    /// Rendered image, present only after a render stage. Dropped when the
    /// page is finished so memory stays bounded.
    pub image: Option<DynamicImage>,
    /// Cached luma copy of `image` (computed once, shared by stages).
    gray: Option<image::GrayImage>,
    pub content: Option<PageContentSummary>,
    /// When set, later stages skip this page (e.g. blank page).
    pub skip_rest: bool,
}

/// Luma (Rec.709 weights, integer math) of an image. Several times faster than
/// `DynamicImage::to_luma8` on RGBA/RGB rasters (see Plan.md findings); values
/// differ from it by at most one level.
pub fn fast_luma(img: &DynamicImage) -> image::GrayImage {
    let (w, h) = (img.width(), img.height());
    let conv = |r: u8, g: u8, b: u8| ((54 * r as u32 + 183 * g as u32 + 19 * b as u32) >> 8) as u8;
    let buf: Vec<u8> = match img {
        DynamicImage::ImageRgba8(b) => {
            b.as_raw().as_chunks::<4>().0.iter().map(|p| conv(p[0], p[1], p[2])).collect()
        }
        DynamicImage::ImageRgb8(b) => {
            b.as_raw().as_chunks::<3>().0.iter().map(|p| conv(p[0], p[1], p[2])).collect()
        }
        other => return other.to_luma8(),
    };
    image::GrayImage::from_raw(w, h, buf).expect("buffer size matches dimensions")
}

impl PageContext {
    pub fn new(page: Page) -> Self {
        Self { page, image: None, gray: None, content: None, skip_rest: false }
    }

    /// Replace the page raster (invalidates the cached luma copy).
    pub fn set_image(&mut self, img: Option<DynamicImage>) {
        self.image = img;
        self.gray = None;
    }

    /// Luma copy of the raster, computed on first use.
    pub fn gray(&mut self) -> Option<&image::GrayImage> {
        if self.gray.is_none() {
            self.gray = self.image.as_ref().map(fast_luma);
        }
        self.gray.as_ref()
    }
}

pub trait Stage {
    fn name(&self) -> &'static str;
    /// Document-level work, run once before any page.
    fn run_document(&self, _doc: &dyn PdfDoc, _model: &mut Document) -> Result<()> {
        Ok(())
    }
    /// Page-level work.
    fn run_page(&self, _doc: &dyn PdfDoc, _ctx: &mut PageContext) -> Result<()> {
        Ok(())
    }
    /// Whether this stage still runs on pages flagged `skip_rest`.
    fn runs_on_skipped(&self) -> bool {
        false
    }
}

pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn with(mut self, stage: impl Stage + 'static) -> Self {
        self.stages.push(Box::new(stage));
        self
    }

    pub fn run(&self, backend: &dyn PdfBackend, path: &Path) -> Result<Document> {
        let wall = Instant::now();
        let doc = backend.open(path)?;
        let mut model = Document::new(path.display().to_string(), backend.name());
        model.stats.page_count = doc.page_count();

        for stage in &self.stages {
            let t = Instant::now();
            stage.run_document(doc.as_ref(), &mut model)?;
            model.stats.add_time(stage.name(), t.elapsed().as_secs_f64() * 1000.0);
        }

        for idx in 0..doc.page_count() {
            let mut ctx = PageContext::new(Page::new(idx + 1, doc.page_geometry(idx)?));
            for stage in &self.stages {
                if ctx.skip_rest && !stage.runs_on_skipped() {
                    continue;
                }
                let t = Instant::now();
                stage.run_page(doc.as_ref(), &mut ctx).map_err(|e| Error::Stage {
                    stage: stage.name(),
                    page: idx + 1,
                    msg: e.to_string(),
                })?;
                model.stats.add_time(stage.name(), t.elapsed().as_secs_f64() * 1000.0);
            }
            // Drop the raster before storing the page: bounded memory.
            ctx.set_image(None);
            model.pages.push(ctx.page);
        }

        model.stats.wall_ms = wall.elapsed().as_secs_f64() * 1000.0;
        model.stats.peak_rss_kb = metrics::peak_rss_kb();
        model.stats.cpu_ms = metrics::process_cpu_ms();
        Ok(model)
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}
