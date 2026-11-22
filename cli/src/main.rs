use anyhow::{Context, Result};
use backend_pdfium::PdfiumBackend;
use clap::{Parser, Subcommand};
use export::MarkdownOptions;
use pipeline_core::Pipeline;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "pdfpipe", about = "Rust PDF document-intelligence pipeline")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fast per-page triage (text, language, scan, tables, charts, seals) with no rendering.
    Profile {
        pdf: PathBuf,
        /// Directory containing libpdfium.so.
        #[arg(long, env = "PDFIUM_LIB_DIR", default_value = "libs/pdfium/lib")]
        pdfium_dir: PathBuf,
        /// Page range, 1-based inclusive, e.g. 1-50 (default: all pages).
        #[arg(long)]
        pages: Option<String>,
        /// Write full per-page profiles as JSON to this file.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Also print the position of every candidate region.
        #[arg(long)]
        verbose: bool,
        /// Run the low-resolution pixel pass on pages the triage flags (scans, big images).
        #[arg(long)]
        pixels: bool,
        /// Render DPI for the pixel pass.
        #[arg(long, default_value_t = 72.0)]
        px_dpi: f32,
    },
    /// Run the pipeline on a PDF.
    Run {
        pdf: PathBuf,
        /// Output formats, comma separated: json,md
        #[arg(long, default_value = "json,md", value_delimiter = ',')]
        format: Vec<String>,
        /// Output directory.
        #[arg(long, default_value = "out")]
        out: PathBuf,
        /// Directory containing libpdfium.so.
        #[arg(long, env = "PDFIUM_LIB_DIR", default_value = "libs/pdfium/lib")]
        pdfium_dir: PathBuf,
        /// Render DPI for analysis.
        #[arg(long, default_value_t = 100.0)]
        dpi: f32,
        /// Max pixels on the longest side of a rendered page.
        #[arg(long, default_value_t = 4096)]
        max_dim: u32,
        /// Save page images under <out>/assets.
        #[arg(long)]
        save_images: bool,
        /// Add bbox/geometry HTML comments to the Markdown.
        #[arg(long)]
        with_provenance: bool,
        /// Run the pixel pass on pages the profile flags (scans, big images).
        #[arg(long)]
        pixels: bool,
        /// OCR pages the profile routes to OCR (needs the PP-OCR models in --models-dir).
        #[arg(long)]
        ocr: bool,
        /// Directory with the PP-OCR ONNX models and dictionaries.
        #[arg(long, env = "OCR_MODELS_DIR", default_value = "models")]
        models_dir: PathBuf,
        /// Render DPI for OCR.
        #[arg(long, default_value_t = 150.0)]
        ocr_dpi: f32,
    },
}

