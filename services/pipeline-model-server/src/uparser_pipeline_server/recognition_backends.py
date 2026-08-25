"""OCR and formula recognition backends aligned with MinerU 3.4.4."""

from __future__ import annotations

import math
import sys
from pathlib import Path
from threading import RLock
from typing import Callable

import numpy as np

from .legacy_backends import _decode_page
from .registry import ModelRegistry
from .schemas import (
    FormulaRecognitionInput,
    FormulaRecognitionResult,
    FormulaSpan,
    OcrPageInput,
    OcrResult,
    OcrSpan,
    Polygon,
    Region,
)


TEXT_REGION_LABELS = {
    "abstract",
    "algorithm",
    "aside_text",
    "content",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "number",
    "paragraph_title",
    "reference_content",
    "text",
    "vertical_text",
    "vision_footnote",
}


def mask_formula_regions(
    bgr_image: np.ndarray,
    formula_regions: list[Region],
) -> np.ndarray:
    """White out formula boxes before OCR detection, as current MinerU does."""
    if not formula_regions:
        return bgr_image
    masked = bgr_image.copy()
    height, width = masked.shape[:2]
    for region in formula_regions:
        x0, y0, x1, y1 = region.bbox
        left = max(0, min(width, math.floor(x0)))
        top = max(0, min(height, math.floor(y0)))
        right = max(0, min(width, math.ceil(x1)))
        bottom = max(0, min(height, math.ceil(y1)))
        if right > left and bottom > top:
            masked[top:bottom, left:right] = 255
    return masked


def _crop_with_padding(
    rgb_image: np.ndarray,
    region: Region,
    padding: int,
) -> tuple[np.ndarray, tuple[int, int]]:
    """Crop a layout region onto a white canvas with MinerU's 50px padding."""
    height, width = rgb_image.shape[:2]
    x0, y0, x1, y1 = region.bbox
    left = max(0, min(width, math.floor(x0)))
    top = max(0, min(height, math.floor(y0)))
    right = max(0, min(width, math.ceil(x1)))
    bottom = max(0, min(height, math.ceil(y1)))
    if right <= left or bottom <= top:
        raise ValueError(f"region has no pixels after clipping: {region.region_id}")
    canvas = np.full(
        (bottom - top + 2 * padding, right - left + 2 * padding, 3),
        255,
        dtype=np.uint8,
    )
    canvas[padding : padding + bottom - top, padding : padding + right - left] = (
        rgb_image[top:bottom, left:right]
    )
    return canvas, (left - padding, top - padding)


def _relative_formulas(
    formula_regions: list[Region],
    offset: tuple[int, int],
) -> list[Region]:
    offset_x, offset_y = offset
    relative = []
    for formula in formula_regions:
        x0, y0, x1, y1 = formula.bbox
        relative.append(
            formula.model_copy(
                update={"bbox": (x0 - offset_x, y0 - offset_y, x1 - offset_x, y1 - offset_y)}
            )
        )
    return relative


def _page_polygon(points, offset: tuple[int, int]) -> Polygon:
    offset_x, offset_y = offset
    normalized = np.asarray(points, dtype=float).reshape(-1, 2)
    return Polygon(
        points=[(float(x + offset_x), float(y + offset_y)) for x, y in normalized]
    )


