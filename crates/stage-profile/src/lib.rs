//! Fast per-page triage. One walk over page objects (no render, no full text
//! extraction) answers: is there text, which script/language, is it a scan, are
//! there images, and where are likely tables, charts and seals?
//!
//! Everything here is a **heuristic candidate** with a score and a position, meant
//! to route pages cheaply, not to replace parsing. Known blind spots:
//! * Content inside a scanned page's image is invisible at object level, so such
//!   pages get `needs_pixel_pass` instead of guesses.
//! * Borderless-table detection needs text objects at cell granularity, which
//!   depends on the PDF producer.
//! * Language needs ~25+ letters; short or mixed pages are unreliable.

use pipeline_core::{
    BBox, LangGuess, PageContext, PageProfile, PdfDoc, RawAnnotation, RawPageScan, RawPath, RawText,
    RegionHint, Result, RouteHint, ScanKind, ScanOptions, Stage,
};
use std::time::Instant;

#[derive(Default)]
pub struct ProfileStage {
    pub opts: ScanOptions,
    /// When a page has no content at all, skip the later (render-based) stages.
    pub skip_empty: bool,
}

impl Stage for ProfileStage {
    fn name(&self) -> &'static str {
        "profile"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut PageContext) -> Result<()> {
        let t = Instant::now();
        let scan = doc.page_scan(ctx.page.number - 1, &self.opts)?;
        let g = ctx.page.geometry;
        let mut p = classify(&scan, g.width_pt, g.height_pt);
        p.scan_us = t.elapsed().as_micros() as u64;
        if self.skip_empty && p.route_hint == RouteHint::Skip {
            ctx.skip_rest = true;
        }
        ctx.page.profile = Some(p);
        Ok(())
    }
}

/// Profile a range of pages (0-based, end exclusive) without running any other stage.
pub fn profile_pages(
    doc: &dyn PdfDoc,
    pages: std::ops::Range<usize>,
    opts: &ScanOptions,
) -> Result<Vec<(usize, PageProfile)>> {
    let mut out = Vec::with_capacity(pages.len());
    for idx in pages {
        let t = Instant::now();
        let g = doc.page_geometry(idx)?;
        let scan = doc.page_scan(idx, opts)?;
        let mut p = classify(&scan, g.width_pt, g.height_pt);
        p.scan_us = t.elapsed().as_micros() as u64;
        out.push((idx + 1, p));
    }
    Ok(out)
}

// ---------------------------------------------------------------- geometry --

fn area(b: &BBox) -> f32 {
    b.width().max(0.0) * b.height().max(0.0)
}

fn intersect_area(a: &BBox, b: &BBox) -> f32 {
    let w = a.x1.min(b.x1) - a.x0.max(b.x0);
    let h = a.y1.min(b.y1) - a.y0.max(b.y0);
    if w > 0.0 && h > 0.0 {
        w * h
    } else {
        0.0
    }
}

fn touches(a: &BBox, b: &BBox, pad: f32) -> bool {
    a.x0 - pad <= b.x1 && b.x0 - pad <= a.x1 && a.y0 - pad <= b.y1 && b.y0 - pad <= a.y1
}

fn center(b: &BBox) -> (f32, f32) {
    ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0)
}

fn contains_center(outer: &BBox, inner: &BBox) -> bool {
    let (cx, cy) = center(inner);
    cx >= outer.x0 && cx <= outer.x1 && cy >= outer.y0 && cy <= outer.y1
}

