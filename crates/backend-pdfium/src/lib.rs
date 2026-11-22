//! PDFium backend (via `pdfium-render`). Needs `libpdfium.so`.

use pdfium_render::prelude::*;
use pipeline_core::geometry::rect_to_display;
use pipeline_core::image::DynamicImage;
use pipeline_core::{
    BBox, Error, ImageObject, Metadata, PageContentSummary, PageGeometry, PdfBackend, PdfDoc, RawAnnotation,
    RawImage, RawPageScan, RawPath, RawText, Result, ScanOptions, TextLine,
};
use std::path::{Path, PathBuf};

pub struct PdfiumBackend {
    pdfium: Pdfium,
}

impl PdfiumBackend {
    /// Load libpdfium from `lib_dir` (a directory containing `libpdfium.so`).
    pub fn from_dir(lib_dir: impl AsRef<Path>) -> Result<Self> {
        let lib: PathBuf = Pdfium::pdfium_platform_library_name_at_path(lib_dir.as_ref());
        let bindings = Pdfium::bind_to_library(&lib)
            .map_err(|e| Error::Backend(format!("cannot load {}: {e}", lib.display())))?;
        Ok(Self { pdfium: Pdfium::new(bindings) })
    }
}

fn be<E: std::fmt::Display>(e: E) -> Error {
    Error::Backend(e.to_string())
}

impl PdfBackend for PdfiumBackend {
    fn name(&self) -> &'static str {
        "pdfium-render"
    }

    fn open<'a>(&'a self, path: &Path) -> Result<Box<dyn PdfDoc + 'a>> {
        let doc = self.pdfium.load_pdf_from_file(path, None).map_err(be)?;
        Ok(Box::new(PdfiumDoc { doc }))
    }
}

struct PdfiumDoc<'a> {
    doc: PdfDocument<'a>,
}

