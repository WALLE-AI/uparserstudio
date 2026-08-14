//! Emit MinerU preprocessing intermediates using uparser's production code.
//!
//! Usage:
//!   mineru_preprocess_probe <image> <output-dir> <x0> <y0> <x1> <y1> <angle>

use serde_json::json;
use std::env;
use std::path::Path;
use uparser_core::{geometry, imaging};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 8 {
        eprintln!(
            "usage: mineru_preprocess_probe <image> <output-dir> <x0> <y0> <x1> <y1> <angle>"
        );
        std::process::exit(2);
    }

    let input = &args[1];
    let output_dir = Path::new(&args[2]);
    let bbox_1000 = [
        args[3].parse::<u32>().expect("x0"),
        args[4].parse::<u32>().expect("y0"),
        args[5].parse::<u32>().expect("x1"),
        args[6].parse::<u32>().expect("y1"),
    ];
    let angle = args[7].parse::<u32>().expect("angle");

    std::fs::create_dir_all(output_dir).expect("create output directory");
    let image = image::open(input).expect("open input image").to_rgb8();
    let (width, height) = image.dimensions();

    let layout = imaging::hard_resize(&image, 1036, 1036);
    layout
        .save(output_dir.join("rust-layout.png"))
        .expect("save layout image");

    let bbox_px = geometry::denormalize_0to1000_bbox(bbox_1000, width, height);
    let crop = imaging::crop(&image, bbox_px).expect("valid crop");
    let rotated = imaging::rotate_90n(&crop, angle);
    let extract = imaging::resize_by_need(&rotated, 50.0, 28);
    extract
        .save(output_dir.join("rust-extract.png"))
        .expect("save extract image");

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "source_size": [width, height],
            "bbox_1000": bbox_1000,
            "bbox_px": bbox_px,
            "angle": angle,
            "layout_size": [layout.width(), layout.height()],
            "crop_size": [crop.width(), crop.height()],
            "extract_size": [extract.width(), extract.height()],
        }))
        .expect("serialize probe result")
    );
}
