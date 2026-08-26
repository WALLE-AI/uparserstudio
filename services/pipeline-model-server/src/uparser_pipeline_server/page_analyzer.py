"""Single-page orchestration for the pipeline V2 stage graph."""

from __future__ import annotations

from pathlib import Path
from threading import RLock
from typing import Callable

from .compat import merge_layout_and_mfd
from .registry import BackendRegistry, ModelRegistry
from .schemas import (
    FormulaRecognitionInput,
    FormulaRecognitionResult,
    OcrPageInput,
    OcrResult,
    PageAnalyzeInput,
    PageAnalyzeResult,
    Region,
    TableRecognitionInput,
    TableRecognitionResult,
)


class LayoutReader:
    def __init__(
        self,
        weight_dir: Path,
        device: str,
        loader: Callable[[Path, str], object] | None = None,
        predictor: Callable[[list[list[int]], object], list[int]] | None = None,
        max_regions: int = 200,
    ):
        self.weight_dir = weight_dir
        self.device = device
        self.loader = loader or self._default_loader
        self.predictor = predictor or self._default_predictor
        self.max_regions = max_regions
        self.models = ModelRegistry()
        self.inference_lock = RLock()

    @staticmethod
    def _default_loader(weight_dir: Path, device: str):
        from transformers import LayoutLMv3ForTokenClassification

        return LayoutLMv3ForTokenClassification.from_pretrained(str(weight_dir)).to(device).eval()

    @staticmethod
    def _default_predictor(boxes: list[list[int]], model) -> list[int]:
        from magic_pdf.model.sub_modules.reading_oreder.layoutreader.helpers import (
            boxes2inputs,
            parse_logits,
            prepare_inputs,
        )

        import torch

        inputs = prepare_inputs(boxes2inputs(boxes), model)
        with torch.inference_mode():
            logits = model(**inputs).logits.cpu().squeeze(0)
        return parse_logits(logits, len(boxes))

    @staticmethod
    def _spatial_order(regions: list[Region]) -> list[str]:
        return [
            region.region_id
            for region in sorted(regions, key=lambda item: (item.bbox[1], item.bbox[0]))
        ]

    def __call__(self, regions: list[Region], page_width: int, page_height: int) -> list[str]:
        if not regions:
            return []
        if len(regions) > self.max_regions:
            return self._spatial_order(regions)
        boxes = []
        for region in regions:
            x0, y0, x1, y1 = region.bbox
            boxes.append(
                [
                    round(max(0, min(page_width, x0)) * 1000 / page_width),
                    round(max(0, min(page_height, y0)) * 1000 / page_height),
                    round(max(0, min(page_width, x1)) * 1000 / page_width),
                    round(max(0, min(page_height, y1)) * 1000 / page_height),
                ]
            )
        model = self.models.get_or_load(
            (str(self.weight_dir), self.device),
            lambda: self.loader(self.weight_dir, self.device),
        )
        with self.inference_lock:
            order = self.predictor(boxes, model)
        if sorted(order) != list(range(len(regions))):
            raise ValueError("layoutreader returned an invalid permutation")
        return [regions[index].region_id for index in order]


class PipelinePageAnalyzer:
    def __init__(self, registry: BackendRegistry, reading_order: LayoutReader):
        self.registry = registry
        self.reading_order = reading_order

    def _infer(self, stage: str, item):
        backend = self.registry.get(stage)
        if backend is None:
            raise RuntimeError(f"required backend not registered: {stage}")
        return backend.infer(item)

    def __call__(self, item: PageAnalyzeInput) -> PageAnalyzeResult:
        layout = self._infer("layout", item.page)
        if item.formula_enabled:
            formulas = self._infer("formula_detect", item.page)
            regions = merge_layout_and_mfd(layout.regions, formulas.regions)
        else:
            formulas = None
            regions = [
                region
                for region in layout.regions
                if region.label not in {"inline_formula", "display_formula"}
            ]
        formula_regions = formulas.regions if formulas else []
        ocr = self._infer(
            "ocr",
            OcrPageInput(
                page=item.page,
                layout_regions=regions,
                formula_regions=formula_regions,
                language=item.language,
            ),
        )
        formula_result = (
            self._infer(
                "formula_recognize",
                FormulaRecognitionInput(page=item.page, formula_regions=formula_regions),
            )
            if item.formula_enabled
            else FormulaRecognitionResult(spans=[])
        )
        table_regions = [region for region in regions if region.label == "table"]
        table_ocr = (
            self._infer(
                "ocr",
                OcrPageInput(
                    page=item.page,
                    layout_regions=[
                        region.model_copy(update={"label": "text"})
                        for region in table_regions
                    ],
                    formula_regions=formula_regions,
                    language=item.language,
                ),
            )
            if item.table_enabled and table_regions
            else OcrResult(spans=[])
        )
        table_result = (
            self._infer(
                "table",
                TableRecognitionInput(
                    page=item.page,
                    table_regions=table_regions,
                    ocr_spans=table_ocr.spans,
                    formula_spans=formula_result.spans,
                ),
            )
            if item.table_enabled and table_regions
            else TableRecognitionResult(tables=[])
        )
        order_regions = [region for region in regions if region.label != "discarded"]
        reading_order = self.reading_order(
            order_regions, item.page.dimensions.width, item.page.dimensions.height
        )
        return PageAnalyzeResult(
            regions=regions,
            ocr_spans=ocr.spans + table_ocr.spans,
            formula_spans=formula_result.spans,
            tables=table_result.tables,
            reading_order=reading_order,
            markdown=None,
            assets=[],
        )
