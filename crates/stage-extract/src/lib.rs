//! Stage 5/8: text-layer extraction into structured blocks with bboxes.
//!
//! Pipeline inside the stage:
//! 1. text runs from the backend -> merged into visual lines (same baseline),
//! 2. lines sorted in reading order (top to bottom, left to right),
//! 3. consecutive lines grouped into paragraphs / lists, large type -> headings,
//! 4. image objects -> figure blocks (pixels optionally saved to `assets/`).
//!
//! Grouping happens in the upright frame given by the orientation stage; bboxes
//! are reported in the displayed frame.
//!
//! Limits (tracked in Plan.md): single-column reading order only (multi-column
//! pages interleave), no table/chart detection yet (M3), no OCR (M4).

use pipeline_core::geometry::rotate_bbox_cw;
use pipeline_core::{BBox, Block, BlockKind, BlockSource, PageContext, PdfDoc, Result, Stage, TextLine};
use std::path::PathBuf;

pub struct ExtractStage {
    /// If set, figure pixels are saved as `<dir>/assets/page-NNNN-img-K.png`.
    pub save_dir: Option<PathBuf>,
    /// Ignore images smaller than this many points on either side (icons, rules).
    pub min_image_pt: f32,
}

impl Default for ExtractStage {
    fn default() -> Self {
        Self { save_dir: None, min_image_pt: 12.0 }
    }
}

/// Merge text runs that share a baseline into visual lines, in reading order.
pub fn merge_into_lines(mut runs: Vec<TextLine>) -> Vec<TextLine> {
    runs.sort_by(|a, b| {
        let ca = (a.bbox.y0 + a.bbox.y1) / 2.0;
        let cb = (b.bbox.y0 + b.bbox.y1) / 2.0;
        ca.partial_cmp(&cb).unwrap().then(a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap())
    });
    let mut lines: Vec<(Vec<TextLine>, f32)> = Vec::new(); // (runs, center y)
    for r in runs {
        let cy = (r.bbox.y0 + r.bbox.y1) / 2.0;
        let tol = r.bbox.height().max(1.0) * 0.5;
        match lines.last_mut() {
            Some((group, gy)) if (cy - *gy).abs() <= tol => group.push(r),
            _ => lines.push((vec![r], cy)),
        }
    }
    lines
        .into_iter()
        .map(|(mut g, _)| {
            g.sort_by(|a, b| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap());
            let mut text = String::new();
            let mut bbox = g[0].bbox;
            let mut fs = 0.0f32;
            let mut prev_x1: Option<f32> = None;
            for run in &g {
                if let Some(px) = prev_x1 {
                    // Gap wider than ~ a space: keep words apart.
                    if run.bbox.x0 - px > run.font_size.max(1.0) * 0.15 && !text.ends_with(' ') {
                        text.push(' ');
                    }
                }
                text.push_str(&run.text);
                bbox = bbox.union(&run.bbox);
                fs = fs.max(run.font_size);
                prev_x1 = Some(run.bbox.x1);
            }
            TextLine { text, bbox, font_size: fs }
        })
        .collect()
}

