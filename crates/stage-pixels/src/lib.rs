//! Pixel pass: look inside rasters that object-level triage cannot see.
//!
//! Runs on pages the profile flagged `needs_pixel_pass` (scans, outlined text, large
//! embedded images), on one cheap low-resolution render. Three detectors:
//!
//! * **Seals** - saturated-color blobs shaped like stamps: roughly square, outline-like
//!   (low fill ratio, ink on all four sides), 30-230 pt. Solid logos (high fill), wide
//!   thin strokes (signatures) and photos are rejected.
//! * **Raster charts** - regions of a few *flat* saturated colors (top-6 color bins cover
//!   most colored pixels, 2+ dominant colors) that are also locally uniform (85%+ of
//!   neighbors match). Photographs spread over many colors or jitter pixel to pixel and are
//!   rejected. Gray/black-only charts and single-color bar charts are NOT detected.
//! * **Tables** - long horizontal and vertical dark lines, fed to the same grid test as
//!   vector tables. Needs reasonably straight rules (skew breaks the runs).
//!
//! All candidates are heuristic with a score; `why` is prefixed `pixels:`. On pages with
//! live content the analysis is limited to the image regions.

use pipeline_core::image::DynamicImage;
use pipeline_core::{BBox, PageProfile, PdfDoc, RawPath, RegionHint, Result, ScanOptions, Stage};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct PixelOptions {
    /// Render resolution for the analysis (points -> pixels = dpi / 72).
    pub dpi: f32,
    /// Cap on the longest side in pixels.
    pub max_dim: u32,
}

impl Default for PixelOptions {
    fn default() -> Self {
        Self { dpi: 72.0, max_dim: 1400 }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Findings {
    pub seals: Vec<RegionHint>,
    pub charts: Vec<RegionHint>,
    pub tables: Vec<RegionHint>,
}

// ---------------------------------------------------------------- primitives --

const COLOR_SPREAD: i32 = 60; // max-min channel spread that counts as "saturated ink"

fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    // Separable box dilation with prefix sums: O(n).
    let mut tmp = vec![false; w * h];
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        let mut pre = vec![0u32; w + 1];
        for x in 0..w {
            pre[x + 1] = pre[x] + row[x] as u32;
        }
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(w));
            tmp[y * w + x] = pre[b] > pre[a];
        }
    }
    let mut out = vec![false; w * h];
    let mut pre = vec![0u32; h + 1];
    for x in 0..w {
        for y in 0..h {
            pre[y + 1] = pre[y] + tmp[y * w + x] as u32;
        }
        for y in 0..h {
            let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
            out[y * w + x] = pre[b] > pre[a];
        }
    }
    out
}

/// Connected components (label 1..=n; 0 = background).
fn label(mask: &[bool], w: usize, h: usize, eight: bool) -> (Vec<u32>, u32) {
    let mut labels = vec![0u32; w * h];
    let mut next = 0u32;
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        next += 1;
        labels[start] = next;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    if (dx == 0 && dy == 0) || (!eight && dx != 0 && dy != 0) {
                        continue;
                    }
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && labels[j] == 0 {
                        labels[j] = next;
                        stack.push(j);
                    }
                }
            }
        }
    }
    (labels, next)
}

fn bin512(r: u8, g: u8, b: u8) -> usize {
    ((r >> 5) as usize) << 6 | ((g >> 5) as usize) << 3 | (b >> 5) as usize
}

fn iou(a: &BBox, b: &BBox) -> f32 {
    let w = a.x1.min(b.x1) - a.x0.max(b.x0);
    let h = a.y1.min(b.y1) - a.y0.max(b.y0);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    inter / (a.width() * a.height() + b.width() * b.height() - inter)
}

/// Keep the outermost of nested candidates (the two rings of one seal are separate blobs).
fn drop_nested(mut v: Vec<RegionHint>) -> Vec<RegionHint> {
    v.sort_by(|a, b| {
        (b.bbox.width() * b.bbox.height()).partial_cmp(&(a.bbox.width() * a.bbox.height())).unwrap()
    });
    let mut kept: Vec<RegionHint> = Vec::new();
    for c in v {
        let area = c.bbox.width() * c.bbox.height();
        let nested = kept.iter().any(|k| {
            let w = k.bbox.x1.min(c.bbox.x1) - k.bbox.x0.max(c.bbox.x0);
            let h = k.bbox.y1.min(c.bbox.y1) - k.bbox.y0.max(c.bbox.y0);
            w > 0.0 && h > 0.0 && w * h >= 0.8 * area
        });
        if !nested {
            kept.push(c);
        }
    }
    kept
}

// ------------------------------------------------------------------ analysis --