/// Group items whose boxes touch (within `pad`) into connected clusters.
fn cluster(boxes: &[BBox], pad: f32) -> Vec<Vec<usize>> {
    let n = boxes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let next = p[c];
            p[c] = r;
            c = next;
        }
        r
    }
    // Sweep in x0 order and stop at the first box beyond reach.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| boxes[a].x0.partial_cmp(&boxes[b].x0).unwrap());
    for (i, &a) in order.iter().enumerate() {
        for &b in &order[i + 1..] {
            if boxes[b].x0 - pad > boxes[a].x1 {
                break; // sorted by x0: every later box starts even further right
            }
            if touches(&boxes[a], &boxes[b], pad) {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}

fn union_all(boxes: impl Iterator<Item = BBox>) -> Option<BBox> {
    boxes.reduce(|a, b| a.union(&b))
}

// ------------------------------------------------------------------ script --

/// Script name for a letter.
pub fn script_of(c: char) -> &'static str {
    match c as u32 {
        0x0041..=0x005A | 0x0061..=0x007A | 0x00C0..=0x024F | 0x1E00..=0x1EFF => "latin",
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => "arabic",
        0x0400..=0x052F => "cyrillic",
        0x0370..=0x03FF => "greek",
        0x0590..=0x05FF => "hebrew",
        0x0900..=0x097F => "devanagari",
        0x0E00..=0x0E7F => "thai",
        0x3040..=0x30FF => "kana",
        0x4E00..=0x9FFF | 0x3400..=0x4DBF => "han",
        0xAC00..=0xD7AF => "hangul",
        _ => "other",
    }
}

/// Share of alphabetic characters per script, descending; shares under 1% dropped.
pub fn script_shares(text: &str) -> Vec<(String, f32)> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut total = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        let s = script_of(c);
        total += 1;
        match counts.iter_mut().find(|(n, _)| *n == s) {
            Some((_, k)) => *k += 1,
            None => counts.push((s, 1)),
        }
    }
    if total == 0 {
        return vec![];
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    counts
        .into_iter()
        .map(|(n, k)| (n.to_string(), k as f32 / total as f32))
        .filter(|(_, share)| *share >= 0.01)
        .collect()
}

fn short_code(lang: whatlang::Lang) -> String {
    use whatlang::Lang::*;
    match lang {
        Eng => "en",
        Fra => "fr",
        Ara => "ar",
        Spa => "es",
        Deu => "de",
        Ita => "it",
        Por => "pt",
        Rus => "ru",
        Pes => "fa",
        Urd => "ur",
        Heb => "he",
        Hin => "hi",
        Cmn => "zh",
        Jpn => "ja",
        Kor => "ko",
        Tur => "tr",
        Nld => "nl",
        other => return other.code().to_string(),
    }
    .to_string()
}

/// Language guess; `None` when there are too few letters to say anything.
pub fn detect_language(sample: &str) -> Option<LangGuess> {
    if sample.chars().filter(|c| c.is_alphabetic()).count() < 25 {
        return None;
    }
    let info = whatlang::detect(sample)?;
    Some(LangGuess {
        code: short_code(info.lang()),
        confidence: info.confidence() as f32,
        reliable: info.is_reliable(),
    })
}

// ------------------------------------------------------------------ tables --

const LINE_MAX_THICK: f32 = 2.5;

#[derive(Default)]
struct GridCluster {
    boxes: Vec<BBox>,
    h_lines: usize,
    v_lines: usize,
    cells: Vec<BBox>,
}

/// Ruled-grid detection over thin line paths / cell rectangles. Public so the pixel pass can
/// reuse it on lines it finds in rasters.
pub fn ruled_tables(paths: &[RawPath], texts: &[RawText], page: &BBox) -> Vec<RegionHint> {
    ruled_tables_padded(paths, texts, page, 2.0)
}

/// Like [`ruled_tables`] with an explicit join distance (points) between touching lines.
/// Lines recovered from pixels are less exact than vector paths and need a larger one.
pub fn ruled_tables_padded(paths: &[RawPath], texts: &[RawText], page: &BBox, pad: f32) -> Vec<RegionHint> {
    // Grid elements: thin horizontal/vertical lines, and stroked/filled cell rectangles.
    struct El {
        bbox: BBox,
        kind: u8, // 0 h-line, 1 v-line, 2 cell
    }
    let mut els: Vec<El> = Vec::new();
    for p in paths {
        let (w, h) = (p.bbox.width(), p.bbox.height());
        if h <= LINE_MAX_THICK && w >= 15.0 {
            els.push(El { bbox: p.bbox, kind: 0 });
        } else if w <= LINE_MAX_THICK && h >= 10.0 {
            els.push(El { bbox: p.bbox, kind: 1 });
        } else if p.stroked && w >= 8.0 && h >= 6.0 && area(&p.bbox) < 0.5 * area(page) {
            els.push(El { bbox: p.bbox, kind: 2 });
        }
    }
    if els.len() < 4 {
        return vec![];
    }
    let boxes: Vec<BBox> = els.iter().map(|e| e.bbox).collect();
    let mut out = Vec::new();
    for group in cluster(&boxes, pad) {
        let mut g = GridCluster::default();
        for &i in &group {
            g.boxes.push(els[i].bbox);
            match els[i].kind {
                0 => g.h_lines += 1,
                1 => g.v_lines += 1,
                _ => g.cells.push(els[i].bbox),
            }
        }
        let Some(bbox) = union_all(g.boxes.iter().copied()) else { continue };
        // A lone page border is not a table.
        if area(&bbox) > 0.9 * area(page) && g.h_lines + g.v_lines + g.cells.len() <= 4 {
            continue;
        }
        let inside = texts.iter().filter(|t| t.chars > 0 && contains_center(&bbox, &t.bbox)).count();
        let rows = distinct(g.cells.iter().map(|c| c.y0), 2.0);
        let cols = distinct(g.cells.iter().map(|c| c.x0), 2.0);
        let (score, why) = if g.h_lines >= 3 && g.v_lines >= 2 {
            let s = 0.6
                + 0.3 * ((g.h_lines + g.v_lines) as f32 / 12.0).min(1.0)
                + if inside >= 4 { 0.1 } else { 0.0 };
            (s, format!("ruled grid {} h-lines x {} v-lines", g.h_lines, g.v_lines))
        } else if g.cells.len() >= 6 && rows >= 2 && cols >= 2 {
            (0.6 + if inside >= 4 { 0.2 } else { 0.0 }, format!("cell rectangles {rows} rows x {cols} cols"))
        } else if g.h_lines >= 4 && g.v_lines == 0 && inside >= 6 {
            (0.45, format!("horizontal rules only ({}), likely borderless table with row lines", g.h_lines))
        } else {
            continue;
        };
        out.push(RegionHint { bbox, score: score.min(1.0), why });
    }
    out
}

