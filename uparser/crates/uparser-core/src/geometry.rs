//! Coordinate-system conversion and geometric dedup, per
//! ARCHITECTURE.md §5 / T-1.3.

use crate::types::{CoordFrame, Geometry};

/// Convert a `[0,1]` fraction-of-page bbox to page-pixel coordinates.
/// mineru-vlm's stage-1 boxes are fractions of the (independently
/// stretched) 1036x1036 layout image, but since the stretch was
/// non-uniform per axis, `frac_x * width` / `frac_y * height` lands
/// correctly on the true page regardless of the layout image's own
/// dimensions (see the P1 plan's "Coordinate system" note).
pub fn denormalize_frac_bbox(bbox_frac: [f32; 4], width: u32, height: u32) -> [i32; 4] {
    [
        (bbox_frac[0] * width as f32).round() as i32,
        (bbox_frac[1] * height as f32).round() as i32,
        (bbox_frac[2] * width as f32).round() as i32,
        (bbox_frac[3] * height as f32).round() as i32,
    ]
}

/// Convert a MinerU custom_token box (ints in `[0,1000]`) straight to
/// page pixels in one step.
pub fn denormalize_0to1000_bbox(bbox_1000: [u32; 4], width: u32, height: u32) -> [i32; 4] {
    let frac = [
        bbox_1000[0] as f32 / 1000.0,
        bbox_1000[1] as f32 / 1000.0,
        bbox_1000[2] as f32 / 1000.0,
        bbox_1000[3] as f32 / 1000.0,
    ];
    denormalize_frac_bbox(frac, width, height)
}

/// Convert a `[0,1000]`-normalized bbox to page pixels, swapping any
/// inverted axis and clamping to image bounds with a minimum 1px width
/// and height — MonkeyOCRv2's more defensive variant of the mineru-vlm
/// `denormalize_0to1000_bbox` conversion (ported from `_map_bbox_to_image`).
pub fn map_bbox_0to1000_clamped(bbox_1000: [f32; 4], width: u32, height: u32) -> [i32; 4] {
    let (w, h) = (width as f32, height as f32);
    let mut x1 = bbox_1000[0] / 1000.0 * w;
    let mut y1 = bbox_1000[1] / 1000.0 * h;
    let mut x2 = bbox_1000[2] / 1000.0 * w;
    let mut y2 = bbox_1000[3] / 1000.0 * h;

    if x1 > x2 {
        std::mem::swap(&mut x1, &mut x2);
    }
    if y1 > y2 {
        std::mem::swap(&mut y1, &mut y2);
    }

    let max_x1 = if width > 0 { width as i32 - 1 } else { 0 };
    let max_y1 = if height > 0 { height as i32 - 1 } else { 0 };
    let cx1 = (x1.round() as i32).clamp(0, max_x1);
    let cy1 = (y1.round() as i32).clamp(0, max_y1);
    let cx2 = (x2.round() as i32).clamp(cx1 + 1, width as i32);
    let cy2 = (y2.round() as i32).clamp(cy1 + 1, height as i32);
    [cx1, cy1, cx2, cy2]
}

/// Convert a bbox expressed in a resized image's absolute pixel
/// coordinates back to the original (page) image's pixel coordinates,
/// by dividing out the resize's per-axis scale factor. Used by dots.ocr,
/// whose model output is plain absolute pixels of its own resized input
/// image, not a normalized fraction (see the P2 plan's "Coordinate
/// system" deviation note).
pub fn rescale_bbox_to_original(
    bbox_resized: [f32; 4],
    resized_wh: (u32, u32),
    original_wh: (u32, u32),
) -> [i32; 4] {
    let scale_x = resized_wh.0 as f32 / original_wh.0 as f32;
    let scale_y = resized_wh.1 as f32 / original_wh.1 as f32;
    [
        (bbox_resized[0] / scale_x).round() as i32,
        (bbox_resized[1] / scale_y).round() as i32,
        (bbox_resized[2] / scale_x).round() as i32,
        (bbox_resized[3] / scale_y).round() as i32,
    ]
}

/// Intersection-over-union of two axis-aligned pixel rects.
pub fn iou(a: [i32; 4], b: [i32; 4]) -> f32 {
    let ix0 = a[0].max(b[0]);
    let iy0 = a[1].max(b[1]);
    let ix1 = a[2].min(b[2]);
    let iy1 = a[3].min(b[3]);

    let inter_w = (ix1 - ix0).max(0);
    let inter_h = (iy1 - iy0).max(0);
    let inter = (inter_w as f32) * (inter_h as f32);

    let area_a = ((a[2] - a[0]).max(0) as f32) * ((a[3] - a[1]).max(0) as f32);
    let area_b = ((b[2] - b[0]).max(0) as f32) * ((b[3] - b[1]).max(0) as f32);
    let union = area_a + area_b - inter;

    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Drop boxes that overlap an earlier-kept box above `threshold` IoU,
/// keeping the first occurrence (stable order) of each cluster.
pub fn dedupe_by_iou(boxes: &[[i32; 4]], threshold: f32) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        let overlaps = kept.iter().any(|&k| iou(boxes[k], *b) > threshold);
        if !overlaps {
            kept.push(i);
        }
    }
    kept
}

