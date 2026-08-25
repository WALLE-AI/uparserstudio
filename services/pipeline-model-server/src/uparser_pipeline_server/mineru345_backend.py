"""Authoritative MinerU 3.4.5 page pipeline backed by PP-DocLayoutV2."""

from __future__ import annotations

import base64
import os
from pathlib import Path
import sys
import tempfile
from threading import RLock
from typing import Callable

from .legacy_backends import _decode_page
from .registry import BackendRegistry, RegisteredBackend
from .schemas import ModelMetadata, PageAnalyzeInput, PageAnalyzeResult


MINERU_345_COMMIT = "4fe4bde114a23ee5dd637eae99b767f4669bf58c"


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

    def _load_parser(self) -> Callable[..., None]:
        if self.parser is not None:
            return self.parser
        if not (self.source_root / "mineru" / "__init__.py").is_file():
            raise FileNotFoundError(f"MinerU source tree not found: {self.source_root}")
        if not self.config_path.is_file():
            raise FileNotFoundError(f"MinerU model config not found: {self.config_path}")

        # MinerU reads these settings at module import time.
        os.environ.setdefault("MINERU_TOOLS_CONFIG_JSON", str(self.config_path))
        if self.device:
            os.environ.setdefault("MINERU_DEVICE_MODE", self.device)
        source = str(self.source_root)
        if source not in sys.path:
            sys.path.insert(0, source)
        from mineru.cli.common import do_parse
        from mineru.utils.pdf_image_tools import images_bytes_to_pdf_bytes

        self.parser = do_parse
        self.image_to_pdf = images_bytes_to_pdf_bytes
        return do_parse

    def __call__(self, item: PageAnalyzeInput) -> PageAnalyzeResult:
        # Validate both payload dimensions and image decodability before handing
        # bytes to MinerU's image-to-PDF preparation path.
        with _decode_page(item.page):
            payload = base64.b64decode(item.page.image.base64_data, validate=True)

        parser = self._load_parser()
        pdf_payload = self.image_to_pdf(payload) if self.image_to_pdf is not None else payload
        with self.inference_lock, tempfile.TemporaryDirectory(prefix="uparser-mineru345-") as work:
            parser(
                work,
                ["page"],
                [pdf_payload],
                [item.language],
                backend="pipeline",
                parse_method="ocr",
                formula_enable=item.formula_enabled,
                table_enable=item.table_enabled,
                f_draw_layout_bbox=False,
                f_draw_span_bbox=False,
                f_dump_md=True,
                f_dump_middle_json=False,
                f_dump_model_output=False,
                f_dump_orig_pdf=False,
                f_dump_content_list=False,
            )
            markdown_path = Path(work) / "page" / "ocr" / "page.md"
            if not markdown_path.is_file():
                raise RuntimeError(f"MinerU did not create finalized Markdown: {markdown_path}")
            markdown = markdown_path.read_text(encoding="utf-8")

        return PageAnalyzeResult(
            regions=[],
            ocr_spans=[],
            formula_spans=[],
            tables=[],
            reading_order=[],
            markdown=markdown,
        )


def register_mineru345_page_backend(
    registry: BackendRegistry,
    source_root: Path,
    config_path: Path,
    device: str | None = None,
) -> None:
    backend = MinerU345PageBackend(source_root, config_path, device)
    registry.register(
        "pages_analyze",
        RegisteredBackend(
            infer=backend,
            metadata=ModelMetadata(
                name="uparser_pipeline_v2_mineru_3_4_5",
                revision=MINERU_345_COMMIT,
                weight_sha256=None,
                runtime="mineru-3.4.5/PP-DocLayoutV2/official-finalize",
            ),
        ),
    )
