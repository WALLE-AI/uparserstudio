#!/usr/bin/env python3
"""Generate Pipeline V2 predictions for OmniDocBench or OpenDataLoader Bench."""

from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor, as_completed
import io
import json
import os
from pathlib import Path
import statistics
import subprocess
import time
from datetime import datetime, timezone

import httpx
from PIL import Image


ROOT = Path(__file__).resolve().parents[1]
OMNI_DATA = ROOT / "benchmark" / "OmniDocBenchData" / "OmniDocBench.json"
OMNI_IMAGES = ROOT / "benchmark" / "OmniDocBenchData" / "images"
ODL_ROOT = ROOT / "opensource" / "opendataloader-bench"


def _bbox_contains(outer, inner) -> bool:
    ox0, oy0, ox1, oy1 = outer
    ix0, iy0, ix1, iy1 = inner
    center_x = (ix0 + ix1) / 2
    center_y = (iy0 + iy1) / 2
    return ox0 <= center_x <= ox1 and oy0 <= center_y <= oy1


def _table_html(markup: str) -> str:
    start = markup.find("<table")
    end = markup.rfind("</table>")
    return markup[start : end + len("</table>")] if start >= 0 and end >= start else markup


def render_markdown(result: dict) -> str:
    authoritative = result.get("markdown")
    if authoritative is not None:
        return authoritative

    regions = {region["region_id"]: region for region in result.get("regions", [])}
    ocr_by_parent: dict[str, list[dict]] = {}
    for span in result.get("ocr_spans", []):
        parent = span.get("parent_region_id")
        if parent:
            ocr_by_parent.setdefault(parent, []).append(span)
    formulas = {span["region_id"]: span for span in result.get("formula_spans", [])}
    tables = {table["region_id"]: table for table in result.get("tables", [])}
    table_boxes = [regions[region_id]["bbox"] for region_id in tables if region_id in regions]

    blocks = []
    emitted = set()
    for region_id in result.get("reading_order", []):
        if region_id in emitted:
            continue
        emitted.add(region_id)
        region = regions.get(region_id)
        if not region:
            continue
        label = region["label"]
        if region_id in tables:
            markup = _table_html(tables[region_id].get("html", "")).strip()
            if markup:
                blocks.append(markup)
            continue
        if region_id in formulas:
            if any(_bbox_contains(table_bbox, region["bbox"]) for table_bbox in table_boxes):
                continue
            latex = formulas[region_id].get("latex", "").strip()
            if latex:
                blocks.append(f"${latex}$" if label == "inline_formula" else f"$$\n{latex}\n$$")
            continue
        spans = sorted(
            ocr_by_parent.get(region_id, []),
            key=lambda span: (
                min(point[1] for point in span["polygon"]["points"]),
                min(point[0] for point in span["polygon"]["points"]),
            ),
        )
        text = "\n".join(span["text"].strip() for span in spans if span["text"].strip())
        if not text:
            continue
        if label == "doc_title":
            text = f"# {text}"
        elif label == "paragraph_title":
            text = f"## {text}"
        blocks.append(text)
    return "\n\n".join(blocks).strip() + "\n"


def _encoded_page(image: Image.Image, page_id: str) -> dict:
    rgb = image.convert("RGB")
    buffer = io.BytesIO()
    rgb.save(buffer, format="JPEG", quality=95)
    return {
        "page_id": page_id,
        "image": {
            "media_type": "image/jpeg",
            "base64_data": base64.b64encode(buffer.getvalue()).decode("ascii"),
        },
        "dimensions": {"width": rgb.width, "height": rgb.height},
        "rotation_degrees": 0,
    }


def _file_page(path: Path) -> dict:
    with Image.open(path) as image:
        width, height = image.size
    suffix = path.suffix.lower()
    if suffix in {".png", ".jpg", ".jpeg"}:
        media_type = "image/png" if suffix == ".png" else "image/jpeg"
        return {
            "page_id": path.stem,
            "image": {
                "media_type": media_type,
                "base64_data": base64.b64encode(path.read_bytes()).decode("ascii"),
            },
            "dimensions": {"width": width, "height": height},
            "rotation_degrees": 0,
        }
    with Image.open(path) as image:
        return _encoded_page(image, path.stem)


def _analyze(client: httpx.Client, endpoint: str, page: dict, language: str) -> dict:
    return _analyze_pages(client, endpoint, [page], language)[0]


