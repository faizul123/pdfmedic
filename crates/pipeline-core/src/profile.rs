//! Fast per-page triage ("profile") types. Produced by `stage-profile` from a
//! single pass over page objects, before any rendering or full parsing.

use crate::BBox;
use serde::{Deserialize, Serialize};

/// A candidate region with a 0..1 confidence-like score (heuristic, not calibrated).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionHint {
    pub bbox: BBox,
    pub score: f32,
    /// Short reason, e.g. "ruled grid 5x4" or "aligned columns".
    pub why: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind {
    /// Live content (text and/or vector).
    No,
    /// Page is (almost) one image and has no text layer: needs OCR.
    Scanned,
    /// Page is one image plus a hidden text layer (earlier OCR): text exists
    /// but its quality is unknown.
    ScannedWithTextLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteHint {
    /// Nothing on the page.
    Skip,
    /// Trustworthy text layer: extract with Rust.
    RustOnly,
    /// No usable text: render and OCR.
    Ocr,
    /// Text layer exists but sits under a scan: verify before trusting.
    VerifyTextLayer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangGuess {
    /// ISO 639-1 where known (en, fr, ar...), else the ISO 639-3 code.
    pub code: String,
    pub confidence: f32,
    pub reliable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageProfile {
    pub has_text: bool,
    /// Visible characters in the text layer (estimated from geometry; only trust it as
    /// zero / few / many).
    pub visible_chars: usize,
    /// Non-whitespace characters in hidden (render mode 3) text.
    pub hidden_chars: usize,
    /// Share of characters per script in the sampled text, descending.
    pub scripts: Vec<(String, f32)>,
    pub language: Option<LangGuess>,
    pub scan: ScanKind,
    pub image_count: usize,
    /// Fraction of the page covered by images (0..1).
    pub image_coverage: f32,
    pub images: Vec<BBox>,
    pub text_extent: Option<BBox>,
    pub table_candidates: Vec<RegionHint>,
    pub chart_candidates: Vec<RegionHint>,
    pub seal_candidates: Vec<RegionHint>,
    pub text_objects: usize,
    pub path_objects: usize,
    /// Visible annotations (stamps, form fields, ink...); these are not page objects.
    pub annotation_count: usize,
    /// Tables/charts/seals inside a scan cannot be seen at object level; a
    /// low-resolution pixel pass is needed to look for them.
    pub needs_pixel_pass: bool,
    pub likely_blank: bool,
    pub route_hint: RouteHint,
    /// The object walk hit `max_objects`; counts are lower bounds.
    pub truncated: bool,
    /// Time spent profiling this page.
    pub scan_us: u64,
    /// Set when the pixel pass ran on this page (microseconds, render included).
    /// Candidates it found carry a `pixels:` prefix in `why`.
    #[serde(default)]
    pub pixel_pass_us: Option<u64>,
}

// ---- raw data a backend returns from one object walk -----------------------

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Characters of text kept for script/language detection.
    pub text_sample_chars: usize,
    /// Stop after this many page objects (guards pathological pages).
    pub max_objects: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { text_sample_chars: 600, max_objects: 6000 }
    }
}

#[derive(Debug, Clone)]
pub struct RawText {
    pub bbox: BBox,
    pub font_size: f32,
    pub hidden: bool,
    /// Estimated non-whitespace characters (from box width / font size; exact text per
    /// object is too slow to fetch). Zero for every object when the page text is empty.
    pub chars: usize,
}

#[derive(Debug, Clone)]
pub struct RawPath {
    pub bbox: BBox,
    pub filled: bool,
    pub stroked: bool,
    pub fill_rgb: Option<[u8; 3]>,
    pub stroke_rgb: Option<[u8; 3]>,
    /// Number of Bezier curve segments. Only computed for roughly square,
    /// stamp-sized paths (a per-path cost); 0 otherwise.
    pub curve_segments: usize,
}

#[derive(Debug, Clone)]
pub struct RawImage {
    pub bbox: BBox,
    pub width_px: u32,
    pub height_px: u32,
}

/// A visible annotation (stamp, form field, ink, free text, ...). Links are omitted.
#[derive(Debug, Clone)]
pub struct RawAnnotation {
    pub bbox: BBox,
    /// Lowercase kind: `stamp`, `widget`, `ink`, `freetext`, `highlight`, `text`, `other`.
    pub kind: String,
}

#[derive(Debug, Clone, Default)]
pub struct RawPageScan {
    /// Text from the first text objects, in content order (hidden text included).
    pub text_sample: String,
    pub texts: Vec<RawText>,
    pub paths: Vec<RawPath>,
    pub images: Vec<RawImage>,
    pub annotations: Vec<RawAnnotation>,
    pub truncated: bool,
}
