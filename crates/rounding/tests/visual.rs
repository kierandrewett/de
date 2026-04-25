//! Visual export tests.
//!
//! These tests generate SVG files in `tests/output/` for visual inspection.
//! They are always run (non-ignored) since they have no side-effects beyond
//! writing files to `tests/output/`.

use std::fs;
use std::path::Path;

use rounding::{SquircleConfig, SquirclePath};

/// Generate an SVG with squircles at smoothing 0.0, 0.2, 0.4, 0.6, 0.8, 1.0
/// side by side. Write to `tests/output/squircle_comparison.svg`.
#[test]
fn visual_smoothing_comparison() {
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/output");
    fs::create_dir_all(&output_dir).expect("create output dir");

    let smoothings = [0.0_f32, 0.2, 0.4, 0.6, 0.8, 1.0];
    let rect_w = 120.0_f32;
    let rect_h = 80.0_f32;
    let radius = 20.0_f32;
    let gap = 20.0_f32;
    let pad = 20.0_f32;

    let total_w = pad * 2.0 + smoothings.len() as f32 * (rect_w + gap) - gap;
    let total_h = pad * 2.0 + rect_h + 40.0;

    let mut body = String::new();
    body.push_str(&format!(
        "<rect width=\"{total_w}\" height=\"{total_h}\" fill=\"#f8f8f8\"/>\n"
    ));

    for (i, &xi) in smoothings.iter().enumerate() {
        let x = pad + i as f32 * (rect_w + gap);
        let y = pad;

        let cfg = SquircleConfig::new(radius, xi);
        let path = SquirclePath::new(x, y, rect_w, rect_h, cfg);
        let d = path.to_svg_path_data();

        body.push_str(&format!(
            "<path d=\"{d}\" fill=\"rgba(60,120,220,0.18)\" stroke=\"#3060c0\" stroke-width=\"1.5\"/>\n"
        ));
        body.push_str(&format!(
            "<text x=\"{cx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"11\" fill=\"#333\" text-anchor=\"middle\">xi={xi:.1}</text>\n",
            cx = x + rect_w / 2.0,
            ty = y + rect_h + 18.0,
        ));
    }

    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total_w}\" height=\"{total_h}\" viewBox=\"0 0 {total_w} {total_h}\">\n{body}</svg>\n"
    );

    let out_path = output_dir.join("squircle_comparison.svg");
    fs::write(&out_path, svg).expect("write SVG file");
    println!("Visual output written to: {}", out_path.display());
}

/// Generate an SVG showing per-corner different radii.
#[test]
fn visual_per_corner_radii() {
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/output");
    fs::create_dir_all(&output_dir).expect("create output dir");

    let rect_w = 160.0_f32;
    let rect_h = 100.0_f32;
    let pad = 30.0_f32;
    let total_w = rect_w + pad * 2.0;
    let total_h = rect_h + pad * 2.0 + 30.0;

    let tl = SquircleConfig::new(5.0, 0.0);
    let tr = SquircleConfig::new(15.0, 0.6);
    let br = SquircleConfig::new(30.0, 1.0);
    let bl = SquircleConfig::new(20.0, 0.3);

    let path = SquirclePath::with_corners(pad, pad, rect_w, rect_h, tl, tr, br, bl);
    let d = path.to_svg_path_data();

    let mut body = String::new();
    body.push_str(&format!(
        "<rect width=\"{total_w}\" height=\"{total_h}\" fill=\"#f8f8f8\"/>\n"
    ));
    body.push_str(&format!(
        "<path d=\"{d}\" fill=\"rgba(220,80,60,0.18)\" stroke=\"#c03020\" stroke-width=\"1.5\"/>\n"
    ));
    body.push_str(&format!(
        "<text x=\"{pad}\" y=\"14\" font-family=\"monospace\" font-size=\"10\" fill=\"#333\">TL: r=5 xi=0.0</text>\n"
    ));
    body.push_str(&format!(
        "<text x=\"{tw}\" y=\"14\" font-family=\"monospace\" font-size=\"10\" fill=\"#333\" text-anchor=\"end\">TR: r=15 xi=0.6</text>\n",
        tw = total_w - pad,
    ));
    body.push_str(&format!(
        "<text x=\"{tw}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"10\" fill=\"#333\" text-anchor=\"end\">BR: r=30 xi=1.0</text>\n",
        tw = total_w - pad,
        ty = total_h - 5.0,
    ));
    body.push_str(&format!(
        "<text x=\"{pad}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"10\" fill=\"#333\">BL: r=20 xi=0.3</text>\n",
        ty = total_h - 5.0,
    ));

    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total_w}\" height=\"{total_h}\">\n{body}</svg>\n"
    );

    let out_path = output_dir.join("per_corner_radii.svg");
    fs::write(&out_path, svg).expect("write SVG file");
    println!("Per-corner visual written to: {}", out_path.display());
}

/// Generate an SVG comparing standard rounded rect vs squircle at same radius.
#[test]
fn visual_standard_vs_squircle() {
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/output");
    fs::create_dir_all(&output_dir).expect("create output dir");

    let rect_w = 120.0_f32;
    let rect_h = 80.0_f32;
    let radius = 25.0_f32;
    let pad = 20.0_f32;
    let gap = 20.0_f32;
    let total_w = pad * 2.0 + rect_w * 2.0 + gap;
    let total_h = pad * 2.0 + rect_h + 25.0;

    let standard_cfg = SquircleConfig::new(radius, 0.0);
    let squircle_cfg = SquircleConfig::new(radius, 0.6);

    let std_path = SquirclePath::new(pad, pad, rect_w, rect_h, standard_cfg);
    let sq_path = SquirclePath::new(pad + rect_w + gap, pad, rect_w, rect_h, squircle_cfg);

    let d_std = std_path.to_svg_path_data();
    let d_sq = sq_path.to_svg_path_data();

    let mut body = String::new();
    body.push_str(&format!(
        "<rect width=\"{total_w}\" height=\"{total_h}\" fill=\"#f8f8f8\"/>\n"
    ));
    body.push_str(&format!(
        "<path d=\"{d_std}\" fill=\"rgba(100,100,100,0.15)\" stroke=\"#666\" stroke-width=\"1.5\"/>\n"
    ));
    body.push_str(&format!(
        "<text x=\"{cx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"11\" fill=\"#333\" text-anchor=\"middle\">Standard (xi=0.0)</text>\n",
        cx = pad + rect_w / 2.0,
        ty = pad + rect_h + 18.0,
    ));
    body.push_str(&format!(
        "<path d=\"{d_sq}\" fill=\"rgba(60,120,220,0.18)\" stroke=\"#2060c0\" stroke-width=\"1.5\"/>\n"
    ));
    body.push_str(&format!(
        "<text x=\"{cx}\" y=\"{ty}\" font-family=\"monospace\" font-size=\"11\" fill=\"#333\" text-anchor=\"middle\">Squircle (xi=0.6)</text>\n",
        cx = pad + rect_w + gap + rect_w / 2.0,
        ty = pad + rect_h + 18.0,
    ));

    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total_w}\" height=\"{total_h}\">\n{body}</svg>\n"
    );

    let out_path = output_dir.join("standard_vs_squircle.svg");
    fs::write(&out_path, svg).expect("write SVG file");
    println!("Comparison visual written to: {}", out_path.display());
}
