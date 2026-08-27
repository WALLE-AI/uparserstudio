"""Wireless table recognition using MinerU's current SLANet-plus adapter."""

from __future__ import annotations

import html
import math
import sys
from html.parser import HTMLParser
from pathlib import Path
from threading import RLock
from typing import Callable

import numpy as np

from .legacy_backends import _decode_page
from .registry import ModelRegistry
from .schemas import (
    FormulaSpan,
    OcrSpan,
    RecognizedTable,
    TableCell,
    TableRecognitionInput,
    TableRecognitionResult,
)


class _StructureTokenParser(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.tokens: list[str] = []

    def handle_starttag(self, tag, attrs):
        if tag in {"table", "thead", "tbody", "tr", "td", "th"}:
            attributes = "".join(f' {key}="{value}"' for key, value in attrs)
            self.tokens.append(f"<{tag}{attributes}>")

    def handle_endtag(self, tag):
        if tag in {"table", "thead", "tbody", "tr", "td", "th"}:
            self.tokens.append(f"</{tag}>")


def _structure_tokens(markup: str) -> list[str]:
    parser = _StructureTokenParser()
    parser.feed(markup)
    return parser.tokens


def _span_center(span: OcrSpan) -> tuple[float, float]:
    xs = [point[0] for point in span.polygon.points]
    ys = [point[1] for point in span.polygon.points]
    return sum(xs) / len(xs), sum(ys) / len(ys)


def _table_ocr(
    spans: list[OcrSpan],
    formula_spans: list[FormulaSpan],
    bbox: tuple[float, float, float, float],
) -> tuple[list[list], list[OcrSpan]]:
    x0, y0, x1, y1 = bbox
    selected = []
    result = []
    for span in spans:
        center_x, center_y = _span_center(span)
        if not (x0 <= center_x <= x1 and y0 <= center_y <= y1):
            continue
        points = [[x - x0, y - y0] for x, y in span.polygon.points]
        result.append([points, html.escape(span.text), span.confidence or 1.0])
        selected.append(span)
    for formula in formula_spans:
        if formula.bbox is None or not formula.latex.strip():
            continue
        fx0, fy0, fx1, fy1 = formula.bbox
        center_x = (fx0 + fx1) / 2
        center_y = (fy0 + fy1) / 2
        if not (x0 <= center_x <= x1 and y0 <= center_y <= y1):
            continue
        points = [
            [fx0 - x0, fy0 - y0],
            [fx1 - x0, fy0 - y0],
            [fx1 - x0, fy1 - y0],
            [fx0 - x0, fy1 - y0],
        ]
        result.append([points, html.escape(formula.latex), formula.confidence or 1.0])
    return result, selected


def _cell_text(
    cell_bbox: tuple[float, float, float, float],
    spans: list[OcrSpan],
) -> str:
    x0, y0, x1, y1 = cell_bbox
    matches = []
    for span in spans:
        center_x, center_y = _span_center(span)
        if x0 <= center_x <= x1 and y0 <= center_y <= y1:
            matches.append((center_y, center_x, span.text))
    return " ".join(text for _, _, text in sorted(matches))


def table_cell_from_geometry(
    region_id: str,
    index: int,
    box,
    logic,
    left: int,
    top: int,
    selected_spans: list[OcrSpan],
) -> TableCell:
    """Build a `TableCell` from one wired/wireless table model's raw
    `(cell_bboxes[i], logic_points[i])` pair, clamping row/column/span to
    the schema's actual constraints (`row`/`column` >= 0,
    `row_span`/`column_span` > 0 — see `schemas.py::TableCell`).

    A real run against real MinerU 3.4.5 weights hit a genuine, previously
    unguarded case: a degenerate `logic_points` entry (`row_end <
    row_start` or `column_end < column_start`) produced a zero or
    negative span, which `TableCell`'s pydantic validation then rejected
    outright — turning one malformed cell into a hard `PageError` for the
    entire page instead of a merely-imperfect table. Clamping here keeps
    the cell (with the best geometry/text it can salvage) instead of
    failing the page over one model output quirk; this mirrors this
    project's established `sanitize_bbox_px`/`geometry.rs` convention of
    clamping a model's geometry to the nearest valid value rather than
    erroring on it.
    """
    values = np.asarray(box, dtype=float).reshape(-1)
    local_xs = values[0::2]
    local_ys = values[1::2]
    cell_bbox = (
        float(local_xs.min() + left),
        float(local_ys.min() + top),
        float(local_xs.max() + left),
        float(local_ys.max() + top),
    )
    row_start, row_end, column_start, column_end = [int(value) for value in logic]
    return TableCell(
        cell_id=f"{region_id}/cell-{index}",
        row=max(0, row_start),
        column=max(0, column_start),
        row_span=max(1, row_end - row_start + 1),
        column_span=max(1, column_end - column_start + 1),
        bbox=cell_bbox,
        text=_cell_text(cell_bbox, selected_spans),
    )


class CurrentMineruSlanetBackend:
    def __init__(
        self,
        weight: Path,
        reference_root: Path,
        loader: Callable[[Path, Path], object] | None = None,
    ):
        self.weight = weight
        self.reference_root = reference_root
        self.loader = loader or self._default_loader
        self.models = ModelRegistry()
        self.inference_lock = RLock()

    @staticmethod
    def _default_loader(weight: Path, reference_root: Path):
        root = str(reference_root)
        if root not in sys.path:
            sys.path.insert(0, root)
        from mineru.model.table.rec.slanet_plus.main import PaddleTable, PaddleTableInput

        return PaddleTable(
            PaddleTableInput(model_type="slanet_plus", model_path=str(weight))
        )

    def __call__(self, item: TableRecognitionInput) -> TableRecognitionResult:
        if not item.table_regions:
            return TableRecognitionResult(tables=[])
        model = self.models.get_or_load(
            (str(self.weight),), lambda: self.loader(self.weight, self.reference_root)
        )
        with _decode_page(item.page) as image:
            page_rgb = np.asarray(image, dtype=np.uint8)

        tables = []
        page_height, page_width = page_rgb.shape[:2]
        for region in item.table_regions:
            x0, y0, x1, y1 = region.bbox
            left = max(0, min(page_width, math.floor(x0)))
            top = max(0, min(page_height, math.floor(y0)))
            right = max(0, min(page_width, math.ceil(x1)))
            bottom = max(0, min(page_height, math.ceil(y1)))
            if right <= left or bottom <= top:
                raise ValueError(f"table has no pixels after clipping: {region.region_id}")
            crop = page_rgb[top:bottom, left:right]
            ocr_result, selected_spans = _table_ocr(
                item.ocr_spans,
                item.formula_spans,
                (left, top, right, bottom),
            )
            with self.inference_lock:
                output = model.predict(crop, ocr_result)
            markup = output.pred_html or ""
            cell_boxes = output.cell_bboxes if output.cell_bboxes is not None else []
            logic_points = output.logic_points if output.logic_points is not None else []
            cells = [
                table_cell_from_geometry(
                    region.region_id, index, box, logic, left, top, selected_spans
                )
                for index, (box, logic) in enumerate(zip(cell_boxes, logic_points))
            ]
            tables.append(
                RecognizedTable(
                    region_id=region.region_id,
                    html=markup,
                    structure_tokens=_structure_tokens(markup),
                    cells=cells,
                    classifier_label="wireless",
                    rotation_degrees=0,
                    confidence=region.confidence,
                )
            )
        return TableRecognitionResult(tables=tables)