class LegacyOcrBackend:
    """Use existing PaddleOCR weights with current MinerU crop/mask semantics."""

    def __init__(
        self,
        models_root: Path,
        device: str,
        loader: Callable[[str, Path, str], object] | None = None,
        padding: int = 50,
        min_confidence: float = 0.5,
    ):
        self.models_root = models_root
        self.device = device
        self.loader = loader or self._default_loader
        self.padding = padding
        self.min_confidence = min_confidence
        self.models = ModelRegistry()
        self.inference_lock = RLock()

    @staticmethod
    def _default_loader(language: str, models_root: Path, device: str):
        import magic_pdf.model.sub_modules.ocr.paddleocr2pytorch.pytorch_paddle as paddle

        # Legacy construction reads these helpers from module globals. Supplying
        # explicit values avoids hidden dependence on ~/magic-pdf.json.
        paddle.get_local_models_dir = lambda: str(models_root)
        paddle.get_device = lambda: device
        return paddle.PytorchPaddleOCR(lang=language)

    def __call__(self, item: OcrPageInput) -> OcrResult:
        model = self.models.get_or_load(
            (item.language, str(self.models_root), self.device),
            lambda: self.loader(item.language, self.models_root, self.device),
        )
        with _decode_page(item.page) as image:
            rgb_image = np.asarray(image, dtype=np.uint8)

        spans = []
        candidates = [region for region in item.layout_regions if region.label in TEXT_REGION_LABELS]
        for region in candidates:
            crop_rgb, offset = _crop_with_padding(rgb_image, region, self.padding)
            formulas = _relative_formulas(item.formula_regions, offset)
            crop_bgr = crop_rgb[:, :, ::-1].copy()
            detection_image = mask_formula_regions(crop_bgr, formulas)
            legacy_masks = [{"bbox": list(formula.bbox)} for formula in formulas]
            with self.inference_lock:
                raw = model.ocr(detection_image, det=True, rec=True, mfd_res=legacy_masks)
            results = raw[0] if raw else None
            for points, recognition in results or []:
                text, confidence = recognition
                confidence = float(confidence)
                if confidence < self.min_confidence or not str(text).strip():
                    continue
                spans.append(
                    OcrSpan(
                        span_id=f"{item.page.page_id}/ocr-{len(spans)}",
                        parent_region_id=region.region_id,
                        polygon=_page_polygon(points, offset),
                        text=str(text),
                        language=item.language,
                        confidence=min(1.0, max(0.0, confidence)),
                        coordinate_space="render_pixels",
                    )
                )
        return OcrResult(spans=spans)


class CurrentMineruMfrBackend:
    """Run the current MinerU UniMERNet adapter against the supplied weight set."""

    def __init__(
        self,
        weight_dir: Path,
        reference_root: Path,
        device: str,
        loader: Callable[[Path, Path, str], object] | None = None,
        batch_size: int = 64,
    ):
        self.weight_dir = weight_dir
        self.reference_root = reference_root
        self.device = device
        self.loader = loader or self._default_loader
        self.batch_size = batch_size
        self.models = ModelRegistry()
        self.inference_lock = RLock()

    @staticmethod
    def _default_loader(weight_dir: Path, reference_root: Path, device: str):
        root = str(reference_root)
        if root not in sys.path:
            sys.path.insert(0, root)
        from mineru.model.mfr.unimernet.Unimernet import UnimernetModel

        return UnimernetModel(str(weight_dir), _device_=device)

    def __call__(self, item: FormulaRecognitionInput) -> FormulaRecognitionResult:
        if not item.formula_regions:
            return FormulaRecognitionResult(spans=[])
        model = self.models.get_or_load(
            (str(self.weight_dir), self.device),
            lambda: self.loader(self.weight_dir, self.reference_root, self.device),
        )
        with _decode_page(item.page) as image:
            rgb_image = np.asarray(image, dtype=np.uint8)
        inputs = [
            {
                "region_id": region.region_id,
                "label": region.label,
                "bbox": list(region.bbox),
                "confidence": region.confidence,
            }
            for region in item.formula_regions
        ]
        with self.inference_lock:
            results = model.predict(inputs, rgb_image, batch_size=self.batch_size)
        by_id = {result.get("region_id"): result for result in results}
        spans = []
        for region in item.formula_regions:
            result = by_id.get(region.region_id)
            if result is None:
                continue
            spans.append(
                FormulaSpan(
                    region_id=region.region_id,
                    latex=str(result.get("latex", "")),
                    confidence=region.confidence,
                    bbox=region.bbox,
                )
            )
        return FormulaRecognitionResult(spans=spans)
