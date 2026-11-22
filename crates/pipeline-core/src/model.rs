//! Document model. Coordinates: PDF points (1/72 in), origin top-left, y down.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BBox {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
    /// Smallest box containing both.
    pub fn union(&self, o: &BBox) -> BBox {
        BBox { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    pub pdf_version: Option<String>,
    pub page_count: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PageGeometry {
    pub width_pt: f32,
    pub height_pt: f32,
    /// Intrinsic `/Rotate` value in degrees (0, 90, 180, 270).
    pub rotation: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlankInfo {
    pub is_blank: bool,
    /// Fraction of "ink" pixels in a low-res render (0..1).
    pub ink_ratio: f32,
    pub text_chars: usize,
    pub image_objects: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RenderInfo {
    pub dpi: f32,
    pub width_px: u32,
    pub height_px: u32,
    /// Saved PNG, relative to the output directory (when saving is enabled).
    pub image_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Good,
    Fair,
    Poor,
}

/// Image-quality signals for one page (stage 2). All scores are heuristics
/// computed on the analysis render; see `stage-quality` for the formulas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityInfo {
    /// Sharpness: 90th percentile of per-tile Laplacian variance. Higher = sharper.
    pub sharpness: f32,
    /// Paper-to-ink luma range (p98 - p0.1, 0..255). Low = washed out / faint.
    pub contrast: f32,
    /// Mean luma (0..255).
    pub brightness: f32,
    pub is_blurry: bool,
    pub is_low_contrast: bool,
    /// Combined readability estimate 0..1 (heuristic; OCR confidence joins in M4).
    pub readability: f32,
    pub verdict: Verdict,
    pub notes: Vec<String>,
}

/// Orientation findings for one page (stage 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrientationInfo {
    /// Intrinsic `/Rotate` of the page (already applied by renderers).
    pub page_rotate: u16,
    /// Extra clockwise rotation (0/90/180/270) needed to make the *displayed*
    /// content upright. 0 = already upright.
    pub correction_cw: u16,
    /// `text_layer`, `image_projection` or `none`.
    pub method: String,
    pub confidence: f32,
    /// Sideways text found but 90 vs 270 (or 0 vs 180) cannot be told apart by
    /// this method; `correction_cw` is then the *quarter-turn candidate* and no
    /// rotation is applied. An OCR angle classifier (M4) resolves it.
    pub ambiguous: bool,
    /// True if the render held in the page context was rotated by `correction_cw`.
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Heading,
    Paragraph,
    Figure,
    Table,
    Chart,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSource {
    TextLayer,
    Ocr,
    Image,
}

/// One extracted element with provenance (page is the owning `Page.number`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    /// Heading level 1..=3 when `kind == Heading`.
    pub level: Option<u8>,
    pub bbox: BBox,
    pub source: BlockSource,
    pub confidence: Option<f32>,
    pub font_size: Option<f32>,
    /// Saved image (figures), relative to the output directory.
    pub image_path: Option<String>,
}

/// What the OCR stage did on a page (only present when OCR ran).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrInfo {
    pub engine: String,
    pub dpi: f32,
    pub lines: usize,
    /// Mean line confidence, weighted by text length.
    pub mean_confidence: f32,
    /// Lines below the engine's acceptance confidence after the full model cascade.
    pub low_confidence_lines: usize,
    /// Lines won by each recognizer.
    pub models: std::collections::BTreeMap<String, usize>,
    pub language: Option<crate::LangGuess>,
    pub render_ms: f64,
    pub detect_ms: f64,
    pub recognize_ms: f64,
    /// Orientation applied before OCR (clockwise degrees), when the classifier asked for one.
    pub rotated_cw: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub number: usize,
    pub geometry: PageGeometry,
    pub profile: Option<crate::PageProfile>,
    pub ocr: Option<OcrInfo>,
    pub render: Option<RenderInfo>,
    pub blank: Option<BlankInfo>,
    pub quality: Option<QualityInfo>,
    pub orientation: Option<OrientationInfo>,
    pub blocks: Vec<Block>,
}

impl Page {
    pub fn new(number: usize, geometry: PageGeometry) -> Self {
        Self {
            number,
            geometry,
            profile: None,
            ocr: None,
            render: None,
            blank: None,
            quality: None,
            orientation: None,
            blocks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub page_count: usize,
    pub wall_ms: f64,
    pub cpu_ms: f64,
    pub peak_rss_kb: u64,
    /// Total time per stage in milliseconds.
    pub stage_ms: BTreeMap<String, f64>,
}

impl Stats {
    pub fn add_time(&mut self, stage: &str, ms: f64) {
        *self.stage_ms.entry(stage.to_string()).or_insert(0.0) += ms;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub source: String,
    pub backend: String,
    pub metadata: Metadata,
    pub pages: Vec<Page>,
    pub stats: Stats,
}

impl Document {
    pub fn new(source: String, backend: &str) -> Self {
        Self {
            source,
            backend: backend.to_string(),
            metadata: Metadata::default(),
            pages: Vec::new(),
            stats: Stats::default(),
        }
    }
}
