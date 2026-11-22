//! Exporters: turn the [`Document`] model into JSON or README-style Markdown.
//! JSON is the full-fidelity source of truth; Markdown is a readable view.

use pipeline_core::{BlockKind, Document, Verdict};

pub fn to_json(doc: &Document) -> String {
    serde_json::to_string_pretty(doc).expect("document is serializable")
}

#[derive(Default)]
pub struct MarkdownOptions {
    /// Emit bbox / geometry as HTML comments on each block.
    pub with_provenance: bool,
}

fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn to_markdown(doc: &Document, opts: &MarkdownOptions) -> String {
    let m = &doc.metadata;
    let mut out = String::new();

    // YAML front matter
    out.push_str("---\n");
    out.push_str(&format!("source: {}\n", yaml_str(&doc.source)));
    for (k, v) in [
        ("title", &m.title),
        ("author", &m.author),
        ("subject", &m.subject),
        ("keywords", &m.keywords),
        ("creator", &m.creator),
        ("producer", &m.producer),
        ("created", &m.creation_date),
        ("modified", &m.modification_date),
        ("pdf_version", &m.pdf_version),
    ] {
        if let Some(v) = v {
            out.push_str(&format!("{k}: {}\n", yaml_str(v)));
        }
    }
    out.push_str(&format!("pages: {}\n---\n\n", m.page_count));

    let title = m.title.clone().unwrap_or_else(|| {
        std::path::Path::new(&doc.source)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Document".into())
    });
    out.push_str(&format!("# {title}\n\n"));

    let blank: Vec<usize> =
        doc.pages.iter().filter(|p| p.blank.as_ref().is_some_and(|b| b.is_blank)).map(|p| p.number).collect();
    if !blank.is_empty() {
        let list: Vec<String> = blank.iter().map(|n| n.to_string()).collect();
        out.push_str(&format!("> Blank pages detected: {}\n\n", list.join(", ")));
    }

    for p in &doc.pages {
        out.push_str(&format!("<!-- page {} -->\n", p.number));
        out.push_str(&format!("## Page {}\n\n", p.number));
        if opts.with_provenance {
            out.push_str(&format!(
                "<!-- page size {:.1}x{:.1} pt, rotation {} -->\n\n",
                p.geometry.width_pt, p.geometry.height_pt, p.geometry.rotation
            ));
        }
        if p.blank.as_ref().is_some_and(|b| b.is_blank) {
            out.push_str("*(blank page)*\n\n");
            continue;
        }
        if let Some(q) = &p.quality {
            if q.verdict != Verdict::Good {
                out.push_str(&format!(
                    "> ⚠ Image quality {:?} (readability {:.2}){}\n\n",
                    q.verdict,
                    q.readability,
                    if q.notes.is_empty() { String::new() } else { format!(": {}", q.notes.join("; ")) }
                ));
            }
        }
        if let Some(o) = &p.orientation {
            if o.correction_cw != 0 {
                out.push_str(&format!(
                    "> ↻ Page content needs {}° clockwise rotation to read upright ({}, confidence {:.2}{}).\n\n",
                    o.correction_cw,
                    o.method,
                    o.confidence,
                    if o.ambiguous { "; direction ambiguous, could be the opposite quarter turn" } else { "" }
                ));
            }
        }
        if let Some(o) = &p.ocr {
            if o.mean_confidence < 0.85 || o.low_confidence_lines > 0 {
                out.push_str(&format!(
                    "> ⚠ OCR text (mean confidence {:.2}, {} of {} lines below 0.80): check before relying on it.\n\n",
                    o.mean_confidence, o.low_confidence_lines, o.lines
                ));
            }
            if opts.with_provenance {
                out.push_str(&format!(
                    "<!-- ocr engine={} dpi={} lines={} conf={:.3} models={:?} rotated_cw={} -->\n\n",
                    o.engine, o.dpi, o.lines, o.mean_confidence, o.models, o.rotated_cw
                ));
            }
        }
        for b in &p.blocks {
            if opts.with_provenance {
                out.push_str(&format!(
                    "<!-- p{} {:?} bbox=[{:.1},{:.1},{:.1},{:.1}] src={:?} -->\n",
                    p.number, b.kind, b.bbox.x0, b.bbox.y0, b.bbox.x1, b.bbox.y1, b.source
                ));
            }
            match b.kind {
                BlockKind::Heading => {
                    let hashes = "#".repeat(2 + b.level.unwrap_or(1) as usize);
                    out.push_str(&format!("{hashes} {}\n\n", b.text));
                }
                BlockKind::List => {
                    for item in b.text.lines() {
                        let t = item.trim_start_matches(['-', '*', '•', '–']).trim_start();
                        let numbered = item.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit());
                        if numbered {
                            out.push_str(&format!("{}\n", item.trim()));
                        } else {
                            out.push_str(&format!("- {t}\n"));
                        }
                    }
                    out.push('\n');
                }
                BlockKind::Figure => match &b.image_path {
                    Some(path) => out.push_str(&format!("![Figure on page {}]({path})\n\n", p.number)),
                    None => out.push_str(&format!("*[figure: {}]*\n\n", b.text)),
                },
                _ => out.push_str(&format!("{}\n\n", b.text)),
            }
        }
        if p.blocks.is_empty() {
            if let Some(path) = p.render.as_ref().and_then(|r| r.image_path.as_ref()) {
                out.push_str(&format!("![Page {}]({path})\n\n", p.number));
            } else {
                out.push_str("*(no text layer content extracted)*\n\n");
            }
        }
    }

    out.push_str("---\n\n");
    out.push_str(&format!(
        "_Processed with `{}`: {} pages in {:.0} ms wall, {:.0} ms CPU, peak RSS {:.1} MiB._\n",
        doc.backend,
        doc.stats.page_count,
        doc.stats.wall_ms,
        doc.stats.cpu_ms,
        doc.stats.peak_rss_kb as f64 / 1024.0
    ));
    out
}
