"""Authoritative MinerU 3.4.5 page pipeline backed by PP-DocLayoutV2."""

from __future__ import annotations

import base64
import importlib.metadata
import json
import os
from pathlib import Path
import sys
import tempfile
from threading import RLock
from typing import Callable

from packaging.version import Version

from .legacy_backends import _decode_page
from .registry import BackendRegistry, RegisteredBackend
from .schemas import (
    DocumentAnalyzeInput,
    DocumentAnalyzeResult,
    ModelMetadata,
    PageAnalyzeInput,
    PageAnalyzeResult,
)


MINERU_345_COMMIT = "4fe4bde114a23ee5dd637eae99b767f4669bf58c"
MIN_TRANSFORMERS_VERSION = Version("4.57.3")
MAX_TRANSFORMERS_VERSION = Version("5.0.0")


def pp_formula_enabled() -> bool:
    return os.getenv("MINERU_FORMULA_CH_SUPPORT", "True").lower() in {
        "true",
        "1",
        "yes",
    }


def validate_transformers_runtime(
    installed_version: str | None = None,
    config_factory: Callable[[str], object] | None = None,
) -> None:
    actual = Version(
        installed_version or importlib.metadata.version("transformers")
    )
    if not MIN_TRANSFORMERS_VERSION <= actual < MAX_TRANSFORMERS_VERSION:
        raise RuntimeError(
            "MinerU 3.4.5 PP-DocLayoutV2 requires "
            f"transformers>=4.57.3,<5.0.0; found {actual}"
        )
    if config_factory is None:
        from transformers import AutoConfig

        config_factory = AutoConfig.for_model
    try:
        config_factory("hgnet_v2")
    except Exception as exc:
        raise RuntimeError(
            "transformers runtime cannot resolve the PP-DocLayoutV2 "
            "hgnet_v2 backbone"
        ) from exc