fn list_marker(text: &str) -> bool {
    let t = text.trim_start();
    if t.starts_with("• ") || t.starts_with("- ") || t.starts_with("* ") || t.starts_with("– ") {
        return true;
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0
        && digits <= 3
        && matches!(t[digits..].chars().next(), Some('.') | Some(')'))
        && t[digits + 1..].starts_with(' ')
}

fn body_font_size(lines: &[TextLine]) -> f32 {
    // Character-weighted median font size: stable when sizes are noisy (OCR box heights vary by
    // 20-30% for one font), unlike the mode of narrow bins.
    let mut v: Vec<(f32, usize)> = lines
        .iter()
        .filter(|l| l.font_size > 0.0)
        .map(|l| (l.font_size, l.text.chars().count().max(1)))
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let half = v.iter().map(|x| x.1).sum::<usize>() as f32 / 2.0;
    let mut acc = 0.0;
    for (fs, n) in &v {
        acc += *n as f32;
        if acc >= half {
            return *fs;
        }
    }
    v.last().unwrap().0
}

/// Group visual lines into blocks (paragraph / heading / list).
pub fn group_blocks(lines: &[TextLine]) -> Vec<Block> {
    group_blocks_with(lines, TEXT_LAYER_PARA_GAP, 0.0)
}

/// Gap between lines (as a fraction of line height) up to which lines stay in one paragraph.
/// Text-layer boxes are tight to the glyphs.
pub const TEXT_LAYER_PARA_GAP: f32 = 0.9;
/// OCR detector boxes are inflated (~1.5x the line pitch) and overlap inside a paragraph, so
/// the dividing line is much smaller: inside a paragraph the gap is <= 0, between paragraphs
/// about 0.5 of the box height.
pub const OCR_PARA_GAP: f32 = 0.25;

/// Relative font-size tolerance for "same size" between neighbouring lines. OCR box heights vary
/// by ~20-30% for one font (measured: 38-47 px on one page), text-layer sizes are exact.
pub const OCR_SIZE_TOL: f32 = 0.35;

/// [`group_blocks`] with an explicit paragraph gap threshold and relative size tolerance (see the
/// constants above; 0.0 keeps the absolute 0.5 pt tolerance).
pub fn group_blocks_with(lines: &[TextLine], para_gap: f32, size_tol: f32) -> Vec<Block> {
    let body = body_font_size(lines);
    let mut out: Vec<Block> = Vec::new();
    let mk = |kind, level, text: String, bbox: BBox, fs: f32| Block {
        kind,
        text,
        level,
        bbox,
        source: BlockSource::TextLayer,
        confidence: None,
        font_size: Some(fs),
        image_path: None,
    };
    let mut cur: Option<(BlockKind, Vec<&TextLine>)> = None;
    let flush = |cur: &mut Option<(BlockKind, Vec<&TextLine>)>, out: &mut Vec<Block>| {
        if let Some((kind, ls)) = cur.take() {
            let bbox = ls.iter().skip(1).fold(ls[0].bbox, |b, l| b.union(&l.bbox));
            let fs = ls.iter().map(|l| l.font_size).fold(0.0, f32::max);
            let (text, level, kind) = match kind {
                BlockKind::List => {
                    (ls.iter().map(|l| l.text.trim().to_string()).collect::<Vec<_>>().join("\n"), None, kind)
                }
                BlockKind::Heading => {
                    let ratio = if body > 0.0 { fs / body } else { 1.0 };
                    let lvl = if ratio >= 1.6 {
                        1
                    } else if ratio >= 1.35 {
                        2
                    } else {
                        3
                    };
                    (ls.iter().map(|l| l.text.trim()).collect::<Vec<_>>().join(" "), Some(lvl), kind)
                }
                _ => (ls.iter().map(|l| l.text.trim()).collect::<Vec<_>>().join(" "), None, kind),
            };
            out.push(mk(kind, level, text, bbox, fs));
        }
    };

    for (i, l) in lines.iter().enumerate() {
        let is_heading = body > 0.0 && l.font_size >= body * 1.15 && l.text.chars().count() <= 120;
        let is_item = list_marker(&l.text);
        let kind = if is_heading {
            BlockKind::Heading
        } else if is_item {
            BlockKind::List
        } else {
            BlockKind::Paragraph
        };

        let continues = match &cur {
            Some((ck, ls)) => {
                let prev = ls.last().unwrap();
                let gap = l.bbox.y0 - prev.bbox.y1;
                let line_h = prev.bbox.height().max(l.bbox.height()).max(1.0);
                let same_size = (prev.font_size - l.font_size).abs()
                    <= 0.5f32.max(size_tol * prev.font_size.max(l.font_size));
                match (*ck, kind) {
                    (BlockKind::Paragraph, BlockKind::Paragraph) => gap <= line_h * para_gap && same_size,
                    (BlockKind::List, BlockKind::List) => gap <= line_h * para_gap,
                    (BlockKind::Heading, BlockKind::Heading) => gap <= line_h * 0.6 && same_size,
                    _ => false,
                }
            }
            None => false,
        };
        if !continues {
            flush(&mut cur, &mut out);
            cur = Some((kind, vec![l]));
        } else if let Some((_, ls)) = cur.as_mut() {
            ls.push(l);
        }
        let _ = i;
    }
    flush(&mut cur, &mut out);
    out
}

impl Stage for ExtractStage {
    fn name(&self) -> &'static str {
        "extract"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        let idx = ctx.page.number - 1;
        // Group in the *upright* frame (orientation stage's correction), so
        // sideways / upside-down text still reads line by line; boxes are mapped
        // back to the displayed frame afterwards.
        let corr = match &ctx.page.orientation {
            Some(o) if !o.ambiguous => o.correction_cw,
            _ => 0,
        };
        let (pw, ph) = (ctx.page.geometry.width_pt, ctx.page.geometry.height_pt);
        let (uw, uh) = if corr % 180 == 90 { (ph, pw) } else { (pw, ph) };

        let runs: Vec<TextLine> = doc
            .page_text_lines(idx)?
            .into_iter()
            .map(|mut r| {
                r.bbox = rotate_bbox_cw(&r.bbox, pw, ph, corr);
                r
            })
            .collect();
        let lines = merge_into_lines(runs);
        let mut blocks = group_blocks(&lines);

        let want_pixels = self.save_dir.is_some();
        let mut k = 0;
        for im in doc.page_images(idx, want_pixels)? {
            if im.bbox.width() < self.min_image_pt || im.bbox.height() < self.min_image_pt {
                continue;
            }
            k += 1;
            let mut path = None;
            if let (Some(dir), Some(img)) = (&self.save_dir, &im.image) {
                let assets = dir.join("assets");
                std::fs::create_dir_all(&assets)?;
                let rel = format!("assets/page-{:04}-img-{}.png", ctx.page.number, k);
                img.save(dir.join(&rel)).map_err(|e| pipeline_core::Error::Other(e.to_string()))?;
                path = Some(rel);
            }
            blocks.push(Block {
                kind: BlockKind::Figure,
                text: format!("image {}x{} px", im.width_px, im.height_px),
                level: None,
                bbox: rotate_bbox_cw(&im.bbox, pw, ph, corr),
                source: BlockSource::Image,
                confidence: None,
                font_size: None,
                image_path: path,
            });
        }
        // Reading order across kinds (upright frame): top to bottom, then left to right.
        blocks.sort_by(|a, b| {
            a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap().then(a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap())
        });
        for b in &mut blocks {
            b.bbox = rotate_bbox_cw(&b.bbox, uw, uh, (360 - corr) % 360);
        }
        ctx.page.blocks = blocks;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, x0: f32, y0: f32, x1: f32, fs: f32) -> TextLine {
        TextLine { text: text.into(), bbox: BBox { x0, y0, x1, y1: y0 + fs * 1.2 }, font_size: fs }
    }

    #[test]
    fn merges_runs_on_one_baseline_and_orders_lines() {
        let runs = vec![
            run("world", 120.0, 100.0, 160.0, 12.0),
            run("Second line", 72.0, 120.0, 150.0, 12.0),
            run("Hello", 72.0, 100.0, 110.0, 12.0),
        ];
        let lines = merge_into_lines(runs);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Hello world");
        assert_eq!(lines[1].text, "Second line");
        assert!((lines[0].bbox.x1 - 160.0).abs() < 1e-3);
    }

    #[test]
    fn heading_paragraph_list_split() {
        let lines = vec![
            run("Annual Report", 72.0, 50.0, 300.0, 24.0),
            run("Revenue grew this year.", 72.0, 100.0, 300.0, 12.0),
            run("Costs were flat.", 72.0, 115.0, 300.0, 12.0),
            run("- first item", 72.0, 150.0, 200.0, 12.0),
            run("- second item", 72.0, 165.0, 200.0, 12.0),
        ];
        let b = group_blocks(&lines);
        assert_eq!(b.len(), 3, "{b:#?}");
        assert_eq!(b[0].kind, BlockKind::Heading);
        assert_eq!(b[0].level, Some(1));
        assert_eq!(b[1].kind, BlockKind::Paragraph);
        assert_eq!(b[1].text, "Revenue grew this year. Costs were flat.");
        assert_eq!(b[2].kind, BlockKind::List);
        assert_eq!(b[2].text, "- first item\n- second item");
    }

    #[test]
    fn inflated_ocr_boxes_need_the_smaller_gap() {
        // Boxes 38 px tall on a 27 px pitch (overlapping), and a paragraph gap of 19 px: the
        // numbers measured on the English eval page.
        let l = |y: f32| run("text line", 100.0, y, 900.0, 28.0);
        let mut lines = vec![l(130.0), l(157.0), l(189.0)];
        for ln in &mut lines {
            ln.bbox.y1 = ln.bbox.y0 + 38.0;
        }
        let mut second = vec![l(245.0), l(276.0)];
        for ln in &mut second {
            ln.bbox.y1 = ln.bbox.y0 + 38.0;
        }
        lines.extend(second);
        assert_eq!(group_blocks_with(&lines, OCR_PARA_GAP, OCR_SIZE_TOL).len(), 2);
        assert_eq!(group_blocks(&lines).len(), 1, "the text-layer threshold merges them");
    }

    #[test]
    fn noisy_ocr_heights_do_not_split_a_paragraph_or_invent_headings() {
        // One paragraph whose box heights vary 38/44/40 px (same font), then a real heading.
        let mut lines = Vec::new();
        for (i, h) in [38.0f32, 44.0, 40.0, 41.0].iter().enumerate() {
            let mut l =
                run("body text of the paragraph here", 100.0, 200.0 + i as f32 * 27.0, 900.0, h * 0.75 / 2.0);
            l.bbox.y1 = l.bbox.y0 + h;
            lines.push(l);
        }
        let b = group_blocks_with(&lines, OCR_PARA_GAP, OCR_SIZE_TOL);
        assert_eq!(b.len(), 1, "{b:#?}");
        assert_eq!(b[0].kind, BlockKind::Paragraph);
        // The strict text-layer tolerance does split them (documents why OCR needs its own).
        assert!(group_blocks(&lines).len() > 1);
    }

    #[test]
    fn big_gap_starts_new_paragraph() {
        let lines = vec![run("One.", 72.0, 100.0, 120.0, 12.0), run("Two.", 72.0, 160.0, 120.0, 12.0)];
        assert_eq!(group_blocks(&lines).len(), 2);
    }

    #[test]
    fn list_marker_detection() {
        assert!(list_marker("• item") && list_marker("1. item") && list_marker("12) item"));
        assert!(!list_marker("2025 was good") && !list_marker("Plain text"));
    }
}