/// Analyse one rendered page. `regions` (points, displayed frame) limits the analysis to
/// those areas; `None` analyses the whole page.
pub fn analyze(img: &DynamicImage, page_w: f32, page_h: f32, regions: Option<&[BBox]>) -> Findings {
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let raw = rgb.as_raw();
    let n = w * h;
    if n == 0 || page_w <= 0.0 {
        return Findings::default();
    }
    let k = w as f32 / page_w; // pixels per point
    let to_pt = |x: f32| x / k;

    // Region mask.
    let in_region: Option<Vec<bool>> = regions.map(|rs| {
        let mut m = vec![false; n];
        for r in rs {
            let (x0, x1) = ((r.x0 * k).floor().max(0.0) as usize, ((r.x1 * k).ceil() as usize).min(w));
            let (y0, y1) = ((r.y0 * k).floor().max(0.0) as usize, ((r.y1 * k).ceil() as usize).min(h));
            for y in y0..y1 {
                m[y * w + x0..y * w + x1.max(x0)].fill(true);
            }
        }
        m
    });

    // Per-pixel luma and "saturated ink".
    let mut luma = vec![255u8; n];
    let mut colored = vec![false; n];
    for i in 0..n {
        if let Some(m) = &in_region {
            if !m[i] {
                continue;
            }
        }
        let (r, g, b) = (raw[3 * i], raw[3 * i + 1], raw[3 * i + 2]);
        let (mx, mn) = (r.max(g).max(b) as i32, r.min(g).min(b) as i32);
        let l = ((54 * r as u32 + 183 * g as u32 + 19 * b as u32) >> 8) as u8;
        luma[i] = l;
        colored[i] = mx - mn >= COLOR_SPREAD && l <= 235;
    }

    let mut out = Findings::default();
    out.seals = drop_nested(find_seals(raw, &colored, w, h, k));
    out.charts = find_charts(raw, &colored, w, h, k, &out.seals);
    out.tables = find_tables(&luma, &colored, w, h, k, page_w, page_h);
    let _ = to_pt;
    out
}

fn find_seals(_raw: &[u8], colored: &[bool], w: usize, h: usize, k: f32) -> Vec<RegionHint> {
    let dil = dilate(colored, w, h, 2);
    let (labels, count) = label(&dil, w, h, false);
    if count == 0 {
        return vec![];
    }
    #[derive(Clone)]
    struct C {
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        n: usize,
        r: u64,
        g: u64,
        b: u64,
    }
    let mut comps =
        vec![C { x0: usize::MAX, y0: usize::MAX, x1: 0, y1: 0, n: 0, r: 0, g: 0, b: 0 }; count as usize];
    for i in 0..w * h {
        let l = labels[i];
        if l == 0 || !colored[i] {
            continue;
        }
        let c = &mut comps[l as usize - 1];
        let (x, y) = (i % w, i / w);
        c.x0 = c.x0.min(x);
        c.x1 = c.x1.max(x);
        c.y0 = c.y0.min(y);
        c.y1 = c.y1.max(y);
        c.n += 1;
        c.r += _raw[3 * i] as u64;
        c.g += _raw[3 * i + 1] as u64;
        c.b += _raw[3 * i + 2] as u64;
    }
    let mut out = Vec::new();
    for (ci, c) in comps.iter().enumerate() {
        if c.n < 60 {
            continue;
        }
        let (bw, bh) = ((c.x1 - c.x0 + 1) as f32, (c.y1 - c.y0 + 1) as f32);
        let (wpt, hpt) = (bw / k, bh / k);
        let aspect = bw / bh;
        if !(30.0..=230.0).contains(&wpt) || !(30.0..=230.0).contains(&hpt) || !(0.7..=1.45).contains(&aspect)
        {
            continue;
        }
        let fill = c.n as f32 / (bw * bh);
        if !(0.04..=0.4).contains(&fill) {
            continue; // solid logo or sparse scribble
        }
        // Outline evidence: ink near each of the four sides of the box.
        let band = ((0.12 * bw.min(bh)) as usize).max(3);
        let id = ci as u32 + 1;
        let occ = |horizontal: bool, far: bool| -> f32 {
            let len = if horizontal { c.x1 - c.x0 + 1 } else { c.y1 - c.y0 + 1 };
            let mut hit = 0usize;
            for t in 0..len {
                let mut any = false;
                for d in 0..band {
                    let (x, y) = if horizontal {
                        (c.x0 + t, if far { c.y1 - d } else { c.y0 + d })
                    } else {
                        (if far { c.x1 - d } else { c.x0 + d }, c.y0 + t)
                    };
                    let i = y * w + x;
                    if labels[i] == id && colored[i] {
                        any = true;
                        break;
                    }
                }
                hit += any as usize;
            }
            hit as f32 / len as f32
        };
        let sides = [occ(true, false), occ(true, true), occ(false, false), occ(false, true)];
        let closed = sides.iter().filter(|&&s| s >= 0.3).count();
        if closed < 3 {
            continue;
        }
        // Outline seals have a mostly empty center (text aside); photos and blobs do not.
        let (cx0, cx1) = (c.x0 + (bw * 0.3) as usize, c.x1 - (bw * 0.3) as usize);
        let (cy0, cy1) = (c.y0 + (bh * 0.3) as usize, c.y1 - (bh * 0.3) as usize);
        let mut center_ink = 0usize;
        for y in cy0..=cy1.max(cy0) {
            for x in cx0..=cx1.max(cx0) {
                center_ink += (labels[y * w + x] == id && colored[y * w + x]) as usize;
            }
        }
        let center_area = ((cx1.max(cx0) - cx0 + 1) * (cy1.max(cy0) - cy0 + 1)) as f32;
        if center_ink as f32 / center_area > 0.2 {
            continue;
        }
        let (mr, mg, mb) = (c.r as f32 / c.n as f32, c.g as f32 / c.n as f32, c.b as f32 / c.n as f32);
        let hue_family = mr > mg + 40.0 && mr > mb + 40.0 || mb > mr + 40.0 && mb > mg + 20.0;
        let mut score =
            0.5 + 0.2 * (closed == 4) as u8 as f32 + 0.15 * (0.06..=0.3).contains(&fill) as u8 as f32;
        if hue_family {
            score += 0.15;
        }
        out.push(RegionHint {
            bbox: BBox {
                x0: c.x0 as f32 / k,
                y0: c.y0 as f32 / k,
                x1: (c.x1 + 1) as f32 / k,
                y1: (c.y1 + 1) as f32 / k,
            },
            score: score.min(1.0),
            why: format!("pixels: outline-like colored blob, fill {:.2}, {} of 4 sides closed", fill, closed),
        });
    }
    out
}