/// Convert a pixel bbox from a crop's local frame into its parent's
/// frame. Not exercised by the mineru-vlm adapter in this pass (see the
/// P1 plan's note that v0.1.14's stage 2 doesn't emit child boxes), but
/// kept as shared machinery for protocols/versions that do.
pub fn crop_bbox_to_parent(bbox_in_crop: [i32; 4], crop_frame: &CoordFrame) -> [i32; 4] {
    match crop_frame {
        CoordFrame::Page => bbox_in_crop,
        CoordFrame::Crop { crop_bbox_px, .. } => [
            bbox_in_crop[0] + crop_bbox_px[0],
            bbox_in_crop[1] + crop_bbox_px[1],
            bbox_in_crop[2] + crop_bbox_px[0],
            bbox_in_crop[3] + crop_bbox_px[1],
        ],
    }
}

/// Extract the bounding rect of a `Geometry`, ignoring polygon detail.
pub fn geometry_bounds(geom: &Geometry) -> [f32; 4] {
    match geom {
        Geometry::Rect(r) => *r,
        Geometry::Polygon(points) => {
            let xs: Vec<f32> = points.iter().map(|p| p[0]).collect();
            let ys: Vec<f32> = points.iter().map(|p| p[1]).collect();
            [
                xs.iter().cloned().fold(f32::INFINITY, f32::min),
                ys.iter().cloned().fold(f32::INFINITY, f32::min),
                xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denormalize_frac_bbox_scales_by_dimensions() {
        let px = denormalize_frac_bbox([0.1, 0.2, 0.5, 0.6], 1000, 2000);
        assert_eq!(px, [100, 400, 500, 1200]);
    }

    #[test]
    fn denormalize_0to1000_matches_frac_equivalent() {
        let a = denormalize_0to1000_bbox([100, 200, 500, 600], 1000, 2000);
        let b = denormalize_frac_bbox([0.1, 0.2, 0.5, 0.6], 1000, 2000);
        assert_eq!(a, b);
    }

    #[test]
    fn rescale_bbox_to_original_divides_out_scale_factor() {
        // Resized image is half the original's dimensions; a bbox at
        // (50,100)-(150,200) in the resized image should land at
        // (100,200)-(300,400) in the original.
        let px = rescale_bbox_to_original([50.0, 100.0, 150.0, 200.0], (500, 800), (1000, 1600));
        assert_eq!(px, [100, 200, 300, 400]);
    }

    #[test]
    fn rescale_bbox_to_original_identity_when_dims_match() {
        let px = rescale_bbox_to_original([10.0, 20.0, 30.0, 40.0], (1000, 1000), (1000, 1000));
        assert_eq!(px, [10, 20, 30, 40]);
    }

    #[test]
    fn map_bbox_0to1000_clamped_normal_box() {
        let px = map_bbox_0to1000_clamped([100.0, 200.0, 500.0, 600.0], 1000, 2000);
        assert_eq!(px, [100, 400, 500, 1200]);
    }

    #[test]
    fn map_bbox_0to1000_clamped_swaps_inverted_axes() {
        // x1 > x2 in the 0-1000 space (500 > 100) must be swapped.
        let px = map_bbox_0to1000_clamped([500.0, 200.0, 100.0, 600.0], 1000, 2000);
        assert_eq!(px, [100, 400, 500, 1200]);
    }

    #[test]
    fn map_bbox_0to1000_clamped_clips_out_of_bounds() {
        let px = map_bbox_0to1000_clamped([-500.0, -500.0, 2000.0, 2000.0], 100, 100);
        assert_eq!(px, [0, 0, 100, 100]);
    }

    #[test]
    fn map_bbox_0to1000_clamped_degenerate_box_gets_minimum_1px() {
        let px = map_bbox_0to1000_clamped([500.0, 500.0, 500.0, 500.0], 100, 100);
        assert_eq!(px[2] - px[0], 1);
        assert_eq!(px[3] - px[1], 1);
    }

    #[test]
    fn iou_identical_boxes_is_one() {
        let a = [0, 0, 10, 10];
        assert!((iou(a, a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_disjoint_boxes_is_zero() {
        assert_eq!(iou([0, 0, 10, 10], [20, 20, 30, 30]), 0.0);
    }

    #[test]
    fn iou_partial_overlap() {
        // [0,0,10,10] and [5,0,15,10]: intersection 5x10=50, union 100+100-50=150
        let v = iou([0, 0, 10, 10], [5, 0, 15, 10]);
        assert!((v - (50.0 / 150.0)).abs() < 1e-6);
    }

    #[test]
    fn dedupe_by_iou_keeps_first_of_each_cluster() {
        let boxes = vec![
            [0, 0, 10, 10],
            [1, 1, 11, 11], // near-duplicate of box 0
            [50, 50, 60, 60],
        ];
        let kept = dedupe_by_iou(&boxes, 0.5);
        assert_eq!(kept, vec![0, 2]);
    }

    #[test]
    fn crop_bbox_to_parent_page_frame_is_identity() {
        let b = [1, 2, 3, 4];
        assert_eq!(crop_bbox_to_parent(b, &CoordFrame::Page), b);
    }

    #[test]
    fn crop_bbox_to_parent_offsets_by_crop_origin() {
        let frame = CoordFrame::Crop {
            parent_block: 0,
            crop_bbox_px: [100, 200, 300, 400],
        };
        let result = crop_bbox_to_parent([5, 5, 15, 15], &frame);
        assert_eq!(result, [105, 205, 115, 215]);
    }

    #[test]
    fn geometry_bounds_rect_passthrough() {
        let r = Geometry::Rect([1.0, 2.0, 3.0, 4.0]);
        assert_eq!(geometry_bounds(&r), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn geometry_bounds_polygon_computes_envelope() {
        let p = Geometry::Polygon(vec![[1.0, 5.0], [3.0, 1.0], [0.0, 2.0]]);
        assert_eq!(geometry_bounds(&p), [0.0, 1.0, 3.0, 5.0]);
    }
}