def _analyze_pages(
    client: httpx.Client, endpoint: str, pages: list[dict], language: str
) -> list[dict]:
    response = client.post(
        f"{endpoint.rstrip('/')}/v2/pipeline/pages:analyze",
        json={
            "schema_version": "uparser.pipeline.v2",
            "request_id": f"eval-{pages[0]['page_id']}",
            "items": [
                {
                    "page": page,
                    "language": language,
                    "formula_enabled": True,
                    "table_enabled": True,
                }
                for page in pages
            ],
        },
    )
    response.raise_for_status()
    results = []
    for item in response.json()["items"]:
        if item.get("error"):
            raise RuntimeError(f"{item['error']['code']}: {item['error']['message']}")
        fallbacks = [
            warning for warning in item.get("warnings", [])
            if warning.get("code") == "batch_fallback"
        ]
        if fallbacks:
            raise RuntimeError(f"formal evaluation rejects batch fallback: {fallbacks[0]['message']}")
        results.append(item["result"])
    return results


def _analyze_document(client: httpx.Client, endpoint: str, path: Path, language: str) -> str:
    payload = path.read_bytes()
    response = client.post(
        f"{endpoint.rstrip('/')}/v2/pipeline/documents:analyze",
        json={
            "schema_version": "uparser.pipeline.v2",
            "request_id": f"eval-{path.stem}",
            "items": [
                {
                    "document": {
                        "document_id": path.stem,
                        "media_type": "application/pdf",
                        "base64_data": base64.b64encode(payload).decode("ascii"),
                    },
                    "language": language,
                    "formula_enabled": True,
                    "table_enabled": True,
                }
            ],
        },
    )
    response.raise_for_status()
    item = response.json()["items"][0]
    if item.get("error"):
        raise RuntimeError(f"{item['error']['code']}: {item['error']['message']}")
    return item["result"]["markdown"]


def _omni_paths(dataset: Path, limit: int) -> list[Path]:
    items = json.loads(dataset.read_text(encoding="utf-8"))
    paths = []
    for item in items[:limit] if limit else items:
        raw = item.get("image_path") or item.get("page_info", {}).get("image_path")
        if not raw:
            continue
        path = Path(raw)
        if not path.is_absolute():
            direct = dataset.parent / path
            path = direct if direct.exists() else OMNI_IMAGES / path
        paths.append(path)
    return paths


def _run_cli(args, path: Path) -> str:
    env = os.environ.copy()
    env["NO_PROXY"] = "127.0.0.1,localhost"
    env["no_proxy"] = "127.0.0.1,localhost"
    command = [
        str(args.uparser_bin),
        "parse",
        "--protocol",
        "pipeline",
        "--format",
        "markdown",
        "--endpoint",
        args.endpoint,
        "--pipeline-language",
        args.language,
        "--max-concurrency",
        str(args.cli_max_concurrency),
        "--no-cache",
        "--no-assets",
        str(path),
    ]
    completed = subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=args.timeout,
        check=False,
    )
    if completed.returncode != 0:
        diagnostic = completed.stderr.strip() or completed.stdout.strip()
        raise RuntimeError(f"uparser exited {completed.returncode}: {diagnostic}")
    return completed.stdout


def run_omni(args) -> dict:
    paths = _omni_paths(args.dataset, args.limit)
    args.output.mkdir(parents=True, exist_ok=True)
    pending = [path for path in paths if not (args.output / f"{path.stem}.md").is_file()]

    if args.runner == "cli":
        work_items = pending

        def process(path: Path):
            started = time.perf_counter()
            markdown = _run_cli(args, path)
            (args.output / f"{path.stem}.md").write_text(markdown, encoding="utf-8")
            return path.name, time.perf_counter() - started
    else:
        work_items = [
            pending[offset : offset + args.batch_size]
            for offset in range(0, len(pending), args.batch_size)
        ]

        def process(paths: list[Path]):
            started = time.perf_counter()
            with httpx.Client(timeout=args.timeout, trust_env=False) as client:
                results = _analyze_pages(
                    client, args.endpoint, [_file_page(path) for path in paths], args.language
                )
            for path, result in zip(paths, results):
                (args.output / f"{path.stem}.md").write_text(
                    render_markdown(result), encoding="utf-8"
                )
            return [path.name for path in paths], time.perf_counter() - started

    summary = _run_parallel(work_items, process, args.workers)
    summary.update({"count": len(paths), "skipped_existing": len(paths) - len(pending)})
    return summary


def _pdf_pages(path: Path, dpi: int):
    import pypdfium2 as pdfium

    document = pdfium.PdfDocument(str(path))
    scale = dpi / 72
    try:
        for index in range(len(document)):
            bitmap = document[index].render(scale=scale)
            yield index, bitmap.to_pil()
    finally:
        document.close()