fn find_charts(
    raw: &[u8],
    colored: &[bool],
    w: usize,
    h: usize,
    k: f32,
    seals: &[RegionHint],
) -> Vec<RegionHint> {
    const CELL: usize = 8;
    let (gw, gh) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cell_n = vec![0u16; gw * gh];
    for y in 0..h {
        for x in 0..w {
            if colored[y * w + x] {
                cell_n[(y / CELL) * gw + x / CELL] += 1;
            }
        }
    }
    let active: Vec<bool> = cell_n.iter().map(|&c| c >= 8).collect();
    let grown = dilate(&active, gw, gh, 2);
    let (cl, count) = label(&grown, gw, gh, true);
    if count == 0 {
        return vec![];
    }
    let mut hist = vec![[0u32; 512]; count as usize];
    let mut bbox = vec![(usize::MAX, usize::MAX, 0usize, 0usize); count as usize];
    for cy in 0..gh {
        for cx in 0..gw {
            let c = cy * gw + cx;
            if !active[c] {
                continue;
            }
            let b = &mut bbox[cl[c] as usize - 1];
            b.0 = b.0.min(cx * CELL);
            b.1 = b.1.min(cy * CELL);
            b.2 = b.2.max(((cx + 1) * CELL).min(w));
            b.3 = b.3.max(((cy + 1) * CELL).min(h));
        }
    }
    // Per component: how many colored pixels have a right neighbor of (nearly) the same
    // color. Chart fills are uniform; photo pixels jitter even when their palette is narrow.
    let mut smooth = vec![(0u32, 0u32); count as usize];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !colored[i] {
                continue;
            }
            let c = (y / CELL) * gw + x / CELL;
            if active[c] {
                let id = cl[c] as usize - 1;
                hist[id][bin512(raw[3 * i], raw[3 * i + 1], raw[3 * i + 2])] += 1;
                if x + 1 < w && colored[i + 1] {
                    let near =
                        (0..3).all(|ch| (raw[3 * i + ch] as i32 - raw[3 * (i + 1) + ch] as i32).abs() <= 12);
                    smooth[id].0 += near as u32;
                    smooth[id].1 += 1;
                }
            }
        }
    }
    let mut out = Vec::new();
    for (ci, hst) in hist.iter().enumerate() {
        let (x0, y0, x1, y1) = bbox[ci];
        if x0 == usize::MAX {
            continue;
        }
        let (wpt, hpt) = ((x1 - x0) as f32 / k, (y1 - y0) as f32 / k);
        let total: u32 = hst.iter().sum();
        if wpt < 70.0 || hpt < 45.0 || total < 1200 {
            continue;
        }
        let mut sorted: Vec<u32> = hst.iter().copied().filter(|&c| c > 0).collect();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let top6: u32 = sorted.iter().take(6).sum();
        let flat = top6 as f32 / total as f32;
        let dominant = sorted.iter().filter(|&&c| c as f32 / total as f32 >= 0.08).count();
        if flat < 0.75 || dominant < 2 {
            continue; // photo / gradient / single-color blob
        }
        let (near, pairs) = smooth[ci];
        if pairs == 0 || (near as f32 / pairs as f32) < 0.85 {
            continue; // pixel-level jitter: a photograph, not flat chart fills
        }
        let bb = BBox { x0: x0 as f32 / k, y0: y0 as f32 / k, x1: x1 as f32 / k, y1: y1 as f32 / k };
        if seals.iter().any(|s| iou(&s.bbox, &bb) > 0.4) {
            continue;
        }
        let score = 0.45
            + 0.2 * (dominant >= 3) as u8 as f32
            + 0.2 * (flat >= 0.85) as u8 as f32
            + 0.1 * (wpt >= 100.0 && hpt >= 70.0) as u8 as f32;
        out.push(RegionHint {
            bbox: bb,
            score: score.min(1.0),
            why: format!("pixels: {dominant} flat colors, top-6 bins {:.0}% of colored pixels", flat * 100.0),
        });
    }
    out
}

