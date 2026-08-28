#!/usr/bin/env python3
"""Run real OCR/MFR pipeline stages on one image and record reproducible evidence."""

from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import time
from pathlib import Path

from PIL import Image


ROOT = Path(__file__).resolve().parents[1]
SERVICE_SRC = ROOT / "services" / "pipeline-model-server" / "src"
sys.path.insert(0, str(SERVICE_SRC))

from uparser_pipeline_server.compat import merge_layout_and_mfd  # noqa: E402
from uparser_pipeline_server.legacy_backends import register_pipeline_backends  # noqa: E402
from uparser_pipeline_server.registry import BackendRegistry  # noqa: E402
from uparser_pipeline_server.schemas import (  # noqa: E402
    EncodedImage,
    FormulaRecognitionInput,
    ImageDimensions,
    OcrPageInput,
    PageImage,
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--image",
        type=Path,
        default=ROOT
        / "benchmark/OmniDocBenchData/images/page-942ac90d-4704-43f3-9286-719c7be9e655.png",
    )
    parser.add_argument("--manifest", type=Path, default=ROOT / "pipeline/model-manifest.json")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--device", default=os.getenv("UPARSER_PIPELINE_DEVICE", "cpu"))
    parser.add_argument(
        "--stages",
        nargs="+",
        choices=("ocr", "mfr"),
        default=("ocr", "mfr"),
    )
    return parser.parse_args()


def build_page(image_path: Path) -> PageImage:
    payload = image_path.read_bytes()
    with Image.open(image_path) as image:
        width, height = image.size
        media_type = "image/jpeg" if image.format == "JPEG" else "image/png"
    return PageImage(
        page_id=image_path.stem,
        image=EncodedImage(
            media_type=media_type,
            base64_data=base64.b64encode(payload).decode("ascii"),
        ),
        dimensions=ImageDimensions(width=width, height=height),
    )


def timed(callable_):
    started = time.perf_counter()
    result = callable_()
    return result, round(time.perf_counter() - started, 3)


def main() -> int:
    args = parse_args()
    registry = BackendRegistry()
    register_pipeline_backends(registry, args.manifest.resolve(), args.device)
    page = build_page(args.image.resolve())

    layout, layout_seconds = timed(lambda: registry.get("layout").infer(page))
    mfd, mfd_seconds = timed(lambda: registry.get("formula_detect").infer(page))
    regions = merge_layout_and_mfd(layout.regions, mfd.regions)
    report = {
        "schema_version": 1,
        "image": str(args.image.resolve()),
        "device": args.device,
        "reference_pipeline": json.loads(args.manifest.read_text(encoding="utf-8"))[
            "reference_pipeline"
        ],
        "layout": {"regions": len(layout.regions), "seconds": layout_seconds},
        "mfd": {"regions": len(mfd.regions), "seconds": mfd_seconds},
    }

    if "ocr" in args.stages:
        result, seconds = timed(
            lambda: registry.get("ocr").infer(
                OcrPageInput(
                    page=page,
                    layout_regions=regions,
                    formula_regions=mfd.regions,
                    language="ch",
                )
            )
        )
        report["ocr"] = {
            "spans": len(result.spans),
            "seconds": seconds,
            "sample": [span.text for span in result.spans[:10]],
            "model": registry.get("ocr").metadata.model_dump(mode="json"),
        }

    if "mfr" in args.stages:
        result, seconds = timed(
            lambda: registry.get("formula_recognize").infer(
                FormulaRecognitionInput(page=page, formula_regions=mfd.regions)
            )
        )
        report["mfr"] = {
            "spans": len(result.spans),
            "seconds": seconds,
            "sample": [span.latex for span in result.spans[:10]],
            "model": registry.get("formula_recognize").metadata.model_dump(mode="json"),
        }

    rendered = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
