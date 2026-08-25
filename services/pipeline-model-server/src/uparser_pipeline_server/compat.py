"""Normalize legacy PDF-Extract-Kit detections to current MinerU labels."""

from __future__ import annotations

from dataclasses import dataclass
from math import isfinite
from typing import Iterable

from .schemas import CoordinateSpace, Polygon, Region


LEGACY_LAYOUT_LABELS = {
    0: "paragraph_title",
    1: "text",
    2: "discarded",
    3: "image",
    4: "figure_title",
    5: "table",
    6: "figure_title",
    7: "vision_footnote",
    8: "display_formula",
    9: "formula_number",
}

LEGACY_MFD_LABELS = {
    0: "inline_formula",
    1: "display_formula",
}


@dataclass(frozen=True)
class LegacyDetection:
    class_id: int
    bbox: tuple[float, float, float, float]
    confidence: float | None = None


def poly_to_bbox(poly: Iterable[float]) -> tuple[float, float, float, float]:
    values = [float(value) for value in poly]
    if len(values) != 8 or not all(isfinite(value) for value in values):
        raise ValueError("legacy polygon must contain eight finite coordinates")
    xs = values[0::2]
    ys = values[1::2]
    bbox = (min(xs), min(ys), max(xs), max(ys))
    if bbox[2] <= bbox[0] or bbox[3] <= bbox[1]:
        raise ValueError("legacy polygon has no area")
    return bbox


def _polygon(bbox: tuple[float, float, float, float]) -> Polygon:
    x0, y0, x1, y1 = bbox
    return Polygon(points=[(x0, y0), (x1, y0), (x1, y1), (x0, y1)])


def _region(
    page_id: str,
    source: str,
    index: int,
    label: str,
    detection: LegacyDetection,
) -> Region:
    confidence = detection.confidence
    if confidence is not None:
        confidence = min(1.0, max(0.0, float(confidence)))
    return Region(
        region_id=f"{page_id}/{source}-{index}",
        label=label,
        bbox=detection.bbox,
        polygon=_polygon(detection.bbox),
        confidence=confidence,
        coordinate_space="render_pixels",
    )


def normalize_layout(page_id: str, detections: Iterable[LegacyDetection]) -> list[Region]:
    regions = []
    for index, detection in enumerate(detections):
        label = LEGACY_LAYOUT_LABELS.get(detection.class_id)
        if label is None:
            raise ValueError(f"unsupported legacy layout class id: {detection.class_id}")
        regions.append(_region(page_id, "layout", index, label, detection))
    return regions


def normalize_mfd(page_id: str, detections: Iterable[LegacyDetection]) -> list[Region]:
    regions = []
    for index, detection in enumerate(detections):
        label = LEGACY_MFD_LABELS.get(detection.class_id)
        if label is None:
            raise ValueError(f"unsupported legacy MFD class id: {detection.class_id}")
        regions.append(_region(page_id, "mfd", index, label, detection))
    return regions


def intersection_over_min_area(left: Region, right: Region) -> float:
    lx0, ly0, lx1, ly1 = left.bbox
    rx0, ry0, rx1, ry1 = right.bbox
    width = max(0.0, min(lx1, rx1) - max(lx0, rx0))
    height = max(0.0, min(ly1, ry1) - max(ly0, ry0))
    intersection = width * height
    left_area = (lx1 - lx0) * (ly1 - ly0)
    right_area = (rx1 - rx0) * (ry1 - ry0)
    denominator = min(left_area, right_area)
    return intersection / denominator if denominator > 0 else 0.0


def merge_layout_and_mfd(
    layout_regions: Iterable[Region],
    formula_regions: Iterable[Region],
    overlap_threshold: float = 0.7,
) -> list[Region]:
    formulas = list(formula_regions)
    merged = []
    for region in layout_regions:
        if region.label in {"inline_formula", "display_formula"} and any(
            intersection_over_min_area(region, formula) >= overlap_threshold
            for formula in formulas
        ):
            continue
        merged.append(region)
    merged.extend(formulas)
    return merged
