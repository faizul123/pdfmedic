//! OCR engine abstraction. Engines work on a rendered page image and return text lines
//! with boxes in *image pixels*; the OCR stage maps them back to PDF points.

use crate::{BBox, Result};
use image::RgbImage;

/// One recognized text line.
#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    /// Box in pixels of the image given to the engine (top-left origin).
    pub bbox: BBox,
    /// Recognizer confidence 0..1.
    pub confidence: f32,
    /// Name of the recognizer that produced the text (e.g. `general`, `arabic`).
    pub model: String,
}

#[derive(Debug, Clone, Default)]
pub struct OcrOutput {
    /// In reading order (top to bottom, then left to right).
    pub lines: Vec<OcrLine>,
    pub detect_ms: f64,
    pub recognize_ms: f64,
}

/// Page rotation as seen by an orientation classifier.
#[derive(Debug, Clone, Copy)]
pub struct OrientationGuess {
    /// Clockwise rotation (0, 90, 180, 270) that makes the page upright.
    pub correction_cw: u16,
    /// Score of the winning class.
    pub confidence: f32,
    /// Winning score minus the runner-up: the reliable signal (absolute scores are low even
    /// when the answer is right).
    pub margin: f32,
}

pub trait OcrEngine {
    fn name(&self) -> String;
    /// Detect text lines and recognize them.
    fn recognize(&self, img: &RgbImage) -> Result<OcrOutput>;
    /// Whole-page orientation, when the engine has a classifier for it.
    fn page_orientation(&self, _img: &RgbImage) -> Result<Option<OrientationGuess>> {
        Ok(None)
    }
}