/// Number of distinct values after clustering within `tol`.
fn distinct(vals: impl Iterator<Item = f32>, tol: f32) -> usize {
    let mut v: Vec<f32> = vals.collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut n = 0;
    let mut last = f32::NEG_INFINITY;
    for x in v {
        if x - last > tol {
            n += 1;
        }
        last = x;
    }
    n
}

/// Tables without ruling lines: consecutive rows of >= 3 text cells whose left edges repeat.
fn aligned_tables(texts: &[RawText]) -> Vec<RegionHint> {
    let vis: Vec<&RawText> = texts.iter().filter(|t| !t.hidden && t.chars > 0).collect();
    if vis.len() < 9 {
        return vec![];
    }
    // Rows by vertical center.
    let mut idx: Vec<usize> = (0..vis.len()).collect();
    idx.sort_by(|&a, &b| center(&vis[a].bbox).1.partial_cmp(&center(&vis[b].bbox).1).unwrap());
    let mut rows: Vec<Vec<&RawText>> = Vec::new();
    let mut row_y = f32::NEG_INFINITY;
    for i in idx {
        let t = vis[i];
        let cy = center(&t.bbox).1;
        let tol = t.bbox.height().max(6.0) * 0.5;
        if rows.is_empty() || (cy - row_y).abs() > tol {
            rows.push(vec![t]);
            row_y = cy;
        } else {
            rows.last_mut().unwrap().push(t);
        }
    }
    // Row spacing limit scales with font size (glyph boxes are tight, so padded table rows
    // sit much further apart than the glyph height suggests).
    let mut sizes: Vec<f32> = vis.iter().map(|t| t.font_size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med_font = sizes[sizes.len() / 2].max(6.0);
    let mut heights: Vec<f32> = vis.iter().map(|t| t.bbox.height()).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med_h = heights[heights.len() / 2].max(6.0);
    let max_gap = (med_h * 3.0).max(med_font * 3.5);

    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].len() < 3 {
            i += 1;
            continue;
        }
        // Extend a run of multi-cell rows with small vertical gaps.
        let mut j = i;
        while j + 1 < rows.len() && rows[j + 1].len() >= 3 {
            let gap = center(&rows[j + 1][0].bbox).1 - center(&rows[j][0].bbox).1;
            if gap > max_gap {
                break;
            }
            j += 1;
        }
        let run = &rows[i..=j];
        if run.len() >= 3 {
            // Column anchors: left edges repeated across most rows.
            let mut xs: Vec<f32> = run.iter().flatten().map(|t| t.bbox.x0).collect();
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut anchors: Vec<(f32, usize)> = Vec::new();
            for x in xs {
                match anchors.last_mut() {
                    Some((ax, n)) if x - *ax <= 5.0 => *n += 1,
                    _ => anchors.push((x, 1)),
                }
            }
            let need = ((run.len() as f32) * 0.6).ceil().max(3.0) as usize;
            let strong = anchors.iter().filter(|(_, n)| *n >= need).count();
            if strong >= 3 {
                let bbox = union_all(run.iter().flatten().map(|t| t.bbox)).unwrap();
                let score = 0.5
                    + 0.2 * ((run.len() as f32 - 3.0) / 6.0).clamp(0.0, 1.0)
                    + 0.2 * ((strong as f32 - 3.0) / 3.0).clamp(0.0, 1.0);
                out.push(RegionHint {
                    bbox,
                    score,
                    why: format!("aligned text columns: {} rows x {} columns", run.len(), strong),
                });
            }
        }
        i = j + 1;
    }
    out
}