fn parse_range(spec: &str, total: usize) -> Result<std::ops::Range<usize>> {
    let (a, b) = match spec.split_once('-') {
        Some((a, b)) => (a.trim().parse::<usize>()?, b.trim().parse::<usize>()?),
        None => {
            let n = spec.trim().parse::<usize>()?;
            (n, n)
        }
    };
    anyhow::ensure!(a >= 1 && a <= b && a <= total, "page range {spec} is outside 1-{total}");
    Ok(a - 1..b.min(total))
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Profile { pdf, pdfium_dir, pages, json, verbose, pixels, px_dpi } => {
            use pipeline_core::PdfBackend;
            let wall = std::time::Instant::now();
            let backend = PdfiumBackend::from_dir(&pdfium_dir)
                .with_context(|| format!("loading PDFium from {}", pdfium_dir.display()))?;
            let doc = backend.open(&pdf).context("opening PDF")?;
            let total = doc.page_count();
            let range = match &pages {
                Some(spec) => parse_range(spec, total)?,
                None => 0..total,
            };
            let scan_opts = pipeline_core::ScanOptions::default();
            let profiles = if pixels {
                let px = stage_pixels::PixelOptions { dpi: px_dpi, ..Default::default() };
                stage_pixels::profile_pages_with_pixels(doc.as_ref(), range, &scan_opts, &px)?
            } else {
                stage_profile::profile_pages(doc.as_ref(), range, &scan_opts)?
            };
            let elapsed = wall.elapsed();

            println!(
                "{:>4} {:<7} {:>6} {:<12} {:<9} {:>3} {:>3} {:>3} {:>3} {:>3} {:>7}",
                "page", "route", "chars", "lang/script", "scan", "img", "tbl", "cht", "sel", "ann", "us"
            );
            for (n, p) in &profiles {
                let lang = match (&p.language, p.scripts.first()) {
                    (Some(l), _) => {
                        if l.reliable {
                            l.code.clone()
                        } else {
                            format!("{}?", l.code)
                        }
                    }
                    (None, Some((s, _))) => format!("({s})"),
                    _ => "-".into(),
                };
                let route = format!("{:?}", p.route_hint);
                println!(
                    "{:>4} {:<7} {:>6} {:<12} {:<9} {:>3} {:>3} {:>3} {:>3} {:>3} {:>7}{}",
                    n,
                    route,
                    p.visible_chars,
                    lang,
                    format!("{:?}", p.scan),
                    p.image_count,
                    p.table_candidates.len(),
                    p.chart_candidates.len(),
                    p.seal_candidates.len(),
                    p.annotation_count,
                    p.scan_us,
                    match (p.pixel_pass_us, p.needs_pixel_pass) {
                        (Some(us), _) => format!("  px:{:.1}ms", us as f64 / 1000.0),
                        (None, true) => "  *pixel-pass".to_string(),
                        _ => String::new(),
                    }
                );
                if verbose {
                    for (label, v) in [
                        ("table", &p.table_candidates),
                        ("chart", &p.chart_candidates),
                        ("seal", &p.seal_candidates),
                    ] {
                        for h in v {
                            println!(
                                "       {label} {:.2} [{:.0},{:.0},{:.0},{:.0}] {}",
                                h.score, h.bbox.x0, h.bbox.y0, h.bbox.x1, h.bbox.y1, h.why
                            );
                        }
                    }
                }
            }
            let n = profiles.len().max(1);
            let sum_us: u64 = profiles.iter().map(|(_, p)| p.scan_us).sum();
            let px_pages: Vec<u64> = profiles.iter().filter_map(|(_, p)| p.pixel_pass_us).collect();
            if !px_pages.is_empty() {
                println!(
                    "\npixel pass ran on {} pages: {:.1} ms/page average (render included), {:.1} ms max",
                    px_pages.len(),
                    px_pages.iter().sum::<u64>() as f64 / 1000.0 / px_pages.len() as f64,
                    *px_pages.iter().max().unwrap() as f64 / 1000.0
                );
            }
            println!(
                "\n{} of {} pages profiled in {:.1} ms wall ({:.2} ms/page profiling, incl. open); peak RSS {:.1} MiB",
                profiles.len(),
                total,
                elapsed.as_secs_f64() * 1000.0,
                sum_us as f64 / 1000.0 / n as f64,
                pipeline_core::metrics::peak_rss_kb() as f64 / 1024.0
            );
            if let Some(path) = json {
                let v: Vec<_> =
                    profiles.iter().map(|(n, p)| serde_json::json!({"page": n, "profile": p})).collect();
                std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
                println!("wrote {}", path.display());
            }
        }
        Cmd::Run {
            pdf,
            format,
            out,
            pdfium_dir,
            dpi,
            max_dim,
            save_images,
            with_provenance,
            pixels,
            ocr,
            models_dir,
            ocr_dpi,
        } => {
            std::fs::create_dir_all(&out)?;
            let backend = PdfiumBackend::from_dir(&pdfium_dir)
                .with_context(|| format!("loading PDFium from {}", pdfium_dir.display()))?;

            let mut pipeline = Pipeline::new()
                .with(stage_metadata::MetadataStage)
                .with(stage_profile::ProfileStage::default());
            if pixels {
                pipeline = pipeline.with(stage_pixels::PixelPassStage::default());
            }
            let pipeline = pipeline
                .with(stage_render::RenderStage { dpi, max_dim, save_dir: save_images.then(|| out.clone()) })
                .with(stage_blank::BlankStage::default())
                .with(stage_quality::QualityStage::default())
                .with(stage_orient::OrientStage::default())
                .with(stage_extract::ExtractStage {
                    save_dir: save_images.then(|| out.clone()),
                    ..Default::default()
                });

            let pipeline = if ocr {
                let engine = ocr_oar::OarEngine::new(&ocr_oar::OarConfig::new(&models_dir))
                    .with_context(|| format!("loading OCR models from {}", models_dir.display()))?;
                let mut st = stage_ocr::OcrStage::new(Box::new(engine));
                st.dpi = ocr_dpi;
                pipeline.with(st)
            } else {
                pipeline
            };

            let doc = pipeline.run(&backend, &pdf).context("pipeline failed")?;

            let stem = pdf
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".into());
            for f in &format {
                match f.as_str() {
                    "json" => {
                        let p = out.join(format!("{stem}.json"));
                        std::fs::write(&p, export::to_json(&doc))?;
                        println!("wrote {}", p.display());
                    }
                    "md" | "markdown" => {
                        let p = out.join(format!("{stem}.md"));
                        std::fs::write(&p, export::to_markdown(&doc, &MarkdownOptions { with_provenance }))?;
                        println!("wrote {}", p.display());
                    }
                    other => anyhow::bail!("unknown format `{other}` (use json, md)"),
                }
            }
            println!(
                "{} pages | wall {:.0} ms | cpu {:.0} ms | peak RSS {:.1} MiB",
                doc.stats.page_count,
                doc.stats.wall_ms,
                doc.stats.cpu_ms,
                doc.stats.peak_rss_kb as f64 / 1024.0
            );
            for (stage, ms) in &doc.stats.stage_ms {
                println!("  {stage:<10} {ms:>9.1} ms");
            }
        }
    }
    Ok(())
}
