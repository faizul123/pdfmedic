use ocr_oar::{OarConfig, OarEngine};
use pipeline_core::image;
use pipeline_core::OcrEngine;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut cfg = OarConfig::new(&a[1]);
    if let Ok(v) = std::env::var("REC_BATCH") {
        cfg.rec_batch = v.parse().unwrap();
    }
    if let Ok(v) = std::env::var("MEM_PATTERN") {
        cfg.memory_pattern = v == "1";
    }
    if let Ok(v) = std::env::var("THREADS") {
        cfg.intra_threads = Some(v.parse().unwrap());
    }
    let eng = OarEngine::new(&cfg).unwrap();
    println!("engine: {}", eng.name());
    let img = image::open(&a[2]).unwrap().to_rgb8();
    if a.get(3).map(|s| s.as_str()) == Some("orient") {
        // rotate clockwise by k*90 and ask the classifier what correction is needed
        for k in 0..4u32 {
            let r = match k {
                0 => img.clone(),
                1 => image::imageops::rotate90(&img),
                2 => image::imageops::rotate180(&img),
                _ => image::imageops::rotate270(&img),
            };
            let g = eng.page_orientation(&r).unwrap();
            println!(
                "page rotated {:>3} deg clockwise -> classifier correction_cw = {:?}",
                k * 90,
                g.map(|g| (g.correction_cw, format!("top {:.2} margin {:.2}", g.confidence, g.margin)))
            );
        }
        return;
    }
    if let Ok(n) = std::env::var("LOOP") {
        let rss = || {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|l| l.starts_with("VmRSS"))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u64>()
                .unwrap()
                / 1024
        };
        let paths: Vec<&String> = a[2..].iter().collect();
        println!("start RSS {} MiB", rss());
        for i in 0..n.parse::<usize>().unwrap() {
            let p = paths[i % paths.len()];
            let im = image::open(p).unwrap().to_rgb8();
            let t = Instant::now();
            let o = eng.recognize(&im).unwrap();
            println!(
                "iter {i:>2} {:<28} {} lines {:>5.0} ms  RSS {} MiB",
                p.rsplit('/').next().unwrap(),
                o.lines.len(),
                t.elapsed().as_secs_f64() * 1000.0,
                rss()
            );
        }
        return;
    }
    let t = Instant::now();
    let o = eng.recognize(&img).unwrap();
    println!(
        "total {:.0} ms (detect {:.0}, recognize {:.0})",
        t.elapsed().as_secs_f64() * 1000.0,
        o.detect_ms,
        o.recognize_ms
    );
    for l in &o.lines {
        println!(
            "  {:.2} {:<8} [{:.0},{:.0},{:.0},{:.0}] {}",
            l.confidence, l.model, l.bbox.x0, l.bbox.y0, l.bbox.x1, l.bbox.y1, l.text
        );
    }
}