// ------------------------------------------------------------------ charts --

fn is_colored(rgb: [u8; 3]) -> bool {
    let (mx, mn) = (*rgb.iter().max().unwrap() as i32, *rgb.iter().min().unwrap() as i32);
    mx - mn >= 40
}

fn quantize(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0] / 32, rgb[1] / 32, rgb[2] / 32)
}

/// Vector charts. Two detectors, both on colored filled shapes:
/// * **bars**: rectangles chained left to right on a shared baseline (gaps up to 80 pt)
///   with at least three distinct heights;
/// * **clusters**: touching shapes with several colors (pie slices, areas, stacks),
///   excluding wide thin strips (striped table rows).
///
/// Raster charts cannot be seen here (`needs_pixel_pass` covers them).
fn vector_charts(paths: &[RawPath], tables: &[RegionHint]) -> Vec<RegionHint> {
    let colored: Vec<&RawPath> = paths
        .iter()
        .filter(|p| p.filled && p.fill_rgb.is_some_and(is_colored) && area(&p.bbox) >= 12.0)
        .collect();
    if colored.len() < 3 {
        return vec![];
    }
    let mut out: Vec<RegionHint> = Vec::new();

    // --- bars on a shared baseline
    let mut by_x: Vec<&RawPath> = colored.clone();
    by_x.sort_by(|a, b| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap());
    let mut used = vec![false; by_x.len()];
    for start in 0..by_x.len() {
        if used[start] {
            continue;
        }
        let base = by_x[start].bbox.y1;
        let mut chain = vec![start];
        let mut right = by_x[start].bbox.x1;
        for k in start + 1..by_x.len() {
            if used[k] || (by_x[k].bbox.y1 - base).abs() > 3.0 {
                continue;
            }
            if by_x[k].bbox.x0 - right > 80.0 {
                break;
            }
            chain.push(k);
            right = right.max(by_x[k].bbox.x1);
        }
        let members: Vec<&RawPath> = chain.iter().map(|&k| by_x[k]).collect();
        let heights = distinct(members.iter().map(|p| p.bbox.height()), 3.0);
        if members.len() >= 3 && heights >= 3 {
            let bbox = union_all(members.iter().map(|p| p.bbox)).unwrap();
            if bbox.width() >= 50.0 && bbox.height() >= 35.0 {
                let mut colors: Vec<(u8, u8, u8)> =
                    members.iter().filter_map(|p| p.fill_rgb).map(quantize).collect();
                colors.sort();
                colors.dedup();
                let score: f32 = 0.6
                    + if colors.len() >= 2 { 0.2 } else { 0.0 }
                    + if members.len() >= 5 { 0.1 } else { 0.0 };
                out.push(RegionHint {
                    bbox,
                    score: score.min(1.0),
                    why: format!(
                        "{} baseline-aligned bars, {} heights, {} colors",
                        members.len(),
                        heights,
                        colors.len()
                    ),
                });
                for &k in &chain {
                    used[k] = true;
                }
            }
        }
    }

    // --- touching multi-color clusters (pies, areas, stacks)
    let boxes: Vec<BBox> = colored.iter().map(|p| p.bbox).collect();
    for group in cluster(&boxes, 6.0) {
        if group.len() < 3 {
            continue;
        }
        let members: Vec<&RawPath> = group.iter().map(|&i| colored[i]).collect();
        let bbox = union_all(members.iter().map(|p| p.bbox)).unwrap();
        if bbox.width() < 50.0 || bbox.height() < 35.0 {
            continue;
        }
        let strips = members.iter().filter(|p| p.bbox.width() > 4.0 * p.bbox.height()).count();
        if strips * 5 >= members.len() * 4 {
            continue; // striped rows / banners, not a chart
        }
        let mut colors: Vec<(u8, u8, u8)> = members.iter().filter_map(|p| p.fill_rgb).map(quantize).collect();
        colors.sort();
        colors.dedup();
        if colors.len() < 2 {
            continue;
        }
        if out.iter().any(|o| intersect_area(&o.bbox, &bbox) > 0.5 * area(&bbox)) {
            continue; // already reported as bars
        }
        out.push(RegionHint {
            bbox,
            score: 0.5,
            why: format!("{} colored shapes, {} colors", members.len(), colors.len()),
        });
    }

    // Shaded table cells are not charts.
    out.retain(|c| !tables.iter().any(|t| intersect_area(&t.bbox, &c.bbox) > 0.7 * area(&c.bbox)));
    out
}