impl PdfiumDoc<'_> {
    fn page(&self, idx: usize) -> Result<PdfPage<'_>> {
        self.doc.pages().get(idx as PdfPageIndex).map_err(be)
    }

    fn rotate_deg(page: &PdfPage<'_>) -> Result<u16> {
        Ok(match page.rotation().map_err(be)? {
            PdfPageRenderRotation::None => 0,
            PdfPageRenderRotation::Degrees90 => 90,
            PdfPageRenderRotation::Degrees180 => 180,
            PdfPageRenderRotation::Degrees270 => 270,
        })
    }

    /// Unrotated page size in points. `width()/height()` already reflect `/Rotate`.
    fn unrotated_size(page: &PdfPage<'_>, rot: u16) -> (f32, f32) {
        let (w, h) = (page.width().value, page.height().value);
        if rot % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

impl PdfDoc for PdfiumDoc<'_> {
    fn page_count(&self) -> usize {
        self.doc.pages().len() as usize
    }

    fn metadata(&self) -> Result<Metadata> {
        let md = self.doc.metadata();
        let get = |t| md.get(t).map(|x| x.value().to_string()).filter(|s| !s.is_empty());
        Ok(Metadata {
            title: get(PdfDocumentMetadataTagType::Title),
            author: get(PdfDocumentMetadataTagType::Author),
            subject: get(PdfDocumentMetadataTagType::Subject),
            keywords: get(PdfDocumentMetadataTagType::Keywords),
            creator: get(PdfDocumentMetadataTagType::Creator),
            producer: get(PdfDocumentMetadataTagType::Producer),
            creation_date: get(PdfDocumentMetadataTagType::CreationDate),
            modification_date: get(PdfDocumentMetadataTagType::ModificationDate),
            pdf_version: Some(format!("{:?}", self.doc.version())),
            page_count: self.page_count(),
        })
    }

    fn page_geometry(&self, idx: usize) -> Result<PageGeometry> {
        let page = self.page(idx)?;
        let rotation = Self::rotate_deg(&page)?;
        Ok(PageGeometry { width_pt: page.width().value, height_pt: page.height().value, rotation })
    }

    fn render_page(&self, idx: usize, dpi: f32, max_dim: u32) -> Result<DynamicImage> {
        let page = self.page(idx)?;
        let scale = dpi / 72.0;
        let (mut w, mut h) = (page.width().value * scale, page.height().value * scale);
        let longest = w.max(h);
        if longest > max_dim as f32 {
            let k = max_dim as f32 / longest;
            w *= k;
            h *= k;
        }
        let cfg = PdfRenderConfig::new().set_target_size(w.max(1.0) as i32, h.max(1.0) as i32);
        let bitmap = page.render_with_config(&cfg).map_err(be)?;
        bitmap.as_image().map_err(be)
    }

    fn content_summary(&self, idx: usize) -> Result<PageContentSummary> {
        let page = self.page(idx)?;
        // NOTE: do not use `text().all()`: it filters by the page rectangle, which on
        // /Rotate 90/270 pages is the rotated box while glyphs live in unrotated
        // space, so most text is silently dropped. Iterate characters instead.
        let text = page.text().map_err(be)?;
        let text_chars =
            text.chars().iter().filter(|c| c.unicode_char().is_some_and(|ch| !ch.is_whitespace())).count();
        let (mut image_objects, mut path_objects) = (0, 0);
        for obj in page.objects().iter() {
            match obj.object_type() {
                PdfPageObjectType::Image => image_objects += 1,
                PdfPageObjectType::Path => path_objects += 1,
                _ => {}
            }
        }
        Ok(PageContentSummary { text_chars, image_objects, path_objects })
    }

    fn page_text_lines(&self, idx: usize) -> Result<Vec<TextLine>> {
        let page = self.page(idx)?;
        let rot = Self::rotate_deg(&page)?;
        let (w0, h0) = Self::unrotated_size(&page, rot);
        let text = page.text().map_err(be)?;
        let mut lines = Vec::new();
        for seg in text.segments().iter() {
            let t = seg.text();
            if t.trim().is_empty() {
                continue;
            }
            let r = seg.bounds();
            let bbox = rect_to_display(
                r.left().value,
                r.bottom().value,
                r.right().value,
                r.top().value,
                w0,
                h0,
                rot,
            );
            let font_size = match seg.chars() {
                Ok(chars) => chars
                    .iter()
                    .find(|ch| ch.unicode_char().is_some_and(|x| !x.is_whitespace()))
                    .map(|ch| ch.scaled_font_size().value)
                    .unwrap_or(0.0),
                Err(_) => 0.0,
            };
            lines.push(TextLine { text: t.trim_end().to_string(), bbox, font_size });
        }
        Ok(lines)
    }

    fn page_images(&self, idx: usize, with_pixels: bool) -> Result<Vec<ImageObject>> {
        let page = self.page(idx)?;
        let rot = Self::rotate_deg(&page)?;
        let (w0, h0) = Self::unrotated_size(&page, rot);
        let mut out = Vec::new();
        for obj in page.objects().iter() {
            let Some(img) = obj.as_image_object() else { continue };
            let Ok(q) = obj.bounds() else { continue };
            let bbox: BBox = rect_to_display(
                q.left().value,
                q.bottom().value,
                q.right().value,
                q.top().value,
                w0,
                h0,
                rot,
            );
            let image = if with_pixels { img.get_processed_image(&self.doc).ok() } else { None };
            out.push(ImageObject {
                bbox,
                width_px: img.width().unwrap_or(0).max(0) as u32,
                height_px: img.height().unwrap_or(0).max(0) as u32,
                image,
            });
        }
        Ok(out)
    }

    fn page_scan(&self, idx: usize, opts: &ScanOptions) -> Result<RawPageScan> {
        let page = self.page(idx)?;
        let rot = Self::rotate_deg(&page)?;
        let (w0, h0) = Self::unrotated_size(&page, rot);
        let mut scan = RawPageScan::default();

        for (n, obj) in page.objects().iter().enumerate() {
            if n >= opts.max_objects {
                scan.truncated = true;
                break;
            }
            let kind = obj.object_type();
            if !matches!(kind, PdfPageObjectType::Text | PdfPageObjectType::Path | PdfPageObjectType::Image) {
                continue;
            }
            let Ok(q) = obj.bounds() else { continue };
            let bbox = rect_to_display(
                q.left().value,
                q.bottom().value,
                q.right().value,
                q.top().value,
                w0,
                h0,
                rot,
            );
            match kind {
                PdfPageObjectType::Text => {
                    let Some(t) = obj.as_text_object() else { continue };
                    // NOTE: `PdfPageTextObject::text()` costs ~170 us per object (measured: 5.3 ms
                    // for a 31-object page; every other call here is < 0.1 ms/page). So the
                    // character count is estimated from geometry, and exact text comes from one
                    // text-page character walk below.
                    let fs = t.scaled_font_size().value.max(1.0);
                    let est = ((bbox.width() / (0.45 * fs)).round() as usize).max(1);
                    scan.texts.push(RawText {
                        bbox,
                        font_size: fs,
                        hidden: matches!(t.render_mode(), PdfPageTextRenderMode::Invisible),
                        chars: est,
                    });
                }
                PdfPageObjectType::Path => {
                    let Some(p) = obj.as_path_object() else { continue };
                    let filled = p.fill_mode().map(|m| m != PdfPathFillMode::None).unwrap_or(false);
                    let stroked = p.is_stroked().unwrap_or(false);
                    let rgb = |c: std::result::Result<PdfColor, PdfiumError>| {
                        c.ok().map(|c| [c.red(), c.green(), c.blue()])
                    };
                    // Curve counting costs one FFI call per segment, so only do it for
                    // roughly square, stamp-sized paths (seal candidates).
                    let (w, h) = (bbox.width(), bbox.height());
                    let squarish =
                        w > 30.0 && w < 260.0 && h > 30.0 && h < 260.0 && (w / h) > 0.8 && (w / h) < 1.25;
                    let curve_segments = if squarish {
                        p.segments()
                            .iter()
                            .filter(|sg| matches!(sg.segment_type(), PdfPathSegmentType::BezierTo))
                            .count()
                    } else {
                        0
                    };
                    scan.paths.push(RawPath {
                        bbox,
                        filled,
                        stroked,
                        fill_rgb: if filled { rgb(obj.fill_color()) } else { None },
                        stroke_rgb: if stroked { rgb(obj.stroke_color()) } else { None },
                        curve_segments,
                    });
                }
                PdfPageObjectType::Image => {
                    let Some(img) = obj.as_image_object() else { continue };
                    scan.images.push(RawImage {
                        bbox,
                        width_px: img.width().unwrap_or(0).max(0) as u32,
                        height_px: img.height().unwrap_or(0).max(0) as u32,
                    });
                }
                _ => {}
            }
        }
        // One text-page walk, capped: language sample plus an exact "any visible text?" check
        // (a page of whitespace-only text objects must not count as having text).
        if !scan.texts.is_empty() {
            let mut nonws = 0usize;
            if let Ok(text) = page.text() {
                for ch in text.chars().iter() {
                    let Some(c) = ch.unicode_char() else { continue };
                    if !c.is_whitespace() {
                        nonws += 1;
                    }
                    if c != '\r' && c != '\n' {
                        scan.text_sample.push(c);
                    } else if !scan.text_sample.ends_with(' ') {
                        scan.text_sample.push(' ');
                    }
                    if nonws >= opts.text_sample_chars {
                        break;
                    }
                }
            }
            if nonws == 0 {
                for t in &mut scan.texts {
                    t.chars = 0;
                }
            }
        }
        for ann in page.annotations().iter() {
            let kind = match ann.annotation_type() {
                PdfPageAnnotationType::Link => continue,
                PdfPageAnnotationType::Stamp => "stamp",
                PdfPageAnnotationType::Widget | PdfPageAnnotationType::XfaWidget => "widget",
                PdfPageAnnotationType::Ink => "ink",
                PdfPageAnnotationType::FreeText => "freetext",
                PdfPageAnnotationType::Highlight => "highlight",
                PdfPageAnnotationType::Text => "text",
                _ => "other",
            };
            let Ok(r) = ann.bounds() else { continue };
            let bbox = rect_to_display(
                r.left().value,
                r.bottom().value,
                r.right().value,
                r.top().value,
                w0,
                h0,
                rot,
            );
            scan.annotations.push(RawAnnotation { bbox, kind: kind.to_string() });
        }
        Ok(scan)
    }

    fn text_orientation(&self, idx: usize) -> Result<Option<(u16, f32)>> {
        let page = self.page(idx)?;
        let rot = Self::rotate_deg(&page)? as f32;
        let text = page.text().map_err(be)?;
        let mut buckets = [0usize; 4];
        let mut total = 0usize;
        for ch in text.chars().iter().take(20_000) {
            if !ch.unicode_char().is_some_and(|c| !c.is_whitespace()) {
                continue;
            }
            let Ok(a) = ch.angle_degrees() else { continue };
            // PDFium reports glyph angle clockwise (verified: a glyph run drawn
            // bottom-to-top, a 90 deg *counter*-clockwise matrix, yields 270).
            // /Rotate is clockwise too, so the displayed direction is their sum, and
            // the clockwise turn that makes it upright is the negation.
            let display_cw = (a + rot).rem_euclid(360.0);
            let correction = (360.0 - display_cw).rem_euclid(360.0);
            let b = (((correction + 45.0) / 90.0).floor() as usize) % 4;
            buckets[b] += 1;
            total += 1;
        }
        if total == 0 {
            return Ok(None);
        }
        let (best, n) = buckets.iter().enumerate().max_by_key(|(_, n)| **n).map(|(i, n)| (i, *n)).unwrap();
        Ok(Some(((best as u16) * 90, n as f32 / total as f32)))
    }
}
