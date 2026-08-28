"""Real legacy detection backends with current MinerU-compatible outputs."""

from __future__ import annotations

import base64
import io
import json
import os
from pathlib import Path
from threading import RLock
from typing import Callable

from PIL import Image

from .compat import LegacyDetection, normalize_layout, normalize_mfd, poly_to_bbox
from .registry import BackendRegistry, ModelRegistry, RegisteredBackend
from .schemas import (
    FormulaDetectionResult,
    LayoutResult,
    ModelMetadata,
    PageImage,
)


def _decode_page(page: PageImage) -> Image.Image:
    max_pixels = int(os.getenv("UPARSER_PIPELINE_MAX_IMAGE_PIXELS", "50000000"))
    max_bytes = int(os.getenv("UPARSER_PIPELINE_MAX_IMAGE_BYTES", "104857600"))
    declared_pixels = page.dimensions.width * page.dimensions.height
    if declared_pixels > max_pixels:
        raise ValueError(f"declared image exceeds pixel limit: {declared_pixels} > {max_pixels}")
    estimated_bytes = len(page.image.base64_data) * 3 // 4
    if estimated_bytes > max_bytes:
        raise ValueError(f"encoded image exceeds byte limit: {estimated_bytes} > {max_bytes}")
    try:
        payload = base64.b64decode(page.image.base64_data, validate=True)
    except ValueError as exc:
        raise ValueError("image payload is not valid base64") from exc
    if len(payload) > max_bytes:
        raise ValueError(f"decoded image exceeds byte limit: {len(payload)} > {max_bytes}")
    try:
        image = Image.open(io.BytesIO(payload)).convert("RGB")
    except Exception as exc:
        raise ValueError("image payload cannot be decoded") from exc
    expected = (page.dimensions.width, page.dimensions.height)
    if image.size != expected:
        image.close()
        raise ValueError(f"decoded image dimensions {image.size} do not match declared {expected}")
    return image


def _asset(manifest: dict, logical_model: str, role: str) -> dict:
    for item in manifest.get("assets", []):
        if item.get("logical_model") == logical_model and item.get("role") == role:
            if not item.get("exists"):
                raise FileNotFoundError(item.get("path"))
            return item
    raise ValueError(f"manifest has no asset for {logical_model}/{role}")


def _layout_detections(raw_results: list[dict]) -> list[LegacyDetection]:
    detections = []
    for item in raw_results:
        detections.append(
            LegacyDetection(
                class_id=int(item["category_id"]),
                bbox=poly_to_bbox(item["poly"]),
                confidence=float(item["score"]) if item.get("score") is not None else None,
            )
        )
    return detections


def _mfd_detections(raw_result) -> list[LegacyDetection]:
    detections = []
    boxes = raw_result.boxes
    for xyxy, confidence, class_id in zip(
        boxes.xyxy.cpu(), boxes.conf.cpu(), boxes.cls.cpu()
    ):
        bbox = tuple(float(value.item()) for value in xyxy)
        detections.append(
            LegacyDetection(
                class_id=int(class_id.item()),
                bbox=bbox,
                confidence=float(confidence.item()),
            )
        )
    return detections


def _detector_device(device: str):
    """Prevent YOLO helpers from replacing an existing CUDA visibility mask."""
    if device.startswith("cuda"):
        import torch

        return torch.device("cuda:0" if device == "cuda" else device)
    return device


class LegacyLayoutBackend:
    def __init__(
        self,
        weight: Path,
        device: str,
        loader: Callable[[Path, str], object] | None = None,
    ):
        self.weight = weight
        self.device = device
        self.loader = loader or self._default_loader
        self.models = ModelRegistry()
        self.inference_lock = RLock()

    @staticmethod
    def _default_loader(weight: Path, device: str):
        from magic_pdf.model.sub_modules.layout.doclayout_yolo.DocLayoutYOLO import (
            DocLayoutYOLOModel,
        )

        return DocLayoutYOLOModel(str(weight), _detector_device(device))

    def __call__(self, page: PageImage) -> LayoutResult:
        model = self.models.get_or_load(
            ("doclayout_yolo", str(self.weight), self.device),
            lambda: self.loader(self.weight, self.device),
        )
        with _decode_page(page) as image, self.inference_lock:
            raw_results = model.predict(image)
        return LayoutResult(regions=normalize_layout(page.page_id, _layout_detections(raw_results)))