// ------------------------------------------------------------------- seals --

fn saturated(rgb: [u8; 3]) -> bool {
    let (mx, mn) = (*rgb.iter().max().unwrap() as i32, *rgb.iter().min().unwrap() as i32);
    mx - mn >= 70
}

fn seal_candidates(
    paths: &[RawPath],
    images: &[pipeline_core::RawImage],
    annotations: &[RawAnnotation],
    texts: &[RawText],
    page: &BBox,
) -> Vec<RegionHint> {
    let mut out: Vec<RegionHint> = Vec::new();
    for p in paths {
        let (w, h) = (p.bbox.width(), p.bbox.height());
        let squarish =
            (40.0..=200.0).contains(&w) && (40.0..=200.0).contains(&h) && (0.8..=1.25).contains(&(w / h));
        if squarish && p.stroked && p.curve_segments >= 4 && p.stroke_rgb.is_some_and(saturated) {
            let inside = texts.iter().filter(|t| t.chars > 0 && contains_center(&p.bbox, &t.bbox)).count();
            out.push(RegionHint {
                bbox: p.bbox,
                score: (0.7_f32 + if inside >= 2 { 0.1 } else { 0.0 }).min(1.0),
                why: "vector ring with saturated stroke".into(),
            });
        }
    }
    for im in images {
        let (w, h) = (im.bbox.width(), im.bbox.height());
        let squarish =
            (40.0..=200.0).contains(&w) && (40.0..=200.0).contains(&h) && (0.8..=1.25).contains(&(w / h));
        if squarish && area(&im.bbox) < 0.2 * area(page) {
            out.push(RegionHint { bbox: im.bbox, score: 0.4, why: "stamp-sized square image".into() });
        }
    }
    for a in annotations.iter().filter(|a| a.kind == "stamp") {
        out.push(RegionHint { bbox: a.bbox, score: 0.7, why: "stamp annotation".into() });
    }
    // Drop near-duplicates (nested rings of one seal): keep the highest score.
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut kept: Vec<RegionHint> = Vec::new();
    for c in out {
        let dup =
            kept.iter().any(|k| intersect_area(&k.bbox, &c.bbox) > 0.5 * area(&c.bbox).min(area(&k.bbox)));
        if !dup {
            kept.push(c);
        }
    }
    kept
}

// ---------------------------------------------------------------- classify --

