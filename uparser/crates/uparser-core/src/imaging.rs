//! Image preprocessing primitives shared by adapters, per
//! ARCHITECTURE.md §9.2 / T-1.2. Parameter defaults here mirror the
//! confirmed mineru-vlm v0.1.14 wire protocol (see the P1 plan): stage 1
//! hard-resizes the whole page (no aspect preservation), stage 2 crops
//! get an aspect-ratio-preserving pad for extreme ratios plus a
//! short-edge upscale floor.

use crate::transport::image_data_url;
use image::imageops::FilterType;
use image::{Rgb, RgbImage};

/// Convert to a plain RGB buffer (drops alpha).
pub fn to_rgb(img: &image::DynamicImage) -> RgbImage {
    img.to_rgb8()
}

/// Hard resize to exactly `(width, height)`, independent x/y scale
/// factors — no aspect-ratio preservation, no padding. Used for
/// mineru-vlm's stage-1 layout image (default 1036x1036).
pub fn hard_resize(img: &RgbImage, width: u32, height: u32) -> RgbImage {
    image::imageops::resize(img, width, height, FilterType::CatmullRom)
}

/// Crop `img` to a pixel bbox `[x0, y0, x1, y1]`, clamped to image bounds.
pub fn crop(img: &RgbImage, bbox_px: [i32; 4]) -> RgbImage {
    let (w, h) = img.dimensions();
    let x0 = bbox_px[0].clamp(0, w as i32) as u32;
    let y0 = bbox_px[1].clamp(0, h as i32) as u32;
    let x1 = bbox_px[2].clamp(x0 as i32, w as i32) as u32;
    let y1 = bbox_px[3].clamp(y0 as i32, h as i32) as u32;
    image::imageops::crop_imm(img, x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)).to_image()
}

/// Rotate by a multiple of 90 degrees (0/90/180/270), matching
/// mineru-vlm's rotation token semantics.
pub fn rotate_90n(img: &RgbImage, angle: u32) -> RgbImage {
    match angle % 360 {
        90 => image::imageops::rotate90(img),
        180 => image::imageops::rotate180(img),
        270 => image::imageops::rotate270(img),
        _ => img.clone(),
    }
}

/// Aspect-ratio-preserving adjustment for a stage-2 content crop:
/// - if `max(w,h)/min(w,h) > max_edge_ratio`, white-pad the shorter
///   dimension so the crop sits centered on a squarer canvas;
/// - if `min(w,h) < min_edge`, upscale (preserving aspect ratio) so the
///   short edge reaches `min_edge`.
pub fn resize_by_need(img: &RgbImage, max_edge_ratio: f32, min_edge: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return img.clone();
    }

    let long = w.max(h) as f32;
    let short = w.min(h) as f32;
    let padded = if long / short > max_edge_ratio {
        pad_to_square_ratio(img, max_edge_ratio)
    } else {
        img.clone()
    };

    let (pw, ph) = padded.dimensions();
    let short_edge = pw.min(ph);
    if short_edge < min_edge && short_edge > 0 {
        let scale = min_edge as f32 / short_edge as f32;
        let new_w = (pw as f32 * scale).round().max(1.0) as u32;
        let new_h = (ph as f32 * scale).round().max(1.0) as u32;
        image::imageops::resize(&padded, new_w, new_h, FilterType::CatmullRom)
    } else {
        padded
    }
}

/// Center the image on a white canvas whose long edge is unchanged and
/// whose short edge is stretched to `long / max_edge_ratio`, capping the
/// aspect ratio without distorting the original content.
fn pad_to_square_ratio(img: &RgbImage, max_edge_ratio: f32) -> RgbImage {
    let (w, h) = img.dimensions();
    let (canvas_w, canvas_h) = if w >= h {
        (w, ((w as f32) / max_edge_ratio).ceil().max(h as f32) as u32)
    } else {
        (((h as f32) / max_edge_ratio).ceil().max(w as f32) as u32, h)
    };

    let mut canvas = RgbImage::from_pixel(canvas_w, canvas_h, Rgb([255, 255, 255]));
    let offset_x = (canvas_w.saturating_sub(w)) / 2;
    let offset_y = (canvas_h.saturating_sub(h)) / 2;
    image::imageops::overlay(&mut canvas, img, offset_x as i64, offset_y as i64);
    canvas
}

