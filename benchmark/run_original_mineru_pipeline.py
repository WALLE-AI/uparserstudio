#!/usr/bin/env python3
"""Run the legacy MinerU pipeline with the exact models used by Pipeline V2."""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import time
from pathlib import Path

import fitz


ROOT = Path(__file__).resolve().parents[1]
OMNI_DATA = ROOT / "benchmark" / "OmniDocBenchData" / "OmniDocBench.json"
OMNI_IMAGES = ROOT / "benchmark" / "OmniDocBenchData" / "images"
ODL_PDFS = ROOT / "opensource" / "opendataloader-bench" / "pdfs"


def omni_paths(dataset: Path) -> list[Path]:
    items = json.loads(dataset.read_text(encoding="utf-8"))
    paths = []
    for item in items:
        raw = item.get("image_path") or item.get("page_info", {}).get("image_path")
        if not raw:
            continue
        path = Path(raw)
        if not path.is_absolute():
            direct = dataset.parent / path
            path = direct if direct.exists() else OMNI_IMAGES / path
        paths.append(path)
    return paths


def image_pdf_bytes(path: Path) -> bytes:
    with fitz.open(path) as image_document:
        return image_document.convert_to_pdf()


def source_bytes(path: Path, input_kind: str) -> bytes:
    if input_kind == "image":
        return image_pdf_bytes(path)
    return path.read_bytes()


def output_markdown(work_dir: Path, stem: str, method: str) -> Path:
    return work_dir / stem / method / f"{stem}.md"


def process_batch(
    paths: list[Path],
    *,
    input_kind: str,
    output_dir: Path,
    work_dir: Path,
    method: str,
    lang: str | None,
) -> None:
    from magic_pdf.data.dataset import PymuDocDataset
    from magic_pdf.tools.common import batch_do_parse

    datasets = [PymuDocDataset(source_bytes(path, input_kind), lang=lang) for path in paths]
    stems = [path.stem for path in paths]
    batch_do_parse(
        str(work_dir),
        stems,
        datasets,
        method,
        False,
        f_draw_span_bbox=False,
        f_draw_layout_bbox=False,
        f_dump_md=True,
        f_dump_middle_json=False,
        f_dump_model_json=False,
        f_dump_orig_pdf=False,
        f_dump_content_list=False,
        lang=lang,
        formula_enable=True,
        table_enable=True,
    )
    for stem in stems:
        source = output_markdown(work_dir, stem, method)
        if not source.is_file():
            raise FileNotFoundError(f"MinerU did not create {source}")
        shutil.copy2(source, output_dir / f"{stem}.md")
        shutil.rmtree(work_dir / stem)


def run(args: argparse.Namespace) -> dict:
    if args.benchmark == "omni":
        paths = omni_paths(args.dataset)
        input_kind = "image"
    else:
        paths = sorted(args.input.glob("*.pdf"))
        input_kind = "pdf"
    if args.limit:
        paths = paths[: args.limit]

    args.output.mkdir(parents=True, exist_ok=True)
    args.work.mkdir(parents=True, exist_ok=True)
    pending = [path for path in paths if not (args.output / f"{path.stem}.md").is_file()]
    failures = []
    started = time.perf_counter()
    completed = 0

    for offset in range(0, len(pending), args.batch_size):
        batch = pending[offset : offset + args.batch_size]
        try:
            process_batch(
                batch,
                input_kind=input_kind,
                output_dir=args.output,
                work_dir=args.work,
                method=args.method,
                lang=args.lang,
            )
            completed += len(batch)
        except Exception as batch_error:
            print(f"batch failed, retrying individually: {batch_error}", file=sys.stderr)
            for path in batch:
                try:
                    process_batch(
                        [path],
                        input_kind=input_kind,
                        output_dir=args.output,
                        work_dir=args.work,
                        method=args.method,
                        lang=args.lang,
                    )
                    completed += 1
                except Exception as exc:
                    failures.append({"path": str(path), "error": repr(exc)})
        done = min(offset + len(batch), len(pending))
        print(f"completed={done}/{len(pending)} failures={len(failures)}", flush=True)

    wall_seconds = time.perf_counter() - started
    summary = {
        "benchmark": args.benchmark,
        "method": args.method,
        "language": args.lang,
        "documents": len(paths),
        "skipped_existing": len(paths) - len(pending),
        "completed_this_run": completed,
        "prediction_count": sum(1 for path in paths if (args.output / f"{path.stem}.md").is_file()),
        "failures": failures,
        "wall_seconds_this_run": wall_seconds,
        "seconds_per_completed_item": wall_seconds / completed if completed else None,
        "output": str(args.output),
    }
    args.summary.parent.mkdir(parents=True, exist_ok=True)
    args.summary.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2), flush=True)
    return summary


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("benchmark", choices=("omni", "odl"))
    parser.add_argument("--method", choices=("ocr", "auto", "txt"), default="ocr")
    parser.add_argument("--lang", default=None)
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--dataset", type=Path, default=OMNI_DATA)
    parser.add_argument("--input", type=Path, default=ODL_PDFS)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--summary", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    result = run(parse_args())
    raise SystemExit(1 if result["failures"] else 0)