/// Turn a raw object scan into a [`PageProfile`]. Pure function: no I/O.
pub fn classify(scan: &RawPageScan, width_pt: f32, height_pt: f32) -> PageProfile {
    let page = BBox { x0: 0.0, y0: 0.0, x1: width_pt, y1: height_pt };
    let visible_chars: usize = scan.texts.iter().filter(|t| !t.hidden).map(|t| t.chars).sum();
    let hidden_chars: usize = scan.texts.iter().filter(|t| t.hidden).map(|t| t.chars).sum();

    // Image coverage, clipped to the page.
    let page_area = area(&page).max(1.0);
    let coverage =
        (scan.images.iter().map(|i| intersect_area(&i.bbox, &page)).sum::<f32>() / page_area).min(1.0);

    let scan_kind = if coverage >= 0.85 || (coverage >= 0.5 && visible_chars + hidden_chars == 0) {
        if visible_chars >= 20 {
            ScanKind::No // live text over a background image (letterhead, watermark)
        } else if hidden_chars >= 20 {
            ScanKind::ScannedWithTextLayer
        } else {
            ScanKind::Scanned
        }
    } else {
        ScanKind::No
    };

    // Text pages of outlined glyphs: no text objects but a swarm of tiny paths.
    let tiny_paths = scan.paths.iter().filter(|p| p.bbox.width() < 30.0 && p.bbox.height() < 30.0).count();
    let outlined_text = visible_chars + hidden_chars == 0 && tiny_paths >= 150;

    let sample_text = if scan.text_sample.is_empty() { String::new() } else { scan.text_sample.clone() };
    let scripts = script_shares(&sample_text);
    let language = detect_language(&sample_text);

    let mut tables = ruled_tables(&scan.paths, &scan.texts, &page);
    // The same table found by two methods is reported once (ruled wins).
    let aligned: Vec<RegionHint> = aligned_tables(&scan.texts)
        .into_iter()
        .filter(|a| !tables.iter().any(|r| intersect_area(&r.bbox, &a.bbox) > 0.6 * area(&a.bbox)))
        .collect();
    tables.extend(aligned);
    let charts = vector_charts(&scan.paths, &tables);
    let seals = seal_candidates(&scan.paths, &scan.images, &scan.annotations, &scan.texts, &page);

    // "Ink" paths: stroked, or filled with something that is not near-white.
    let has_ink_path = scan
        .paths
        .iter()
        .any(|p| p.stroked || p.fill_rgb.map(|c| *c.iter().min().unwrap() < 240).unwrap_or(p.filled));
    let likely_blank = visible_chars + hidden_chars == 0
        && scan.images.is_empty()
        && scan.annotations.is_empty()
        && !has_ink_path
        && !scan.truncated;

    let big_images =
        scan.images.iter().filter(|i| i.bbox.width() >= 120.0 && i.bbox.height() >= 80.0).count();
    // Stamp-sized square images are only weak seal guesses: look at their pixels to confirm.
    let stamp_like_image = seals.iter().any(|s| s.why.contains("square image"));
    let needs_pixel_pass = scan_kind != ScanKind::No || outlined_text || big_images > 0 || stamp_like_image;

    let route_hint = if likely_blank {
        RouteHint::Skip
    } else if scan_kind == ScanKind::Scanned || outlined_text {
        RouteHint::Ocr
    } else if scan_kind == ScanKind::ScannedWithTextLayer {
        RouteHint::VerifyTextLayer
    } else {
        RouteHint::RustOnly
    };

    PageProfile {
        has_text: visible_chars > 0,
        visible_chars,
        hidden_chars,
        scripts,
        language,
        scan: scan_kind,
        image_count: scan.images.len(),
        image_coverage: coverage,
        images: scan.images.iter().map(|i| i.bbox).collect(),
        text_extent: union_all(scan.texts.iter().filter(|t| t.chars > 0).map(|t| t.bbox)),
        table_candidates: tables,
        chart_candidates: charts,
        seal_candidates: seals,
        text_objects: scan.texts.len(),
        path_objects: scan.paths.len(),
        annotation_count: scan.annotations.len(),
        needs_pixel_pass,
        likely_blank,
        route_hint,
        truncated: scan.truncated,
        scan_us: 0,
        pixel_pass_us: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::RawImage;

    fn bb(x0: f32, y0: f32, x1: f32, y1: f32) -> BBox {
        BBox { x0, y0, x1, y1 }
    }
    fn txt(x0: f32, y0: f32, w: f32, chars: usize) -> RawText {
        RawText { bbox: bb(x0, y0, x0 + w, y0 + 10.0), font_size: 10.0, hidden: false, chars }
    }
    fn line_h(x0: f32, x1: f32, y: f32) -> RawPath {
        RawPath {
            bbox: bb(x0, y, x1, y + 0.5),
            filled: true,
            stroked: false,
            fill_rgb: Some([0; 3]),
            stroke_rgb: None,
            curve_segments: 0,
        }
    }
    fn line_v(x: f32, y0: f32, y1: f32) -> RawPath {
        RawPath {
            bbox: bb(x, y0, x + 0.5, y1),
            filled: true,
            stroked: false,
            fill_rgb: Some([0; 3]),
            stroke_rgb: None,
            curve_segments: 0,
        }
    }
    fn rect(b: BBox, rgb: [u8; 3]) -> RawPath {
        RawPath {
            bbox: b,
            filled: true,
            stroked: false,
            fill_rgb: Some(rgb),
            stroke_rgb: None,
            curve_segments: 0,
        }
    }

    #[test]
    fn scripts_and_language() {
        let ar = "مرحبا بالعالم هذا اختبار للغة العربية في الوثائق الرسمية";
        let s = script_shares(ar);
        assert_eq!(s[0].0, "arabic");
        assert_eq!(detect_language(ar).unwrap().code, "ar");
        let fr = "Le chat dort sur le canapé pendant que les enfants jouent dans le jardin.";
        assert_eq!(detect_language(fr).unwrap().code, "fr");
        let en = "The quick brown fox jumps over the lazy dog while the sun sets slowly.";
        assert_eq!(detect_language(en).unwrap().code, "en");
        assert!(detect_language("Hi 12").is_none(), "too short to guess");
    }

    #[test]
    fn mixed_scripts_reported() {
        let s = script_shares("Hello world مرحبا بالعالم");
        assert!(s.iter().any(|(n, _)| n == "latin") && s.iter().any(|(n, _)| n == "arabic"));
    }

    #[test]
    fn scanned_page_detected() {
        let scan = RawPageScan {
            images: vec![RawImage { bbox: bb(0.0, 0.0, 595.0, 842.0), width_px: 1200, height_px: 1700 }],
            ..Default::default()
        };
        let p = classify(&scan, 595.0, 842.0);
        assert_eq!(p.scan, ScanKind::Scanned);
        assert_eq!(p.route_hint, RouteHint::Ocr);
        assert!(p.needs_pixel_pass && !p.has_text);
    }

    #[test]
    fn scan_with_hidden_text_layer() {
        let mut hidden = txt(50.0, 50.0, 200.0, 80);
        hidden.hidden = true;
        let scan = RawPageScan {
            images: vec![RawImage { bbox: bb(0.0, 0.0, 595.0, 842.0), width_px: 1200, height_px: 1700 }],
            texts: vec![hidden],
            ..Default::default()
        };
        let p = classify(&scan, 595.0, 842.0);
        assert_eq!(p.scan, ScanKind::ScannedWithTextLayer);
        assert_eq!(p.route_hint, RouteHint::VerifyTextLayer);
        assert!(!p.has_text, "hidden text is not visible text");
    }

    #[test]
    fn live_text_over_background_image_is_not_a_scan() {
        let scan = RawPageScan {
            images: vec![RawImage { bbox: bb(0.0, 0.0, 595.0, 842.0), width_px: 600, height_px: 800 }],
            texts: vec![txt(50.0, 50.0, 300.0, 120)],
            ..Default::default()
        };
        assert_eq!(classify(&scan, 595.0, 842.0).scan, ScanKind::No);
    }

    #[test]
    fn empty_page_is_blank_but_annotation_only_page_is_not() {
        let empty = RawPageScan::default();
        assert!(classify(&empty, 595.0, 842.0).likely_blank);
        let white_bg = RawPageScan {
            paths: vec![rect(bb(0.0, 0.0, 595.0, 842.0), [255, 255, 255])],
            ..Default::default()
        };
        assert!(classify(&white_bg, 595.0, 842.0).likely_blank, "white background fill is not ink");
        let stamp = RawPageScan {
            annotations: vec![RawAnnotation { bbox: bb(100.0, 100.0, 200.0, 200.0), kind: "stamp".into() }],
            ..Default::default()
        };
        let p = classify(&stamp, 595.0, 842.0);
        assert!(!p.likely_blank);
        assert_eq!(p.seal_candidates.len(), 1);
    }

    #[test]
    fn outlined_text_routes_to_ocr() {
        let paths: Vec<RawPath> =
            (0..200).map(|i| rect(bb(10.0 + i as f32, 10.0, 15.0 + i as f32, 20.0), [0, 0, 0])).collect();
        let p = classify(&RawPageScan { paths, ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.route_hint, RouteHint::Ocr);
    }

    #[test]
    fn ruled_table_found_with_position() {
        let mut paths = Vec::new();
        for r in 0..5 {
            paths.push(line_h(100.0, 400.0, 100.0 + r as f32 * 20.0));
        }
        for c in 0..4 {
            paths.push(line_v(100.0 + c as f32 * 100.0, 100.0, 180.0));
        }
        let texts: Vec<RawText> = (0..12)
            .map(|i| txt(105.0 + (i % 3) as f32 * 100.0, 105.0 + (i / 3) as f32 * 20.0, 40.0, 5))
            .collect();
        let p = classify(&RawPageScan { paths, texts, ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.table_candidates.len(), 1, "{:?}", p.table_candidates);
        let t = &p.table_candidates[0];
        assert!(
            t.score > 0.8
                && t.bbox.x0 <= 100.5
                && t.bbox.x1 >= 400.0
                && t.bbox.y0 <= 100.5
                && t.bbox.y1 >= 180.0
        );
    }

    #[test]
    fn page_border_alone_is_not_a_table() {
        let paths = vec![
            line_h(10.0, 585.0, 10.0),
            line_h(10.0, 585.0, 830.0),
            line_v(10.0, 10.0, 830.0),
            line_v(585.0, 10.0, 830.0),
        ];
        let p = classify(&RawPageScan { paths, ..Default::default() }, 595.0, 842.0);
        assert!(p.table_candidates.is_empty());
    }

    #[test]
    fn borderless_aligned_table_found_but_prose_is_not() {
        let mut t = Vec::new();
        for r in 0..5 {
            for c in 0..4 {
                t.push(txt(72.0 + c as f32 * 110.0, 100.0 + r as f32 * 16.0, 60.0, 8));
            }
        }
        let p = classify(&RawPageScan { texts: t, ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.table_candidates.len(), 1, "{:?}", p.table_candidates);
        assert!(p.table_candidates[0].why.contains("aligned"));

        // Prose: one text object per line -> no multi-cell rows.
        let prose: Vec<RawText> = (0..30).map(|r| txt(72.0, 100.0 + r as f32 * 14.0, 450.0, 80)).collect();
        assert!(classify(&RawPageScan { texts: prose, ..Default::default() }, 595.0, 842.0)
            .table_candidates
            .is_empty());
    }

    #[test]
    fn padded_table_rows_far_apart_still_found() {
        // Glyph boxes ~9 pt tall, 12 pt font, rows 28 pt apart (cell padding).
        let mut t = Vec::new();
        for r in 0..5 {
            for c in 0..4 {
                let mut x = txt(57.0 + c as f32 * 30.0, 59.0 + r as f32 * 28.0, 15.0, 3);
                x.bbox.y1 = x.bbox.y0 + 9.0;
                x.font_size = 12.0;
                t.push(x);
            }
        }
        let p = classify(&RawPageScan { texts: t, ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.table_candidates.len(), 1, "{:?}", p.table_candidates);
    }

    #[test]
    fn bar_chart_found_and_shaded_table_rows_are_not() {
        let heights = [30.0, 60.0, 45.0, 80.0];
        let cols = [[200, 60, 60], [60, 120, 200], [60, 180, 90], [230, 170, 40]];
        let bars: Vec<RawPath> = heights
            .iter()
            .enumerate()
            .map(|(i, h)| {
                rect(bb(100.0 + i as f32 * 30.0, 300.0 - h, 120.0 + i as f32 * 30.0, 300.0), cols[i])
            })
            .collect();
        let p = classify(&RawPageScan { paths: bars, ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.chart_candidates.len(), 1, "{:?}", p.chart_candidates);
        assert!(p.chart_candidates[0].score >= 0.8);

        // Same-colored equal-height strips: decoration, not a chart.
        let strips: Vec<RawPath> = (0..5)
            .map(|i| {
                rect(bb(100.0, 100.0 + i as f32 * 20.0, 300.0, 115.0 + i as f32 * 20.0), [200, 220, 255])
            })
            .collect();
        assert!(classify(&RawPageScan { paths: strips, ..Default::default() }, 595.0, 842.0)
            .chart_candidates
            .is_empty());
    }

    #[test]
    fn vector_seal_and_square_stamp_image() {
        let ring = RawPath {
            bbox: bb(300.0, 600.0, 400.0, 700.0),
            filled: false,
            stroked: true,
            fill_rgb: None,
            stroke_rgb: Some([200, 20, 30]),
            curve_segments: 8,
        };
        let black_ring = RawPath { stroke_rgb: Some([20, 20, 20]), ..ring.clone() };
        let p = classify(&RawPageScan { paths: vec![ring], ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.seal_candidates.len(), 1);
        assert!(classify(&RawPageScan { paths: vec![black_ring], ..Default::default() }, 595.0, 842.0)
            .seal_candidates
            .is_empty());

        let img = RawImage { bbox: bb(300.0, 600.0, 390.0, 690.0), width_px: 200, height_px: 200 };
        let p = classify(&RawPageScan { images: vec![img], ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.seal_candidates.len(), 1);
        assert_eq!(p.scan, ScanKind::No);
    }

    #[test]
    fn stamp_sized_square_image_asks_for_pixel_verification() {
        let img = RawImage { bbox: bb(300.0, 600.0, 390.0, 690.0), width_px: 200, height_px: 200 };
        let p = classify(&RawPageScan { images: vec![img], ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.seal_candidates.len(), 1);
        assert!(p.needs_pixel_pass, "weak image-based seal guess must be verified on pixels");
    }

    #[test]
    fn nested_rings_deduplicated() {
        let ring = |s: f32| RawPath {
            bbox: bb(300.0 + s, 600.0 + s, 400.0 - s, 700.0 - s),
            filled: false,
            stroked: true,
            fill_rgb: None,
            stroke_rgb: Some([30, 30, 200]),
            curve_segments: 4,
        };
        let p =
            classify(&RawPageScan { paths: vec![ring(0.0), ring(5.0)], ..Default::default() }, 595.0, 842.0);
        assert_eq!(p.seal_candidates.len(), 1);
    }
}