/// Runs of `true` along one axis with up to `gap` pixels of tolerance, at least `min_len` long.
fn runs(line: impl Iterator<Item = bool>, gap: usize, min_len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut start, mut last, mut open) = (0usize, 0usize, false);
    for (i, v) in line.enumerate() {
        if v {
            if !open {
                start = i;
                open = true;
            }
            last = i;
        } else if open && i - last > gap {
            if last - start + 1 >= min_len {
                out.push((start, last));
            }
            open = false;
        }
    }
    if open && last - start + 1 >= min_len {
        out.push((start, last));
    }
    out
}

/// Merge segments of consecutive rows/columns (thick lines) that overlap along their length.
/// Input: (cross position, from, to). Output: (cross0, cross1, from, to).
fn merge_segments(mut segs: Vec<(usize, usize, usize)>) -> Vec<(usize, usize, usize, usize)> {
    segs.sort();
    let mut lines: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (c, a, b) in segs {
        let mut merged = false;
        for l in lines.iter_mut().rev() {
            if c > l.1 + 1 {
                continue;
            }
            let overlap = b.min(l.3) as isize - a.max(l.2) as isize;
            if c <= l.1 + 1 && overlap > 0 && overlap as f32 >= 0.6 * ((b - a).min(l.3 - l.2)) as f32 {
                l.1 = c;
                l.2 = l.2.min(a);
                l.3 = l.3.max(b);
                merged = true;
                break;
            }
        }
        if !merged {
            lines.push((c, c, a, b));
        }
    }
    lines
}

type Seg = ((f32, f32), (f32, f32));

/// Dilate by one pixel along one axis (`horizontal`: left/right, else up/down).
fn dilate1(mask: &[bool], w: usize, h: usize, horizontal: bool) -> Vec<bool> {
    let mut out = mask.to_vec();
    if horizontal {
        for y in 0..h {
            for x in 0..w {
                if mask[y * w + x] {
                    if x > 0 {
                        out[y * w + x - 1] = true;
                    }
                    if x + 1 < w {
                        out[y * w + x + 1] = true;
                    }
                }
            }
        }
    } else {
        for y in 0..h {
            for x in 0..w {
                if mask[y * w + x] {
                    if y > 0 {
                        out[(y - 1) * w + x] = true;
                    }
                    if y + 1 < h {
                        out[(y + 1) * w + x] = true;
                    }
                }
            }
        }
    }
    out
}

/// Thin horizontal runs along scanlines tilted by slope `t` (dy per dx), as endpoints
/// `((x0, y0), (x1, y1))` in image pixels. Runs are found on `tol` (the mask dilated one pixel
/// vertically): a small angle error otherwise lets the scanline drift off a 1 px rule
/// (measured: 0.1 deg lost the far end of a 350 px rule). `mask` (undilated) is used for the thin
/// test: runs whose neighbours 5 px above/below are mostly dark (filled regions, photos) are
/// dropped. Untilted scans skip rows that hold too little ink to contain a run.
fn scan_h(mask: &[bool], tol: &[bool], w: usize, h: usize, t: f32, min_len: usize, gap: usize) -> Vec<Seg> {
    const OFF: i32 = 5;
    let at = |x: i32, y: i32| {
        x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && mask[y as usize * w + x as usize]
    };
    let drift = (w as f32 * t.abs()).ceil() as i32 + 1;
    let mut out = Vec::new();
    for r in -drift..h as i32 + drift {
        if t == 0.0
            && (r < 0
                || r >= h as i32
                || tol[r as usize * w..(r as usize + 1) * w].iter().filter(|&&v| v).count() < min_len)
        {
            continue;
        }
        let y_at = |x: usize| (r as f32 + x as f32 * t).round() as i32;
        let hit = |x: usize| {
            let y = y_at(x);
            y >= 0 && (y as usize) < h && tol[y as usize * w + x]
        };
        for (a, b) in runs((0..w).map(hit), gap, min_len) {
            let bad = (a..=b).filter(|&x| at(x as i32, y_at(x) - OFF) || at(x as i32, y_at(x) + OFF)).count();
            if (bad as f32) < 0.3 * (b - a + 1) as f32 {
                out.push(((a as f32, y_at(a) as f32), (b as f32, y_at(b) as f32)));
            }
        }
    }
    out
}

