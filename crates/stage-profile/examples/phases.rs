use pipeline_core::{PdfBackend, ScanOptions};
use std::time::Instant;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let b = backend_pdfium::PdfiumBackend::from_dir("libs/pdfium/lib").unwrap();
    let doc = b.open(std::path::Path::new(&path)).unwrap();
    let n = doc.page_count().min(300);
    let opts = ScanOptions::default();
    let (mut geom, mut scan, mut classify) = (0u128, 0u128, 0u128);
    let mut raws = Vec::new();
    for i in 0..n {
        let t = Instant::now();
        let g = doc.page_geometry(i).unwrap();
        geom += t.elapsed().as_micros();
        let t = Instant::now();
        let s = doc.page_scan(i, &opts).unwrap();
        scan += t.elapsed().as_micros();
        raws.push((s, g));
    }
    let t = Instant::now();
    for (s, g) in &raws {
        let _ = stage_profile::classify(s, g.width_pt, g.height_pt);
    }
    classify += t.elapsed().as_micros();
    let t = Instant::now();
    for (s, _) in &raws {
        let _ = stage_profile::detect_language(&s.text_sample);
    }
    let lang = t.elapsed().as_micros();
    println!("{} pages: geometry {:.2} ms/pg | page_scan {:.2} ms/pg | classify {:.3} ms/pg (of which language {:.3})",
        n, geom as f64 / 1000.0 / n as f64, scan as f64 / 1000.0 / n as f64, classify as f64 / 1000.0 / n as f64, lang as f64 / 1000.0 / n as f64);
    let (s, _) = &raws[10];
    println!(
        "page 11: {} text objs, {} paths, {} images, sample {} chars",
        s.texts.len(),
        s.paths.len(),
        s.images.len(),
        s.text_sample.len()
    );
}
