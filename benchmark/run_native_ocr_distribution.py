#!/usr/bin/env python3
"""Compare local no-OCR, hybrid-OCR, and page-OCR behavior by corpus bucket."""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BIN = ROOT / "uparser" / "target" / "release" / "uparser"
ODL = ROOT / "opensource" / "opendataloader-bench"
OMNI = ROOT / "benchmark" / "OmniDocBench"
OMNI_DATA = ROOT / "benchmark" / "OmniDocBenchData"
RESULTS = ROOT / "benchmark" / "results"
TESSERACT_ROOT = Path("/tmp/uparser-tesseract-runtime")

ODL_NO_OCR = "uparser-native-no-ocr-v2-20260824"
ODL_HYBRID = "uparser-native-hybrid-ocr-v2-20260824"
OMNI_NO_OCR = "uparser-native-no-ocr-v2-20260824"
OMNI_OCR = "uparser-tesseract-local-v2-20260824"


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = round((len(ordered) - 1) * fraction)
    return ordered[index]


def runtime_env(with_ocr: bool, binary: Path, language: str | None = None) -> dict[str, str]:
    env = os.environ.copy()
    deps = binary.parent / "deps"
    library_paths = [str(deps)]
    if with_ocr:
        tess_bin = TESSERACT_ROOT / "usr" / "bin"
        tess_lib = TESSERACT_ROOT / "usr" / "lib" / "x86_64-linux-gnu"
        env["PATH"] = f"{tess_bin}:{env.get('PATH', '')}"
        library_paths.insert(0, str(tess_lib))
        env["TESSDATA_PREFIX"] = str(tess_bin / "tessdata")
        if language:
            env["UPARSER_OCR_LANG"] = language
    else:
        env["PATH"] = "/usr/bin:/bin"
        env.pop("TESSDATA_PREFIX", None)
        env.pop("UPARSER_OCR_LANG", None)
    existing = env.get("LD_LIBRARY_PATH")
    if existing:
        library_paths.append(existing)
    env["LD_LIBRARY_PATH"] = ":".join(library_paths)
    return env