/// Thin vertical runs along columns tilted so that `x = c - y * t` (see [`scan_h`]).
fn scan_v(mask: &[bool], tol: &[bool], w: usize, h: usize, t: f32, min_len: usize, gap: usize) -> Vec<Seg> {
    const OFF: i32 = 5;
    let at = |x: i32, y: i32| {
        x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && mask[y as usize * w + x as usize]
    };
    let drift = (h as f32 * t.abs()).ceil() as i32 + 1;
    let col_ink: Vec<usize> = if t == 0.0 {
        let mut c = vec![0usize; w];
        for y in 0..h {
            for (x, cell) in c.iter_mut().enumerate() {
                *cell += tol[y * w + x] as usize;
            }
        }
        c
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    for c in -drift..w as i32 + drift {
        if t == 0.0 && (c < 0 || c >= w as i32 || col_ink[c as usize] < min_len) {
            continue;
        }
        let x_at = |y: usize| (c as f32 - y as f32 * t).round() as i32;
        let hit = |y: usize| {
            let x = x_at(y);
            x >= 0 && (x as usize) < w && tol[y * w + x as usize]
        };
        for (a, b) in runs((0..h).map(hit), gap, min_len) {
            let bad = (a..=b).filter(|&y| at(x_at(y) - OFF, y as i32) || at(x_at(y) + OFF, y as i32)).count();
            if (bad as f32) < 0.3 * (b - a + 1) as f32 {
                out.push(((x_at(a) as f32, a as f32), (x_at(b) as f32, b as f32)));
            }
        }
    }
    out
}

/// Skew in degrees (+-3.5 searched) estimated from the short thin line pieces found at 0 deg.
/// A tilted rule is a staircase of short pieces; for the right slope `t` their projections
/// `y - x*t` (rows) and `x + y*t` (columns) pile up in few bins. Text is mostly excluded because
/// only *thin* runs are used. 0 when no tilt explains the pieces clearly better than none.
fn skew_from_pieces(hs: &[Seg], vs: &[Seg]) -> f32 {
    let score = |deg: f32| -> f64 {
        let t = deg.to_radians().tan();
        let mut rows: std::collections::HashMap<i32, f64> = Default::default();
        let mut cols: std::collections::HashMap<i32, f64> = Default::default();
        for ((x0, y0), (x1, y1)) in hs {
            let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            *rows.entry((ym - xm * t).round() as i32).or_default() += (x1 - x0) as f64;
        }
        for ((x0, y0), (x1, y1)) in vs {
            let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            *cols.entry((xm + ym * t).round() as i32).or_default() += (y1 - y0) as f64;
        }
        rows.values().chain(cols.values()).map(|v| v * v).sum()
    };
    let flat = score(0.0);
    let (mut best_deg, mut best) = (0.0f32, flat);
    for i in -35..=35 {
        let deg = i as f32 * 0.1;
        let sc = score(deg);
        if sc > best {
            best = sc;
            best_deg = deg;
        }
    }
    if best > flat * 1.3 {
        best_deg
    } else {
        0.0
    }
}

fn find_tables(
    luma: &[u8],
    colored: &[bool],
    w: usize,
    h: usize,
    k: f32,
    page_w: f32,
    page_h: f32,
) -> Vec<RegionHint> {
    // Adaptive dark threshold from the page's own paper and ink levels.
    let mut hist = [0u64; 256];
    for &l in luma {
        hist[l as usize] += 1;
    }
    let total = luma.len() as u64;
    let pct = |q: f64| {
        let t = (total as f64 * q) as u64;
        let mut acc = 0;
        for (v, c) in hist.iter().enumerate() {
            acc += c;
            if acc > t {
                return v as i32;
            }
        }
        255
    };
    let (paper, ink) = (pct(0.98), pct(0.002));
    if paper - ink < 60 {
        return vec![];
    }
    let thr = ((paper + ink) / 2) as u8;
    let mask: Vec<bool> = (0..w * h).map(|i| luma[i] < thr && !colored[i]).collect();
    let tol_h = dilate1(&mask, w, h, false); // for horizontal rules: +-1 px vertically
    let tol_v = dilate1(&mask, w, h, true); // for vertical rules: +-1 px horizontally

    let min_h = ((0.15 * w as f32) as usize).max(60);
    let min_v = ((0.05 * h as f32) as usize).max(40);

    // Grid test in the straightened frame for rules found at tilt `deg`.
    let detect = |deg: f32, hs: Vec<Seg>, vs: Vec<Seg>| -> Vec<RegionHint> {
        let (sin, cos) = deg.to_radians().sin_cos();
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        // Original pixel -> straightened pixel (rotate by -deg about the center).
        let straight =
            |(x, y): (f32, f32)| (cx + (x - cx) * cos + (y - cy) * sin, cy - (x - cx) * sin + (y - cy) * cos);
        let hs: Vec<(usize, usize, usize)> = hs
            .into_iter()
            .map(|(a, b)| (straight(a), straight(b)))
            .map(|(a, b)| {
                (
                    ((a.1 + b.1) / 2.0).round().max(0.0) as usize,
                    a.0.round().max(0.0) as usize,
                    b.0.round().max(0.0) as usize,
                )
            })
            .collect();
        let vs: Vec<(usize, usize, usize)> = vs
            .into_iter()
            .map(|(a, b)| (straight(a), straight(b)))
            .map(|(a, b)| {
                (
                    ((a.0 + b.0) / 2.0).round().max(0.0) as usize,
                    a.1.round().max(0.0) as usize,
                    b.1.round().max(0.0) as usize,
                )
            })
            .collect();
        let hl = merge_segments(hs);
        let vl = merge_segments(vs);
        if hl.len() < 3 || vl.len() < 2 || hl.len() + vl.len() > 80 {
            return vec![];
        }
        let thin = |x0: f32, y0: f32, x1: f32, y1: f32| RawPath {
            bbox: BBox { x0, y0, x1, y1 },
            filled: true,
            stroked: false,
            fill_rgb: Some([0, 0, 0]),
            stroke_rgb: None,
            curve_segments: 0,
        };
        let mut paths = Vec::new();
        for (c0, c1, a, b) in &hl {
            let y = (*c0 + *c1) as f32 / 2.0 / k;
            paths.push(thin(*a as f32 / k, y - 0.5, (*b + 1) as f32 / k, y + 0.5));
        }
        for (c0, c1, a, b) in &vl {
            let x = (*c0 + *c1) as f32 / 2.0 / k;
            paths.push(thin(x - 0.5, *a as f32 / k, x + 0.5, (*b + 1) as f32 / k));
        }
        let page = BBox { x0: 0.0, y0: 0.0, x1: page_w, y1: page_h };
        stage_profile::ruled_tables_padded(&paths, &[], &page, 5.0)
            .into_iter()
            .map(|mut tb| {
                if deg != 0.0 {
                    // Corners from the straightened frame back to the original (skewed) page.
                    let (cxp, cyp) = (page_w / 2.0, page_h / 2.0);
                    let map = |x: f32, y: f32| {
                        (cxp + (x - cxp) * cos - (y - cyp) * sin, cyp + (x - cxp) * sin + (y - cyp) * cos)
                    };
                    let b = tb.bbox;
                    let pts = [map(b.x0, b.y0), map(b.x1, b.y0), map(b.x1, b.y1), map(b.x0, b.y1)];
                    tb.bbox = BBox {
                        x0: pts.iter().map(|p| p.0).fold(f32::MAX, f32::min),
                        y0: pts.iter().map(|p| p.1).fold(f32::MAX, f32::min),
                        x1: pts.iter().map(|p| p.0).fold(f32::MIN, f32::max),
                        y1: pts.iter().map(|p| p.1).fold(f32::MIN, f32::max),
                    };
                    tb.why = format!("pixels: {} (deskewed {:+.1} deg)", tb.why, deg);
                } else {
                    tb.why = format!("pixels: {}", tb.why);
                }
                tb
            })
            .collect()
    };

    // One scan for short pieces (>= 15 px); the long rules are the long ones among them.
    let pieces_h = scan_h(&mask, &tol_h, w, h, 0.0, 15, 2);
    let pieces_v = scan_v(&mask, &tol_v, w, h, 0.0, 15, 2);
    let len = |s: &Seg| ((s.1 .0 - s.0 .0).abs().max((s.1 .1 - s.0 .1).abs())) as usize + 1;
    let long_h: Vec<Seg> = pieces_h.iter().filter(|s| len(s) >= min_h).cloned().collect();
    let long_v: Vec<Seg> = pieces_v.iter().filter(|s| len(s) >= min_v).cloned().collect();
    if long_h.len() >= 3 && long_v.len() >= 2 {
        let upright = detect(0.0, long_h, long_v);
        if !upright.is_empty() {
            return upright;
        }
    }
    // Nothing straight. Only pay for the tilted search when there is line-like evidence: a
    // tilted rule shows up as a staircase of many short pieces.
    if pieces_h.len() + pieces_v.len() < 12 {
        return vec![];
    }
    let skew = skew_from_pieces(&pieces_h, &pieces_v);
    if skew.abs() < 0.3 {
        return vec![];
    }
    let t = skew.to_radians().tan();
    detect(skew, scan_h(&mask, &tol_h, w, h, t, min_h, 2), scan_v(&mask, &tol_v, w, h, t, min_v, 2))
}

// --------------------------------------------------------------------- stage --

/// Run the pixel pass for one page and merge the findings into its profile.
/// Returns without rendering when the profile does not ask for a pixel pass.
pub fn apply(
    doc: &dyn PdfDoc,
    idx: usize,
    profile: &mut PageProfile,
    page_w: f32,
    page_h: f32,
    opts: &PixelOptions,
) -> Result<()> {
    if !profile.needs_pixel_pass {
        return Ok(());
    }
    let t = Instant::now();
    let img = doc.render_page(idx, opts.dpi, opts.max_dim)?;
    // Scans and outlined text are the whole page; live pages: only inside image regions.
    let whole =
        profile.scan != pipeline_core::ScanKind::No || profile.text_objects == 0 && profile.image_count == 0;
    let regions: Vec<BBox> = profile.images.clone();
    let f = analyze(&img, page_w, page_h, if whole { None } else { Some(&regions) });
    // Weak object-level guesses (stamp-sized square images) are settled by the pixels: confirmed
    // ones are replaced by the pixel candidate; unconfirmed ones stay but are demoted, because
    // this detector only sees saturated ink (a black or gray stamp would not show up).
    let confirmed = |weak: &RegionHint| f.seals.iter().any(|px| iou(&px.bbox, &weak.bbox) > 0.3);
    profile.seal_candidates.retain(|c| !(c.why.contains("square image") && confirmed(c)));
    for c in profile.seal_candidates.iter_mut().filter(|c| c.why.contains("square image")) {
        c.score = (c.score * 0.6).min(0.25);
        if !c.why.contains("not confirmed") {
            c.why.push_str(" (not confirmed by pixels)");
        }
    }
    profile.seal_candidates.extend(f.seals);
    profile.chart_candidates.extend(f.charts);
    profile.table_candidates.extend(f.tables);
    profile.pixel_pass_us = Some(t.elapsed().as_micros() as u64);
    Ok(())
}

/// Triage a page range and run the pixel pass on flagged pages.
pub fn profile_pages_with_pixels(
    doc: &dyn PdfDoc,
    pages: std::ops::Range<usize>,
    scan_opts: &ScanOptions,
    px: &PixelOptions,
) -> Result<Vec<(usize, PageProfile)>> {
    let mut out = stage_profile::profile_pages(doc, pages, scan_opts)?;
    for (n, p) in out.iter_mut() {
        let g = doc.page_geometry(*n - 1)?;
        apply(doc, *n - 1, p, g.width_pt, g.height_pt, px)?;
    }
    Ok(out)
}

/// Pipeline stage: pixel pass for pages whose profile asks for it (place after `ProfileStage`).
#[derive(Default)]
pub struct PixelPassStage {
    pub opts: PixelOptions,
}

impl Stage for PixelPassStage {
    fn name(&self) -> &'static str {
        "pixels"
    }

    fn run_page(&self, doc: &dyn PdfDoc, ctx: &mut pipeline_core::PageContext) -> Result<()> {
        let g = ctx.page.geometry;
        let idx = ctx.page.number - 1;
        if let Some(p) = ctx.page.profile.as_mut() {
            apply(doc, idx, p, g.width_pt, g.height_pt, &self.opts)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::image::{Rgb, RgbImage};

    const W: u32 = 450; // 1 px = 1 pt when the page is 450 pt wide
    const H: u32 = 600;

    fn page() -> RgbImage {
        RgbImage::from_pixel(W, H, Rgb([255, 255, 255]))
    }
    fn rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, c: [u8; 3]) {
        for y in y0..y1.min(H) {
            for x in x0..x1.min(W) {
                img.put_pixel(x, y, Rgb(c));
            }
        }
    }
    fn ring(img: &mut RgbImage, cx: f32, cy: f32, r: f32, t: f32, c: [u8; 3]) {
        for y in 0..H {
            for x in 0..W {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                if d <= r && d >= r - t {
                    img.put_pixel(x, y, Rgb(c));
                }
            }
        }
    }
    fn run(img: &RgbImage) -> Findings {
        analyze(&DynamicImage::ImageRgb8(img.clone()), W as f32, H as f32, None)
    }
    const RED: [u8; 3] = [200, 25, 35];

    #[test]
    fn ring_seal_found_with_position() {
        let mut im = page();
        ring(&mut im, 300.0, 400.0, 55.0, 4.0, RED);
        ring(&mut im, 300.0, 400.0, 45.0, 2.0, RED);
        let f = run(&im);
        assert_eq!(f.seals.len(), 1, "{:?}", f.seals);
        let b = f.seals[0].bbox;
        assert!(
            (b.x0 - 245.0).abs() < 4.0 && (b.x1 - 356.0).abs() < 4.0 && (b.y0 - 345.0).abs() < 4.0,
            "{b:?}"
        );
        assert!(f.charts.is_empty() && f.tables.is_empty());
    }

    #[test]
    fn rings_far_apart_are_one_seal() {
        // Gap between rings wider than the dilation radius: two blobs, one seal.
        let mut im = page();
        ring(&mut im, 300.0, 400.0, 55.0, 4.0, RED);
        ring(&mut im, 300.0, 400.0, 42.0, 2.0, RED);
        let f = run(&im);
        assert_eq!(f.seals.len(), 1, "{:?}", f.seals);
        assert!((f.seals[0].bbox.x1 - f.seals[0].bbox.x0) > 105.0, "outer ring kept");
    }

    #[test]
    fn square_stamp_found() {
        let mut im = page();
        rect(&mut im, 100, 300, 200, 400, RED);
        rect(&mut im, 105, 305, 195, 395, [255, 255, 255]);
        assert_eq!(run(&im).seals.len(), 1);
    }

    #[test]
    fn solid_logo_signature_and_photo_are_not_seals() {
        let mut im = page();
        for y in 0..H {
            for x in 0..W {
                let d = ((x as f32 - 300.0).powi(2) + (y as f32 - 400.0).powi(2)).sqrt();
                if d <= 45.0 {
                    im.put_pixel(x, y, Rgb(RED)); // solid disc
                }
            }
        }
        assert!(run(&im).seals.is_empty(), "solid disc");

        let mut sig = page();
        for i in 0..180u32 {
            let y = 300 + ((i as f32 / 6.0).sin() * 8.0) as u32;
            rect(&mut sig, 100 + i, y, 102 + i, y + 2, [30, 60, 190]);
        }
        assert!(run(&sig).seals.is_empty(), "signature");

        let mut photo = page();
        let mut s = 12345u32;
        for y in 300..440 {
            for x in 100..300 {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = (s >> 24) as i32 % 50;
                photo.put_pixel(x, y, Rgb([(120 + n) as u8, (60 + x as i32 % 90) as u8, (150 - n) as u8]));
            }
        }
        let f = run(&photo);
        assert!(f.seals.is_empty() && f.charts.is_empty(), "photo: {:?}", f);
    }

    #[test]
    fn flat_color_bar_chart_found_but_single_color_blob_is_not() {
        let mut im = page();
        let cols = [[51, 102, 204], [220, 57, 18], [16, 150, 24], [255, 153, 0], [153, 0, 153]];
        for (i, h) in [60u32, 110, 85, 130, 45].iter().enumerate() {
            let x = 100 + i as u32 * 40;
            rect(&mut im, x, 400 - h, x + 28, 400, cols[i]);
        }
        let f = run(&im);
        assert_eq!(f.charts.len(), 1, "{:?}", f.charts);
        assert!(f.charts[0].bbox.x0 <= 104.0 && f.charts[0].bbox.x1 >= 260.0);

        let mut one = page();
        rect(&mut one, 100, 300, 260, 420, [51, 102, 204]);
        assert!(run(&one).charts.is_empty());
    }

    #[test]
    fn ruled_table_found() {
        let mut im = page();
        for r in 0..6 {
            rect(&mut im, 60, 200 + r * 24, 340, 201 + r * 24, [0, 0, 0]);
        }
        for c in 0..5 {
            rect(&mut im, 60 + c * 70, 200, 61 + c * 70, 320, [0, 0, 0]);
        }
        let f = run(&im);
        assert_eq!(f.tables.len(), 1, "{:?}", f.tables);
        assert!(f.tables[0].why.starts_with("pixels:"));
    }

    fn skew_image(src: &RgbImage, deg: f32) -> RgbImage {
        let (s, c) = deg.to_radians().sin_cos();
        let mut out = page();
        for dy in 0..H {
            for dx in 0..W {
                let (rx, ry) = (dx as f32 - W as f32 / 2.0, dy as f32 - H as f32 / 2.0);
                let (sx, sy) =
                    ((W as f32 / 2.0 + rx * c - ry * s).round(), (H as f32 / 2.0 + rx * s + ry * c).round());
                if sx >= 0.0 && sy >= 0.0 && sx < W as f32 && sy < H as f32 {
                    out.put_pixel(dx, dy, *src.get_pixel(sx as u32, sy as u32));
                }
            }
        }
        out
    }

    fn table_page() -> RgbImage {
        let mut im = page();
        for r in 0..6 {
            rect(&mut im, 60, 200 + r * 24, 340, 201 + r * 24, [0, 0, 0]);
        }
        for c in 0..5 {
            rect(&mut im, 60 + c * 70, 200, 61 + c * 70, 320, [0, 0, 0]);
        }
        // Some text so the page is not just a grid.
        for line in 0..8u32 {
            for x in (40..400).step_by(9) {
                rect(&mut im, x, 380 + line * 20, x + 5, 390 + line * 20, [0, 0, 0]);
            }
        }
        im
    }

    #[test]
    fn upright_table_is_not_deskewed() {
        let f = run(&table_page());
        assert_eq!(f.tables.len(), 1, "{:?}", f.tables);
        assert!(!f.tables[0].why.contains("deskewed"));
    }

    #[test]
    fn skewed_tables_found_at_several_angles() {
        for deg in [1.0f32, 1.5, 2.0, -1.5, 2.5] {
            let f = run(&skew_image(&table_page(), deg));
            assert_eq!(f.tables.len(), 1, "{deg} deg: {:?}", f.tables);
            let b = f.tables[0].bbox;
            assert!(b.x0 < 75.0 && b.x1 > 325.0 && b.y0 < 220.0 && b.y1 > 300.0, "{deg} deg: {b:?}");
        }
    }

    #[test]
    fn text_like_noise_and_blank_find_nothing() {
        assert!(run(&page()).seals.is_empty());
        let mut im = page();
        for line in 0..20u32 {
            for x in (40..400).step_by(9) {
                rect(&mut im, x, 60 + line * 20, x + 5, 70 + line * 20, [0, 0, 0]);
            }
        }
        let f = run(&im);
        assert!(f.seals.is_empty() && f.charts.is_empty() && f.tables.is_empty(), "{f:?}");
    }

    #[test]
    fn region_restriction_ignores_content_outside() {
        let mut im = page();
        ring(&mut im, 300.0, 400.0, 55.0, 4.0, RED);
        let inside = [BBox { x0: 200.0, y0: 300.0, x1: 400.0, y1: 500.0 }];
        let outside = [BBox { x0: 0.0, y0: 0.0, x1: 150.0, y1: 150.0 }];
        let d = DynamicImage::ImageRgb8(im);
        assert_eq!(analyze(&d, W as f32, H as f32, Some(&inside)).seals.len(), 1);
        assert!(analyze(&d, W as f32, H as f32, Some(&outside)).seals.is_empty());
    }
}