class MinerU345PageBackend:
    """Run the same analyze/finalize/Markdown path as MinerU's pipeline CLI."""

    def __init__(
        self,
        source_root: Path,
        config_path: Path,
        device: str | None = None,
        parser: Callable[..., None] | None = None,
        image_to_pdf: Callable[[bytes], bytes] | None = None,
    ) -> None:
        self.source_root = source_root.resolve()
        self.config_path = config_path.resolve()
        self.device = device
        self.parser = parser
        self.image_to_pdf = image_to_pdf
        self.inference_lock = RLock()

    def validate_configuration(self) -> None:
        if not (self.source_root / "mineru" / "__init__.py").is_file():
            raise FileNotFoundError(f"MinerU source tree not found: {self.source_root}")
        if not self.config_path.is_file():
            raise FileNotFoundError(f"MinerU model config not found: {self.config_path}")
        config = json.loads(self.config_path.read_text(encoding="utf-8"))
        models_dir = config.get("models-dir", {})
        root_value = models_dir.get("pipeline") if isinstance(models_dir, dict) else None
        if not root_value:
            raise ValueError(f"pipeline models-dir is missing in {self.config_path}")
        root = Path(root_value).expanduser()
        if (root / "models").is_dir():
            root = root / "models"
        validate_transformers_runtime()
        formula_path = (
            "MFR/pp_formulanet_plus_m/PP-FormulaNet_plus-M.pth"
            if pp_formula_enabled()
            else "MFR/unimernet_hf_small_2503/model.safetensors"
        )
        required = [
            "Layout/PP-DocLayoutV2/model.safetensors",
            formula_path,
            "OCR/paddleocr_torch/ch_PP-OCRv6_small_det_infer.safetensors",
            "OCR/paddleocr_torch/ch_PP-OCRv6_small_rec_infer.safetensors",
            "TabCls/paddle_table_cls/PP-LCNet_x1_0_table_cls.onnx",
            "TabRec/SlanetPlus/slanet-plus.onnx",
            "TabRec/UnetStructure/unet.onnx",
        ]
        missing = [str(root / relative) for relative in required if not (root / relative).is_file()]
        if missing:
            raise FileNotFoundError("MinerU 3.4.5 model assets missing: " + ", ".join(missing))

    def _load_parser(self) -> Callable[..., None]:
        if self.parser is not None:
            return self.parser
        if not (self.source_root / "mineru" / "__init__.py").is_file():
            raise FileNotFoundError(f"MinerU source tree not found: {self.source_root}")
        if not self.config_path.is_file():
            raise FileNotFoundError(f"MinerU model config not found: {self.config_path}")

        # MinerU reads these settings at module import time.
        os.environ["MINERU_TOOLS_CONFIG_JSON"] = str(self.config_path)
        if self.device:
            os.environ["MINERU_DEVICE_MODE"] = self.device
        source = str(self.source_root)
        if source not in sys.path:
            sys.path.insert(0, source)
        from mineru.cli.common import do_parse
        from mineru.utils.pdf_image_tools import images_bytes_to_pdf_bytes

        self.parser = do_parse
        self.image_to_pdf = images_bytes_to_pdf_bytes
        return do_parse

    def __call__(self, item: PageAnalyzeInput) -> PageAnalyzeResult:
        return self.infer_batch([item])[0]

    def infer_batch(self, items: list[PageAnalyzeInput]) -> list[PageAnalyzeResult]:
        if not items:
            return []
        # Validate both payload dimensions and image decodability before handing
        # bytes to MinerU's image-to-PDF preparation path.
        self._load_parser()
        pdf_payloads = []
        for item in items:
            with _decode_page(item.page):
                payload = base64.b64decode(item.page.image.base64_data, validate=True)
            pdf_payloads.append(
                self.image_to_pdf(payload) if self.image_to_pdf is not None else payload
            )
        outputs = self._run_pipeline(
            pdf_payloads,
            [item.language for item in items],
            [(item.formula_enabled, item.table_enabled) for item in items],
        )
        return [
            PageAnalyzeResult(
                regions=[],
                ocr_spans=[],
                formula_spans=[],
                tables=[],
                reading_order=[],
                markdown=markdown,
                assets=assets,
            )
            for markdown, assets in outputs
        ]

    def infer_document(self, item: DocumentAnalyzeInput) -> DocumentAnalyzeResult:
        return self.infer_documents([item])[0]

    def infer_documents(
        self, items: list[DocumentAnalyzeInput]
    ) -> list[DocumentAnalyzeResult]:
        if not items:
            return []
        max_bytes = int(os.getenv("UPARSER_PIPELINE_MAX_PDF_BYTES", "536870912"))
        payloads = []
        for item in items:
            estimated_bytes = len(item.document.base64_data) * 3 // 4
            if estimated_bytes > max_bytes:
                raise ValueError(
                    f"encoded PDF exceeds byte limit: {estimated_bytes} > {max_bytes}"
                )
            payload = base64.b64decode(item.document.base64_data, validate=True)
            if len(payload) > max_bytes:
                raise ValueError(f"decoded PDF exceeds byte limit: {len(payload)} > {max_bytes}")
            if not payload.startswith(b"%PDF-"):
                raise ValueError("document payload is not a PDF")
            payloads.append(payload)
        outputs = self._run_pipeline(
            payloads,
            [item.language for item in items],
            [(item.formula_enabled, item.table_enabled) for item in items],
        )
        return [
            DocumentAnalyzeResult(markdown=markdown, assets=assets)
            for markdown, assets in outputs
        ]

    def _run_pipeline(
        self,
        pdf_payloads: list[bytes],
        languages: list[str],
        feature_flags: list[tuple[bool, bool]],
    ) -> list[tuple[str, list[dict]]]:
        flags = set(feature_flags)
        if len(flags) != 1:
            raise ValueError("all items in a MinerU batch must use the same feature flags")
        formula_enabled, table_enabled = feature_flags[0]
        parser = self._load_parser()
        names = [f"document_{index}" for index in range(len(pdf_payloads))]
        with self.inference_lock, tempfile.TemporaryDirectory(prefix="uparser-mineru345-") as work:
            parser(
                work,
                names,
                pdf_payloads,
                languages,
                backend="pipeline",
                parse_method="ocr",
                formula_enable=formula_enabled,
                table_enable=table_enabled,
                f_draw_layout_bbox=False,
                f_draw_span_bbox=False,
                f_dump_md=True,
                f_dump_middle_json=False,
                f_dump_model_output=False,
                f_dump_orig_pdf=False,
                f_dump_content_list=False,
            )
            outputs = []
            return_assets = os.getenv(
                "UPARSER_PIPELINE_RETURN_ASSETS", "True"
            ).lower() in {"true", "1", "yes"}
            for name in names:
                markdown_path = Path(work) / name / "ocr" / f"{name}.md"
                if not markdown_path.is_file():
                    raise RuntimeError(
                        f"MinerU did not create finalized Markdown: {markdown_path}"
                    )
                assets = []
                image_dir = markdown_path.parent / "images"
                if return_assets and image_dir.is_dir():
                    for path in sorted(image_dir.iterdir()):
                        suffix = path.suffix.lower()
                        if not path.is_file() or suffix not in {".png", ".jpg", ".jpeg"}:
                            continue
                        assets.append(
                            {
                                "path": f"images/{path.name}",
                                "media_type": "image/png" if suffix == ".png" else "image/jpeg",
                                "base64_data": base64.b64encode(path.read_bytes()).decode("ascii"),
                            }
                        )
                outputs.append((markdown_path.read_text(encoding="utf-8"), assets))

        return outputs


def register_mineru345_page_backend(
    registry: BackendRegistry,
    source_root: Path,
    config_path: Path,
    device: str | None = None,
) -> None:
    backend = MinerU345PageBackend(source_root, config_path, device)
    backend.validate_configuration()
    formula_model = "PP-FormulaNet-plus-M" if pp_formula_enabled() else "UniMERNet-small"
    registry.register(
        "pages_analyze",
        RegisteredBackend(
            infer=backend,
            infer_batch=backend.infer_batch,
            metadata=ModelMetadata(
                name="uparser_pipeline_v2_mineru_3_4_5",
                revision=MINERU_345_COMMIT,
                weight_sha256=None,
                runtime=f"mineru-3.4.5/PP-DocLayoutV2/{formula_model}/official-finalize",
            ),
        ),
    )
    registry.register(
        "documents_analyze",
        RegisteredBackend(
            infer=backend.infer_document,
            infer_batch=backend.infer_documents,
            metadata=ModelMetadata(
                name="uparser_pipeline_v2_mineru_3_4_5",
                revision=MINERU_345_COMMIT,
                weight_sha256=None,
                runtime=f"mineru-3.4.5/PP-DocLayoutV2/{formula_model}/document-finalize",
            ),
        ),
    )
