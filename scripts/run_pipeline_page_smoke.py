#!/usr/bin/env python3
"""Run a real image through the complete Pipeline V2 page analyzer."""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "services" / "pipeline-model-server" / "src"))
sys.path.insert(0, str(ROOT / "scripts"))

from run_pipeline_recognition_smoke import build_page  # noqa: E402
from uparser_pipeline_server.legacy_backends import register_pipeline_backends  # noqa: E402
from uparser_pipeline_server.registry import BackendRegistry  # noqa: E402
from uparser_pipeline_server.schemas import PageAnalyzeInput  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "pipeline/model-manifest.json")
    parser.add_argument("--device", default="cpu")
    parser.add_argument("--language", default="ch")
    parser.add_argument("--no-formula", action="store_true")
    parser.add_argument("--no-table", action="store_true")
    args = parser.parse_args()

    registry = BackendRegistry()
    register_pipeline_backends(registry, args.manifest.resolve(), args.device)
    page = build_page(args.image.resolve())
    started = time.perf_counter()
    result = registry.get("pages_analyze").infer(
        PageAnalyzeInput(
            page=page,
            language=args.language,
            formula_enabled=not args.no_formula,
            table_enabled=not args.no_table,
        )
    )
    elapsed = round(time.perf_counter() - started, 3)
    report = {
        "schema_version": 1,
        "image": str(args.image.resolve()),
        "device": args.device,
        "elapsed_seconds": elapsed,
        "reference_pipeline": json.loads(args.manifest.read_text(encoding="utf-8"))[
            "reference_pipeline"
        ],
        "counts": {
            "regions": len(result.regions),
            "ocr_spans": len(result.ocr_spans),
            "formula_spans": len(result.formula_spans),
            "tables": len(result.tables),
            "reading_order": len(result.reading_order),
        },
        "table_summaries": [
            {
                "region_id": table.region_id,
                "classifier_label": table.classifier_label,
                "cells": len(table.cells),
                "html_length": len(table.html),
                "html": table.html,
            }
            for table in result.tables
        ],
        "formula_sample": [span.latex for span in result.formula_spans[:10]],
        "ocr_sample": [span.text for span in result.ocr_spans[:10]],
        "reading_order": result.reading_order,
        "models": {
            stage: metadata.model_dump(mode="json")
            for stage, metadata in registry.metadata().items()
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps({"elapsed_seconds": elapsed, **report["counts"]}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
