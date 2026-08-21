#!/usr/bin/env python3
"""Evaluate V2 preflight format coverage, routing policy, and CLI overhead."""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import subprocess
import tempfile
import time
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BIN = ROOT / "uparser" / "target" / "release" / "uparser"
PDF_CORPUS = ROOT / "opensource" / "opendataloader-bench" / "pdfs"


FIXTURES = {
    "pdf": ("opensource/opendataloader-bench/pdfs/01030000000001.pdf", "native"),
    "doc": ("opensource/anydoc/tests/fixtures/doc/text.doc", "native"),
    "docx": ("opensource/anydoc/tests/fixtures/docx/text.docx", "native"),
    "ppt": ("opensource/anydoc/tests/fixtures/ppt/pres.ppt", "mineru-vlm"),
    "pptx": ("opensource/anydoc/tests/fixtures/pptx/pres.pptx", "mineru-vlm"),
    "excel": ("opensource/anydoc/tests/fixtures/xls/sheet.xls", "native"),
    "excel_xlsx": ("opensource/anydoc/tests/fixtures/xlsx/sheet.xlsx", "native"),
    "odt": ("opensource/anydoc/tests/fixtures/odt/text.odt", "native"),
    "ods": ("opensource/anydoc/tests/fixtures/ods/sheet.ods", "native"),
    "odp": ("opensource/anydoc/tests/fixtures/odp/pres.odp", "mineru-vlm"),
    "rtf": ("opensource/anydoc/tests/fixtures/rtf/text.rtf", "native"),
    "epub": ("opensource/anydoc/tests/fixtures/epub/book.epub", "native"),
    "csv": ("uparser/crates/uparser-document-engine/tests/fixtures/sample.csv", "native"),
    "png": ("opensource/MonkeyOCRv2/images_test/table.png", "mineru-vlm"),
    "jpeg": ("opensource/MonkeyOCRv2/images_test/es.jpeg", "mineru-vlm"),
}


def percentile(samples: list[float], fraction: float) -> float:
    ordered = sorted(samples)
    if not ordered:
        return 0.0
    return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * fraction))]


def run_plan(binary: Path, path: Path) -> tuple[int, dict | None, str, float]:
    started = time.perf_counter()
    result = subprocess.run(
        [str(binary), "plan", "--mode", "auto", str(path)],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000
    try:
        payload = json.loads(result.stdout) if result.stdout.strip() else None
    except json.JSONDecodeError:
        payload = None
    diagnostic = (result.stderr or result.stdout)[-2000:]
    return result.returncode, payload, diagnostic, elapsed_ms


def evaluate_formats(binary: Path, repeats: int, temporary: Path) -> dict:
    tsv = temporary / "sample.tsv"
    tsv.write_text("name\tvalue\nalpha\t1\nbeta\t2\n", encoding="utf-8")
    unknown = temporary / "unknown.bin"
    unknown.write_bytes(b"uparser-v2-unknown-format-fixture")
    cases = dict(FIXTURES)
    cases["tsv"] = (str(tsv), "native")
    cases["unknown"] = (str(unknown), None)

    rows = []
    for case, (raw_path, expected_route) in cases.items():
        path = Path(raw_path)
        if not path.is_absolute():
            path = ROOT / path
        samples = [run_plan(binary, path) for _ in range(repeats)]
        code, payload, diagnostic, _ = samples[0]
        expected_format = "excel" if case == "excel_xlsx" else case
        if case == "unknown":
            format_ok = all(sample[0] != 0 for sample in samples)
            actual_format = "rejected"
            actual_route = None
            route_ok = True
        else:
            actual_format = payload["format"]["format"] if payload else None
            actual_route = payload["plan"]["route"]["protocol"] if payload else None
            format_ok = all(
                sample[0] == 0
                and sample[1]
                and sample[1]["format"]["format"] == expected_format
                for sample in samples
            )
            route_ok = all(
                sample[1]
                and sample[1]["plan"]["route"]["protocol"] == expected_route
                for sample in samples
            )
        latencies = [sample[3] for sample in samples]
        rows.append(
            {
                "case": case,
                "path": str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path),
                "expected_format": expected_format,
                "actual_format": actual_format,
                "format_ok": format_ok,
                "expected_policy_route": expected_route,
                "actual_route": actual_route,
                "policy_route_ok": route_ok,
                "returncode": code,
                "diagnostic": diagnostic if code else "",
                "latency_ms_median": statistics.median(latencies),
                "latency_ms_p95": percentile(latencies, 0.95),
            }
        )
    return {
        "case_count": len(rows),
        "recognized_case_count": sum(row["case"] != "unknown" for row in rows),
        "format_contract_conformance": sum(row["format_ok"] for row in rows) / len(rows),
        "policy_route_conformance": sum(row["policy_route_ok"] for row in rows) / len(rows),
        "cases": rows,
    }


def evaluate_pdf_preflight(binary: Path, limit: int) -> dict:
    paths = sorted(PDF_CORPUS.glob("*.pdf"))[:limit]
    latencies = []
    routes: Counter[str] = Counter()
    genres: Counter[str] = Counter()
    failures = []
    for path in paths:
        code, payload, diagnostic, elapsed_ms = run_plan(binary, path)
        latencies.append(elapsed_ms)
        if code == 0 and payload:
            routes[payload["plan"]["route"]["protocol"]] += 1
            genres[payload["profile"]["genre"]["primary"]] += 1
        else:
            failures.append({"document": path.name, "returncode": code, "diagnostic": diagnostic})
    total_seconds = sum(latencies) / 1000
    return {
        "documents": len(paths),
        "failures": failures,
        "throughput_docs_per_second": len(paths) / total_seconds if total_seconds else 0.0,
        "latency_ms": {
            "mean": statistics.mean(latencies) if latencies else 0.0,
            "median": statistics.median(latencies) if latencies else 0.0,
            "p95": percentile(latencies, 0.95),
            "max": max(latencies, default=0.0),
        },
        "route_distribution": dict(routes),
        "genre_distribution": dict(genres),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--uparser-bin", type=Path, default=DEFAULT_BIN)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--pdf-limit", type=int, default=200)
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "benchmark" / "results" / "architecture_v2_20260821.json",
    )
    args = parser.parse_args()
    binary = args.uparser_bin.resolve()
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="uparser-v2-eval-") as raw_temp:
        result = {
            "schema_version": 1,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "binary": str(binary),
            "binary_sha256": binary_hash,
            "methodology": {
                "format_repeats": args.repeats,
                "pdf_preflight_limit": args.pdf_limit,
                "route_metric": "fixed V2 policy conformance; not a substitute for labeled G-R regret",
            },
            "format_and_policy": evaluate_formats(binary, args.repeats, Path(raw_temp)),
            "pdf_preflight": evaluate_pdf_preflight(binary, args.pdf_limit),
        }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False), encoding="utf-8")
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