pub fn to_png_bytes(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn to_base64_data_url(img: &RgbImage) -> Result<String, String> {
    Ok(image_data_url(&to_png_bytes(img)?))
}

/// Plain aspect-ratio-preserving, pixel-count-bounded resize (MonkeyOCRv2):
/// scale down by `sqrt(max_pixels/area)` if over `max_pixels`, scale up by
/// `sqrt(min_pixels/area)` if under `min_pixels`, otherwise unchanged.
/// Passing `min_pixels == max_pixels` targets a fixed total pixel count
/// while preserving aspect ratio exactly. Unlike dots.ocr's `smart_resize`,
/// there's no factor-grid snapping — just a plain LANCZOS resize.
pub fn resize_by_pixel_bounds(img: &RgbImage, min_pixels: u32, max_pixels: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let area = (w as f64) * (h as f64);
    if area <= 0.0 {
        return img.clone();
    }

    let scale = if area > max_pixels as f64 {
        (max_pixels as f64 / area).sqrt()
    } else if area < min_pixels as f64 {
        (min_pixels as f64 / area).sqrt()
    } else {
        return img.clone();
    };

    let new_w = ((w as f64) * scale).round().max(1.0) as u32;
    let new_h = ((h as f64) * scale).round().max(1.0) as u32;
    image::imageops::resize(img, new_w, new_h, FilterType::Lanczos3)
}

fn round_by_factor(number: f64, factor: u32) -> u32 {
    ((number / factor as f64).round() * factor as f64) as u32
}

fn ceil_by_factor(number: f64, factor: u32) -> u32 {
    ((number / factor as f64).ceil() * factor as f64) as u32
}

fn floor_by_factor(number: f64, factor: u32) -> u32 {
    ((number / factor as f64).floor() * factor as f64) as u32
}

/// Qwen2.5-VL-style resize (used by dots.ocr): both dimensions land on
/// the `factor` grid, the total pixel count lands in `[min_pixels,
/// max_pixels]`, and the aspect ratio is preserved as closely as
/// possible. Rejects aspect ratios greater than 200, matching the source
/// algorithm's own guard.
pub fn smart_resize(
    height: u32,
    width: u32,
    factor: u32,
    min_pixels: u32,
    max_pixels: u32,
) -> Result<(u32, u32), String> {
    let (h, w) = (height as f64, width as f64);
    if h.max(w) / h.min(w) > 200.0 {
        return Err(format!(
            "absolute aspect ratio must be smaller than 200, got {}",
            h.max(w) / h.min(w)
        ));
    }

    let mut h_bar = round_by_factor(h, factor).max(factor);
    let mut w_bar = round_by_factor(w, factor).max(factor);

    if (h_bar as u64) * (w_bar as u64) > max_pixels as u64 {
        let beta = ((h * w) / max_pixels as f64).sqrt();
        h_bar = floor_by_factor(h / beta, factor).max(factor);
        w_bar = floor_by_factor(w / beta, factor).max(factor);
    } else if (h_bar as u64) * (w_bar as u64) < min_pixels as u64 {
        let beta = (min_pixels as f64 / (h * w)).sqrt();
        h_bar = ceil_by_factor(h * beta, factor);
        w_bar = ceil_by_factor(w * beta, factor);
        if (h_bar as u64) * (w_bar as u64) > max_pixels as u64 {
            let beta = ((h_bar as f64) * (w_bar as f64) / max_pixels as f64).sqrt();
            h_bar = floor_by_factor(h_bar as f64 / beta, factor).max(factor);
            w_bar = floor_by_factor(w_bar as f64 / beta, factor).max(factor);
        }
    }

    Ok((h_bar, w_bar))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb([10, 20, 30]))
    }

    #[test]
    fn hard_resize_ignores_aspect_ratio() {
        let img = solid(100, 50);
        let resized = hard_resize(&img, 1036, 1036);
        assert_eq!(resized.dimensions(), (1036, 1036));
    }

    #[test]
    fn crop_clamps_to_bounds() {
        let img = solid(100, 100);
        let cropped = crop(&img, [-10, -10, 200, 200]);
        assert_eq!(cropped.dimensions(), (100, 100));
    }

    #[test]
    fn crop_extracts_requested_region() {
        let img = solid(100, 100);
        let cropped = crop(&img, [10, 20, 60, 70]);
        assert_eq!(cropped.dimensions(), (50, 50));
    }

    #[test]
    fn rotate_90n_swaps_dimensions_for_90_and_270() {
        let img = solid(30, 10);
        assert_eq!(rotate_90n(&img, 90).dimensions(), (10, 30));
        assert_eq!(rotate_90n(&img, 270).dimensions(), (10, 30));
        assert_eq!(rotate_90n(&img, 180).dimensions(), (30, 10));
        assert_eq!(rotate_90n(&img, 0).dimensions(), (30, 10));
    }

    #[test]
    fn resize_by_need_pads_extreme_aspect_ratio() {
        // 1000x10 has ratio 100 > default max_edge_ratio 50 -> should be
        // padded so the ratio is capped.
        let img = solid(1000, 10);
        let out = resize_by_need(&img, 50.0, 28);
        let (w, h) = out.dimensions();
        assert!(w.max(h) as f32 / w.min(h) as f32 <= 50.01);
    }

    #[test]
    fn resize_by_need_upscales_tiny_short_edge() {
        let img = solid(20, 15);
        let out = resize_by_need(&img, 50.0, 28);
        let (w, h) = out.dimensions();
        assert!(w.min(h) >= 28);
    }

    #[test]
    fn resize_by_need_leaves_normal_crops_untouched() {
        let img = solid(200, 150);
        let out = resize_by_need(&img, 50.0, 28);
        assert_eq!(out.dimensions(), (200, 150));
    }

    #[test]
    fn png_and_data_url_round_trip() {
        let img = solid(4, 4);
        let bytes = to_png_bytes(&img).unwrap();
        assert!(!bytes.is_empty());
        let url = to_base64_data_url(&img).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn smart_resize_typical_page_matches_hand_computation() {
        // height=850, width=1100, factor=28: round(850/28)=30 -> 840;
        // round(1100/28)=39 -> 1092. Product 917280 is within
        // [3136, 11289600], so neither pixel-count branch triggers.
        let (h, w) = smart_resize(850, 1100, 28, 3136, 11289600).unwrap();
        assert_eq!((h, w), (840, 1092));
    }

    #[test]
    fn smart_resize_always_lands_on_factor_grid() {
        let (h, w) = smart_resize(4000, 3000, 28, 3136, 11289600).unwrap();
        assert_eq!(h % 28, 0);
        assert_eq!(w % 28, 0);
    }

    #[test]
    fn smart_resize_shrinks_to_respect_max_pixels() {
        let (h, w) = smart_resize(4000, 3000, 28, 3136, 11289600).unwrap();
        assert!((h as u64) * (w as u64) <= 11289600);
    }

    #[test]
    fn smart_resize_grows_to_respect_min_pixels() {
        let (h, w) = smart_resize(10, 15, 28, 3136, 11289600).unwrap();
        assert!((h as u64) * (w as u64) >= 3136);
        assert!((h as u64) * (w as u64) <= 11289600);
    }

    #[test]
    fn smart_resize_rejects_extreme_aspect_ratio() {
        let result = smart_resize(10, 3000, 28, 3136, 11289600);
        assert!(result.is_err());
    }

    #[test]
    fn resize_by_pixel_bounds_shrinks_preserving_aspect_ratio() {
        let img = solid(2000, 1000); // area 2,000,000
        let out = resize_by_pixel_bounds(&img, 1_003_520, 1_003_520);
        let (w, h) = out.dimensions();
        // Aspect ratio preserved: w/h should stay ~2.0.
        assert!((w as f64 / h as f64 - 2.0).abs() < 0.05);
        assert!((w as u64) * (h as u64) <= 1_100_000);
    }

    #[test]
    fn resize_by_pixel_bounds_grows_preserving_aspect_ratio() {
        let img = solid(100, 50); // area 5,000, under min_pixels
        let out = resize_by_pixel_bounds(&img, 1_003_520, 1_003_520);
        let (w, h) = out.dimensions();
        assert!((w as f64 / h as f64 - 2.0).abs() < 0.05);
        assert!((w as u64) * (h as u64) >= 900_000);
    }

    #[test]
    fn resize_by_pixel_bounds_noop_within_range() {
        let img = solid(1000, 1000);
        let out = resize_by_pixel_bounds(&img, 100, 2_000_000);
        assert_eq!(out.dimensions(), (1000, 1000));
    }

    #[test]
    fn resize_by_pixel_bounds_min_equals_max_targets_fixed_pixel_count() {
        let img = solid(3000, 1500); // area 4,500,000
        let out = resize_by_pixel_bounds(&img, 1_003_520, 1_003_520);
        let (w, h) = out.dimensions();
        let area = (w as f64) * (h as f64);
        assert!((area - 1_003_520.0).abs() / 1_003_520.0 < 0.01);
    }
}
