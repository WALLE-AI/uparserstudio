#!/usr/bin/env python3
"""Run one real legacy layout/MFD page through the Pipeline V2 adapters."""

from __future__ import annotations

import argparse
import base64
import json
import time
from pathlib import Path

from PIL import Image

from uparser_pipeline_server.compat import merge_layout_and_mfd
from uparser_pipeline_server.legacy_backends import register_legacy_detection_backends
from uparser_pipeline_server.registry import BackendRegistry
from uparser_pipeline_server.schemas import EncodedImage, ImageDimensions, PageImage


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--device", default="cpu")
    args = parser.parse_args()

    with Image.open(args.image) as source:
        width, height = source.size
    page = PageImage(
        page_id=args.image.stem,
        image=EncodedImage(
            media_type="image/jpeg" if args.image.suffix.lower() in {".jpg", ".jpeg"} else "image/png",
            base64_data=base64.b64encode(args.image.read_bytes()).decode("ascii"),
        ),
        dimensions=ImageDimensions(width=width, height=height),
    )

    registry = BackendRegistry()
    register_legacy_detection_backends(registry, args.manifest, args.device)
    layout_backend = registry.get("layout")
    mfd_backend = registry.get("formula_detect")
    assert layout_backend is not None and mfd_backend is not None

    started = time.perf_counter()
    layout = layout_backend.infer(page)
    layout_seconds = time.perf_counter() - started
    started = time.perf_counter()
    formulas = mfd_backend.infer(page)
    mfd_seconds = time.perf_counter() - started
    merged = merge_layout_and_mfd(layout.regions, formulas.regions)

    output = {
        "page_id": page.page_id,
        "image": str(args.image.resolve()),
        "device": args.device,
        "elapsed_seconds": {"layout": layout_seconds, "mfd": mfd_seconds},
        "models": {
            "layout": layout_backend.metadata.model_dump(mode="json"),
            "formula_detect": mfd_backend.metadata.model_dump(mode="json"),
        },
        "layout_regions": [item.model_dump(mode="json") for item in layout.regions],
        "formula_regions": [item.model_dump(mode="json") for item in formulas.regions],
        "merged_regions": [item.model_dump(mode="json") for item in merged],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(output, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        f"layout={len(layout.regions)} ({layout_seconds:.3f}s), "
        f"mfd={len(formulas.regions)} ({mfd_seconds:.3f}s), merged={len(merged)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