def run_odl(args) -> dict:
    paths = sorted(args.input.glob("*.pdf"))
    if args.limit:
        paths = paths[: args.limit]
    markdown_dir = args.output / "markdown"
    markdown_dir.mkdir(parents=True, exist_ok=True)
    pending = [path for path in paths if not (markdown_dir / f"{path.stem}.md").is_file()]

    def process(path: Path):
        started = time.perf_counter()
        if args.runner == "cli":
            markdown = _run_cli(args, path)
        else:
            with httpx.Client(timeout=args.timeout, trust_env=False) as client:
                markdown = _analyze_document(client, args.endpoint, path, args.language)
        (markdown_dir / f"{path.stem}.md").write_text(markdown, encoding="utf-8")
        return path.name, time.perf_counter() - started

    summary = _run_parallel(pending, process, args.workers)
    summary.update({"count": len(paths), "skipped_existing": len(paths) - len(pending)})
    return summary


def _run_parallel(paths, process, workers: int) -> dict:
    started = time.perf_counter()
    timings = []
    success = 0
    failures = []
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(process, path): path for path in paths}
        for completed, future in enumerate(as_completed(futures), start=1):
            path = futures[future]
            try:
                names, elapsed = future.result()
                item_count = len(names) if isinstance(names, list) else 1
                success += item_count
                timings.extend([elapsed / item_count] * item_count)
            except Exception as exc:
                failures.append({"path": str(path), "error": str(exc)})
            if completed == 1 or completed % 25 == 0 or completed == len(paths):
                print(f"completed={completed}/{len(paths)} failures={len(failures)}", flush=True)
    wall = time.perf_counter() - started
    return {
        "count": len(paths),
        "success": success,
        "failures": failures,
        "wall_seconds": wall,
        "mean_item_seconds": statistics.mean(timings) if timings else None,
        "median_item_seconds": statistics.median(timings) if timings else None,
    }


def merge_resume_summary(previous: dict | None, current: dict) -> dict:
    skipped = current.get("skipped_existing", 0)
    if not skipped:
        return current

    resumed = current.copy()
    new_success = current["success"]
    resumed["success"] = skipped + new_success
    if not previous or previous.get("success") != skipped:
        resumed["prior_completed_without_matching_summary"] = skipped
        resumed["timing_scope"] = "current_resume_only"
        return resumed
    if not new_success:
        for key in ("wall_seconds", "mean_item_seconds", "median_item_seconds"):
            resumed[key] = previous.get(key)
        resumed["runs"] = previous.get("runs", 1)
        return resumed
    resumed["wall_seconds"] = previous["wall_seconds"] + current["wall_seconds"]
    if new_success and previous.get("mean_item_seconds") is not None:
        resumed["mean_item_seconds"] = (
            skipped * previous["mean_item_seconds"]
            + new_success * current["mean_item_seconds"]
        ) / resumed["success"]
        resumed["median_item_seconds"] = previous.get("median_item_seconds")
        resumed["median_resume_note"] = (
            "Median is retained from the previous run because summaries cannot be "
            "combined into an exact cumulative median."
        )
    resumed["runs"] = previous.get("runs", 1) + (1 if new_success else 0)
    return resumed


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=("omnidoc", "opendataloader"))
    parser.add_argument("--runner", choices=("http", "cli"), default="http")
    parser.add_argument("--endpoint", default="http://127.0.0.1:19001")
    parser.add_argument(
        "--uparser-bin",
        type=Path,
        default=ROOT / "uparser" / "target" / "release" / "uparser",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--input", type=Path, default=ODL_ROOT / "pdfs")
    parser.add_argument("--dataset", type=Path, default=OMNI_DATA)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--dpi", type=int, default=200)
    parser.add_argument("--language", default="ch")
    parser.add_argument("--cli-max-concurrency", type=int, default=1)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    summary_path = args.output / "summary.json"
    previous = None
    if summary_path.is_file():
        previous = json.loads(summary_path.read_text(encoding="utf-8"))
    summary = run_omni(args) if args.target == "omnidoc" else run_odl(args)
    summary = merge_resume_summary(previous, summary)
    summary.update(
        {
            "target": args.target,
            "endpoint": args.endpoint,
            "runner": args.runner,
            "uparser_bin": str(args.uparser_bin) if args.runner == "cli" else None,
            "workers": args.workers,
            "device": os.getenv("UPARSER_PIPELINE_DEVICE", "server-managed"),
            "engine_name": "uparser-pipeline-v2",
            "engine_version": "0.1.0",
            "processor": (
                "uparser CLI -> Rust PipelineV2Adapter -> staged model service"
                if args.runner == "cli"
                else "Pipeline V2 HTTP service"
            ),
            "document_count": summary["count"],
            "total_elapsed": summary["wall_seconds"],
            "elapsed_per_doc": (
                summary["wall_seconds"] / summary["success"] if summary["success"] else None
            ),
            "date": datetime.now(timezone.utc).date().isoformat(),
        }
    )
    summary_path.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False))
    return 1 if summary["failures"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