class LegacyMfdBackend:
    def __init__(
        self,
        weight: Path,
        device: str,
        loader: Callable[[Path, str], object] | None = None,
    ):
        self.weight = weight
        self.device = device
        self.loader = loader or self._default_loader
        self.models = ModelRegistry()
        self.inference_lock = RLock()

    @staticmethod
    def _default_loader(weight: Path, device: str):
        from magic_pdf.model.sub_modules.mfd.yolov8.YOLOv8 import YOLOv8MFDModel

        return YOLOv8MFDModel(str(weight), _detector_device(device))

    def __call__(self, page: PageImage) -> FormulaDetectionResult:
        model = self.models.get_or_load(
            ("yolo_v8_mfd", str(self.weight), self.device),
            lambda: self.loader(self.weight, self.device),
        )
        with _decode_page(page) as image, self.inference_lock:
            raw_result = model.predict(image)
        return FormulaDetectionResult(
            regions=normalize_mfd(page.page_id, _mfd_detections(raw_result))
        )


def register_legacy_detection_backends(
    registry: BackendRegistry,
    manifest_path: Path,
    device_override: str | None = None,
) -> None:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    device = device_override or manifest.get("runtime", {}).get("device", "cpu")
    layout_asset = _asset(manifest, "layout", "weight_or_config")
    mfd_asset = _asset(manifest, "formula_detection", "weight_or_config")
    layout = LegacyLayoutBackend(Path(layout_asset["path"]), device)
    mfd = LegacyMfdBackend(Path(mfd_asset["path"]), device)
    registry.register(
        "layout",
        RegisteredBackend(
            infer=layout,
            metadata=ModelMetadata(
                name="doclayout_yolo",
                revision="legacy-compatible-current-mineru-labels-v1",
                weight_sha256=layout_asset.get("sha256"),
                runtime="magic_pdf",
            ),
        ),
    )
    registry.register(
        "formula_detect",
        RegisteredBackend(
            infer=mfd,
            metadata=ModelMetadata(
                name="yolo_v8_mfd",
                revision="legacy-compatible-current-mineru-labels-v1",
                weight_sha256=mfd_asset.get("sha256"),
                runtime="magic_pdf",
            ),
        ),
    )


def register_pipeline_backends(
    registry: BackendRegistry,
    manifest_path: Path,
    device_override: str | None = None,
    *,
    include_page_analyzer: bool = True,
) -> None:
    """Register every implemented stage without pretending unfinished stages exist."""
    from .page_analyzer import LayoutReader, PipelinePageAnalyzer
    from .recognition_backends import CurrentMineruMfrBackend, LegacyOcrBackend
    from .table_backend import CurrentMineruSlanetBackend

    register_legacy_detection_backends(registry, manifest_path, device_override)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    device = device_override or manifest.get("runtime", {}).get("device", "cpu")
    ocr_asset = _asset(manifest, "ocr", "detector_weight")
    mfr_asset = _asset(manifest, "formula_recognition", "weight_or_config")
    mfr_weight = next(
        item
        for item in manifest["assets"]
        if item.get("logical_model") == "formula_recognition"
        and Path(item["path"]).name == "model.safetensors"
    )
    table_asset = _asset(manifest, "table", "structure_weight")
    reading_order_asset = _asset(manifest, "reading_order", "weight")
    reference_root = Path(manifest["reference_pipeline"]["root"])
    models_root = Path(ocr_asset["path"]).parents[2]
    mfr_weight_dir = Path(mfr_asset["path"]).parent

    registry.register(
        "ocr",
        RegisteredBackend(
            infer=LegacyOcrBackend(models_root, device),
            metadata=ModelMetadata(
                name="paddleocr_torch",
                revision="mineru-3.4.4-crop-mask-semantics-v1",
                weight_sha256=ocr_asset.get("sha256"),
                runtime="magic_pdf-weights/current-mineru-semantics",
            ),
        ),
    )
    registry.register(
        "formula_recognize",
        RegisteredBackend(
            infer=CurrentMineruMfrBackend(mfr_weight_dir, reference_root, device),
            metadata=ModelMetadata(
                name="unimernet_small",
                revision=manifest["reference_pipeline"]["git_commit"],
                weight_sha256=mfr_weight.get("sha256"),
                runtime="mineru-3.4.4-source",
            ),
        ),
    )
    registry.register(
        "table",
        RegisteredBackend(
            infer=CurrentMineruSlanetBackend(
                Path(table_asset["path"]), reference_root
            ),
            metadata=ModelMetadata(
                name="slanet_plus",
                revision="mineru-3.4.4-wireless-only-v1",
                weight_sha256=table_asset.get("sha256"),
                runtime="mineru-3.4.4-source/onnxruntime",
            ),
        ),
    )
    if include_page_analyzer:
        registry.register(
            "pages_analyze",
            RegisteredBackend(
                infer=PipelinePageAnalyzer(
                    registry,
                    LayoutReader(Path(reading_order_asset["path"]).parent, device),
                ),
                metadata=ModelMetadata(
                    name="uparser_pipeline_v2",
                    revision=manifest["reference_pipeline"]["git_commit"],
                    weight_sha256=None,
                    runtime="mineru-3.4.4-compatible-stage-graph",
                ),
            ),
        )