def run_parse(
    binary: Path,
    source: Path,
    protocol: str,
    env: dict[str, str],
    output: Path,
    timeout: int,
) -> dict[str, Any]:
    output.parent.mkdir(parents=True, exist_ok=True)
    command = [
        str(binary),
        "parse",
        "--protocol",
        protocol,
        "--format",
        "markdown",
        "--no-assets",
        "--no-cache",
        str(source),
    ]
    started = time.perf_counter()
    try:
        result = subprocess.run(
            command,
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        elapsed = time.perf_counter() - started
        markdown = result.stdout if result.returncode == 0 else ""
        output.write_text(markdown, encoding="utf-8")
        return {
            "source": str(source),
            "returncode": result.returncode,
            "elapsed_seconds": elapsed,
            "markdown_bytes": len(markdown.encode()),
            "markdown_sha256": hashlib.sha256(markdown.encode()).hexdigest(),
            "stderr_tail": result.stderr[-2000:],
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as error:
        elapsed = time.perf_counter() - started
        output.write_text("", encoding="utf-8")
        return {
            "source": str(source),
            "returncode": 124,
            "elapsed_seconds": elapsed,
            "markdown_bytes": 0,
            "markdown_sha256": hashlib.sha256(b"").hexdigest(),
            "stderr_tail": f"timed out after {error.timeout} seconds",
            "timed_out": True,
        }


def summarize_rows(rows: list[dict[str, Any]]) -> dict[str, Any]:
    elapsed = [float(row["elapsed_seconds"]) for row in rows]
    return {
        "samples": len(rows),
        "successes": sum(row["returncode"] == 0 for row in rows),
        "failures": sum(row["returncode"] != 0 for row in rows),
        "timeouts": sum(bool(row["timed_out"]) for row in rows),
        "nonempty_outputs": sum(int(row["markdown_bytes"]) > 0 for row in rows),
        "returncodes": dict(Counter(str(row["returncode"]) for row in rows)),
        "elapsed_seconds": {
            "total": sum(elapsed),
            "mean": statistics.mean(elapsed) if elapsed else None,
            "median": statistics.median(elapsed) if elapsed else None,
            "p95": percentile(elapsed, 0.95),
            "max": max(elapsed, default=None),
        },
        "markdown_bytes": {
            "total": sum(int(row["markdown_bytes"]) for row in rows),
            "median": statistics.median(int(row["markdown_bytes"]) for row in rows)
            if rows
            else None,
        },
    }


def run_official_odl(engine: str) -> dict[str, Any]:
    python = ODL / ".venv" / "bin" / "python"
    subprocess.run(
        [
            str(python),
            "src/evaluator.py",
            "--engine",
            engine,
            "--log-level",
            "INFO",
        ],
        cwd=ODL,
        check=True,
    )
    return json.loads((ODL / "prediction" / engine / "evaluation.json").read_text())


def write_odl_summary(engine: str, rows: list[dict[str, Any]]) -> None:
    summary = summarize_rows(rows)
    payload = {
        "engine_name": engine,
        "engine_version": "0.4.0-rc.1",
        "document_count": len(rows),
        "total_elapsed": summary["elapsed_seconds"]["total"],
        "elapsed_per_doc": summary["elapsed_seconds"]["mean"],
        "date": datetime.now(timezone.utc).date().isoformat(),
    }
    path = ODL / "prediction" / engine / "summary.json"
    path.write_text(json.dumps(payload, indent=2), encoding="utf-8")


def run_opendataloader(args: argparse.Namespace) -> dict[str, Any]:
    paths = sorted((ODL / "pdfs").glob("*.pdf"))
    if args.limit:
        paths = paths[: args.limit]
    runs: dict[str, list[dict[str, Any]]] = {}
    for engine, with_ocr in [(ODL_NO_OCR, False), (ODL_HYBRID, True)]:
        out_dir = ODL / "prediction" / engine / "markdown"
        rows = []
        for index, source in enumerate(paths, start=1):
            row = run_parse(
                args.uparser_bin,
                source,
                "native",
                runtime_env(with_ocr, args.uparser_bin),
                out_dir / f"{source.stem}.md",
                args.timeout,
            )
            row["document_id"] = source.stem
            rows.append(row)
            if index == 1 or index % 25 == 0 or index == len(paths):
                print(f"{engine}: {index}/{len(paths)}, failures={sum(r['returncode'] != 0 for r in rows)}", flush=True)
        write_odl_summary(engine, rows)
        runs[engine] = rows

    no_by_id = {row["document_id"]: row for row in runs[ODL_NO_OCR]}
    hybrid_by_id = {row["document_id"]: row for row in runs[ODL_HYBRID]}
    changed = []
    for document_id in sorted(no_by_id):
        before = no_by_id[document_id]
        after = hybrid_by_id[document_id]
        if before["returncode"] != after["returncode"] or before["markdown_sha256"] != after["markdown_sha256"]:
            changed.append(
                {
                    "document_id": document_id,
                    "no_ocr_returncode": before["returncode"],
                    "hybrid_returncode": after["returncode"],
                    "no_ocr_bytes": before["markdown_bytes"],
                    "hybrid_bytes": after["markdown_bytes"],
                }
            )

    quality = {}
    if not args.skip_eval and not args.limit:
        quality[ODL_NO_OCR] = run_official_odl(ODL_NO_OCR)
        quality[ODL_HYBRID] = run_official_odl(ODL_HYBRID)
    return {
        "corpus": "opendataloader-bench",
        "samples": len(paths),
        "runs": {name: {"summary": summarize_rows(rows), "rows": rows} for name, rows in runs.items()},
        "changed_outputs": changed,
        "quality": quality,
    }


def omni_language(attribute: str) -> str:
    return {
        "english": "eng",
        "simplified_chinese": "chi_sim+eng",
        "traditional_chinese": "chi_tra+eng",
        "en_ch_mixed": "chi_sim+chi_tra+eng",
        "other": "eng",
    }.get(attribute, "eng")


def bucket_summary(rows: list[dict[str, Any]], key: str) -> dict[str, Any]:
    buckets: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        buckets[str(row[key])].append(row)
    return {name: summarize_rows(items) for name, items in sorted(buckets.items())}


def write_omni_config(name: str) -> Path:
    path = OMNI / "configs" / f"omnidoc_{name}.yaml"
    path.write_text(
        f"""end2end_eval:
  metrics:
    text_block:
      metric: [Edit_dist]
    display_formula:
      metric: [Edit_dist]
    table:
      metric: [TEDS, Edit_dist]
      teds_workers: 24
    reading_order:
      metric: [Edit_dist]
  dataset:
    dataset_name: end2end_dataset
    ground_truth:
      data_path: ../OmniDocBenchData/OmniDocBench.json
    prediction:
      data_path: ../omnidoc_pred/{name}
    match_method: quick_match
    match_workers: 24
    quick_match_truncated_timeout_sec: 300
    match_timeout_sec: 420
    timeout_fallback_max_chunk_span: 10
    timeout_fallback_order_penalty: 0.10
""",
        encoding="utf-8",
    )
    return path


def run_official_omni(name: str) -> dict[str, Any]:
    config = write_omni_config(name)
    python = OMNI / ".venv" / "bin" / "python"
    subprocess.run(
        [str(python), "run_eval.py", "--config", str(config.relative_to(OMNI))],
        cwd=OMNI,
        check=True,
    )
    metric = OMNI / "result" / f"{name}_quick_match_metric_result.json"
    return json.loads(metric.read_text())


def run_omni_mode(
    args: argparse.Namespace,
    items: list[dict[str, Any]],
    name: str,
    protocol: str,
    with_ocr: bool,
) -> list[dict[str, Any]]:
    out_dir = ROOT / "benchmark" / "omnidoc_pred" / name
    status_dir = out_dir / ".status"
    status_dir.mkdir(parents=True, exist_ok=True)

    def parse_one(item: dict[str, Any]) -> dict[str, Any]:
        page = item["page_info"]
        attribute = page["page_attribute"]
        source = OMNI_DATA / "images" / page["image_path"]
        language = omni_language(attribute["language"]) if with_ocr else None
        row = run_parse(
            args.uparser_bin,
            source,
            protocol,
            runtime_env(with_ocr, args.uparser_bin, language),
            out_dir / f"{source.stem}.md",
            args.timeout,
        )
        row.update(
            {
                "image": source.name,
                "data_source": attribute["data_source"],
                "language": attribute["language"],
                "layout": attribute["layout"],
                "subset": attribute["subset"],
                "ocr_language": language,
            }
        )
        (status_dir / f"{source.stem}.json").write_text(
            json.dumps(row, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        return row

    rows = []
    with ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = [executor.submit(parse_one, item) for item in items]
        for completed, future in enumerate(as_completed(futures), start=1):
            rows.append(future.result())
            if completed == 1 or completed % 50 == 0 or completed == len(futures):
                print(f"{name}: {completed}/{len(futures)}, failures={sum(r['returncode'] != 0 for r in rows)}", flush=True)
    rows.sort(key=lambda row: row["image"])
    return rows


def load_omni_status(name: str, expected: int) -> list[dict[str, Any]]:
    status_dir = ROOT / "benchmark" / "omnidoc_pred" / name / ".status"
    rows = [json.loads(path.read_text()) for path in sorted(status_dir.glob("*.json"))]
    if len(rows) != expected:
        raise RuntimeError(f"{name}: expected {expected} status files, found {len(rows)}")
    rows.sort(key=lambda row: row["image"])
    return rows


def load_existing_omni_quality(name: str) -> dict[str, Any] | None:
    metric = OMNI / "result" / f"{name}_quick_match_metric_result.json"
    return json.loads(metric.read_text()) if metric.exists() else None


def run_omnidoc(args: argparse.Namespace) -> dict[str, Any]:
    items = json.loads((OMNI_DATA / "OmniDocBench.json").read_text())
    if args.limit:
        items = items[: args.limit]
    if args.reuse_status:
        runs = {
            OMNI_NO_OCR: load_omni_status(OMNI_NO_OCR, len(items)),
            OMNI_OCR: load_omni_status(OMNI_OCR, len(items)),
        }
    else:
        runs = {
            OMNI_NO_OCR: run_omni_mode(args, items, OMNI_NO_OCR, "native", False),
            OMNI_OCR: run_omni_mode(args, items, OMNI_OCR, "tesseract", True),
        }
    quality = {}
    quality_status = {}
    if not args.skip_eval and not args.limit:
        quality[OMNI_NO_OCR] = run_official_omni(OMNI_NO_OCR)
        quality[OMNI_OCR] = run_official_omni(OMNI_OCR)
    elif not args.limit:
        for name in runs:
            existing = load_existing_omni_quality(name)
            if existing is not None:
                quality[name] = existing
                quality_status[name] = "completed official result loaded from disk"
            else:
                quality_status[name] = "official evaluation not completed"
    return {
        "corpus": "OmniDocBench",
        "samples": len(items),
        "runs": {
            name: {
                "summary": summarize_rows(rows),
                "by_data_source": bucket_summary(rows, "data_source"),
                "by_language": bucket_summary(rows, "language"),
                "by_layout": bucket_summary(rows, "layout"),
                "by_subset": bucket_summary(rows, "subset"),
                "rows": rows,
            }
            for name, rows in runs.items()
        },
        "quality": quality,
        "quality_status": quality_status,
        "boundary": (
            "OmniDocBench inputs are page images. The no-OCR native run measures the explicit "
            "format/reachability boundary; the OCR run is the in-process tesseract protocol, not "
            "native PDF hybrid fallback."
        ),
    }


def compact_quality(payload: dict[str, Any]) -> dict[str, Any]:
    if "metrics" in payload:
        return payload.get("metrics", {}).get("score", {})
    result = {}
    for component, metric, key in [
        ("text_block", "Edit_dist", "text_edit"),
        ("display_formula", "Edit_dist", "formula_edit"),
        ("table", "TEDS", "table_teds"),
        ("table", "TEDS_structure_only", "table_teds_structure"),
        ("reading_order", "Edit_dist", "reading_order_edit"),
    ]:
        try:
            group = payload[component]["all"][metric]
            result[key] = group.get("ALL_page_avg", group.get("all"))
        except KeyError:
            result[key] = None
    return result


def markdown_report(result: dict[str, Any]) -> str:
    lines = [
        "# Native OCR distribution benchmark",
        "",
        f"Generated: `{result['generated_at']}`",
        f"Binary: `{result['binary']}` (`{result['binary_sha256']}`)",
        "",
    ]
    for corpus in result["corpora"]:
        lines.extend([f"## {corpus['corpus']}", ""])
        if corpus.get("boundary"):
            lines.extend([corpus["boundary"], ""])
        lines.extend([
            "| Run | Samples | Success | Fail | Non-empty | Median s | P95 s | Max s |",
            "|---|---:|---:|---:|---:|---:|---:|---:|",
        ])
        for name, run in corpus["runs"].items():
            summary = run["summary"]
            elapsed = summary["elapsed_seconds"]
            lines.append(
                f"| `{name}` | {summary['samples']} | {summary['successes']} | "
                f"{summary['failures']} | {summary['nonempty_outputs']} | "
                f"{elapsed['median'] or 0:.4f} | {elapsed['p95'] or 0:.4f} | "
                f"{elapsed['max'] or 0:.4f} |"
            )
        lines.append("")
        for name, run in corpus["runs"].items():
            if "by_data_source" not in run or not name.startswith("uparser-tesseract"):
                continue
            lines.extend([f"### `{name}` timing by data source", ""])
            lines.extend([
                "| Data source | Samples | Median s | P95 s | Max s |",
                "|---|---:|---:|---:|---:|",
            ])
            for bucket, summary in run["by_data_source"].items():
                elapsed = summary["elapsed_seconds"]
                lines.append(
                    f"| {bucket} | {summary['samples']} | {elapsed['median']:.4f} | "
                    f"{elapsed['p95']:.4f} | {elapsed['max']:.4f} |"
                )
            lines.append("")
        if corpus.get("quality"):
            lines.extend(["Official quality summaries:", ""])
            for name, payload in corpus["quality"].items():
                lines.append(f"- `{name}`: `{json.dumps(compact_quality(payload), ensure_ascii=False)}`")
            lines.append("")
        if corpus.get("quality_status"):
            lines.extend(["Official evaluation status:", ""])
            for name, status in corpus["quality_status"].items():
                lines.append(f"- `{name}`: {status}")
            lines.append("")
        if corpus.get("evaluation_attempt"):
            attempt = corpus["evaluation_attempt"]
            lines.append(
                "Official OCR evaluation attempt: "
                f"{attempt['matched_pages']}/{attempt['total_pages']} pages in about "
                f"{attempt['elapsed_seconds_approx'] / 60:.0f} minutes; not completed "
                f"({attempt['reason']})."
            )
            lines.append("")
        if corpus.get("changed_outputs") is not None:
            lines.append(f"Changed outputs between no-OCR and hybrid runs: {len(corpus['changed_outputs'])}")
            for changed in corpus["changed_outputs"]:
                lines.append(
                    f"- `{changed['document_id']}`: {changed['no_ocr_bytes']} -> "
                    f"{changed['hybrid_bytes']} bytes"
                )
            lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", choices=["opendataloader", "omnidoc", "all"])
    parser.add_argument("--uparser-bin", type=Path, default=DEFAULT_BIN)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--skip-eval", action="store_true")
    parser.add_argument(
        "--reuse-status",
        action="store_true",
        help="rebuild OmniDocBench summaries from existing per-page status files",
    )
    parser.add_argument("--output", type=Path, default=RESULTS / "native_ocr_distribution_20260824.json")
    args = parser.parse_args()
    args.uparser_bin = args.uparser_bin.resolve()

    corpora = []
    if args.corpus in {"opendataloader", "all"}:
        corpora.append(run_opendataloader(args))
    if args.corpus in {"omnidoc", "all"}:
        corpora.append(run_omnidoc(args))
    result = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "binary": str(args.uparser_bin),
        "binary_sha256": hashlib.sha256(args.uparser_bin.read_bytes()).hexdigest(),
        "tesseract_runtime": str(TESSERACT_ROOT),
        "corpora": corpora,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    report = args.output.with_suffix(".md")
    report.write_text(markdown_report(result), encoding="utf-8")
    print(f"wrote {args.output}")
    print(f"wrote {report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
