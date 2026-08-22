#!/usr/bin/env python3
"""Run reproducible local UParser competitor benchmarks.

The harness deliberately separates process-level performance/reliability from
semantic quality. Quality remains INSUFFICIENT until an external evaluator is
attached; non-empty output is never treated as a quality pass.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "benchmark" / "results"
BASELINES = ROOT / "benchmark" / "baselines"
QUALITY_METRICS = ("overall", "nid", "teds", "mhs")


@dataclass(frozen=True)
class Engine:
    name: str
    repo: Path
    binary: Path
    suites: tuple[str, ...]
    build: tuple[str, ...]
    license: str
    default_configuration: str

    def command(self, source: Path, output: Path) -> list[str]:
        if self.name == "uparser-native":
            command = [
                str(self.binary),
                "parse",
                str(source),
                "--mode",
                "native",
                "--format",
                "markdown",
                "--no-assets",
                "--no-cache",
            ]
            # Match the competitor's output channel within each suite. The PDF
            # comparator writes stdout, while Anydoc writes the requested file.
            if source.suffix.lower() != ".pdf":
                command.extend(["--output", str(output)])
            return command
        if self.name == "liteparse-text":
            return [
                str(self.binary),
                "parse",
                str(source),
                "--format",
                "markdown",
                "--no-ocr",
                "--image-mode",
                "off",
                "--quiet",
                "--output",
                str(output),
            ]
        if self.name == "pdf-inspector":
            return [str(self.binary), str(source), "--raw"]
        if self.name == "anydoc":
            return [str(self.binary), str(source), "--output", str(output)]
        raise ValueError(f"unsupported engine: {self.name}")


ENGINES = (
    Engine(
        "uparser-native",
        ROOT / "uparser",
        ROOT / "uparser" / "target" / "release" / "uparser.exe",
        ("pdf", "office"),
        ("cargo", "build", "--release", "--features", "native", "-p", "uparser-core", "--bin", "uparser"),
        "UNLICENSED",
        "unified runner in explicit native mode; Markdown with no assets or cache",
    ),
    Engine(
        "liteparse-text",
        ROOT / "opensource" / "liteparse",
        ROOT / "opensource" / "liteparse" / "target" / "release" / "lit.exe",
        ("pdf",),
        ("cargo", "build", "--release", "--no-default-features", "-p", "liteparse", "--bin", "lit"),
        "Apache-2.0",
        "text-only Markdown; OCR and images disabled",
    ),
    Engine(
        "pdf-inspector",
        ROOT / "opensource" / "pdf-inspector",
        ROOT / "opensource" / "pdf-inspector" / "target" / "release" / "pdf2md.exe",
        ("pdf",),
        ("cargo", "build", "--release", "--bin", "pdf2md"),
        "MIT",
        "raw Markdown",
    ),
    Engine(
        "anydoc",
        ROOT / "opensource" / "anydoc",
        ROOT / "opensource" / "anydoc" / "target" / "release" / "examples" / "convert.exe",
        ("office",),
        ("cargo", "build", "--release", "--example", "convert"),
        "MIT",
        "Markdown conversion example",
    ),
)


def sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_value(repo: Path, *args: str) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    value = result.stdout.strip()
    return value if result.returncode == 0 and value else None


def engine_manifest(engine: Engine) -> dict[str, Any]:
    return {
        "name": engine.name,
        "repo": str(engine.repo.relative_to(ROOT)),
        "commit": git_value(engine.repo, "rev-parse", "HEAD"),
        "describe": git_value(engine.repo, "describe", "--tags", "--always", "--dirty"),
        "binary": str(engine.binary.relative_to(ROOT)),
        "binary_sha256": sha256(engine.binary),
        "build": list(engine.build),
        "license": engine.license,
        "default_configuration": engine.default_configuration,
        "suites": list(engine.suites),
    }


def write_lock(path: Path) -> dict[str, Any]:
    manifest = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor() or os.environ.get("PROCESSOR_IDENTIFIER"),
            "python": sys.version.split()[0],
        },
        "methodology": {
            "resource_class": "R0-native-text",
            "cache": "engine result caches disabled where exposed",
            "output": "markdown, no extracted assets",
            "performance": "CLI end-to-end; no startup subtraction and no best-of-N",
        },
        "engines": [engine_manifest(engine) for engine in ENGINES],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return manifest


def build_engines(selected: list[Engine]) -> None:
    for engine in selected:
        print(f"building {engine.name}: {' '.join(engine.build)}", flush=True)
        subprocess.run(engine.build, cwd=engine.repo, check=True)


def percentile(samples: list[float], fraction: float) -> float:
    if not samples:
        return 0.0
    ordered = sorted(samples)
    rank = round((len(ordered) - 1) * fraction)
    return ordered[max(0, min(rank, len(ordered) - 1))]


def bootstrap_mean_ci(
    values: list[float], samples: int, seed: int
) -> tuple[float, float] | None:
    if not values or samples < 1:
        return None
    rng = random.Random(seed)
    count = len(values)
    estimates = [
        sum(values[rng.randrange(count)] for _ in range(count)) / count
        for _ in range(samples)
    ]
    return percentile(estimates, 0.025), percentile(estimates, 0.975)


def coefficient_of_variation(values: list[float]) -> float | None:
    if len(values) < 2:
        return None
    mean = statistics.mean(values)
    return statistics.stdev(values) / mean if mean else None


def latin_order(items: list[Engine], round_index: int, seed: int) -> list[Engine]:
    ordered = list(items)
    random.Random(seed).shuffle(ordered)
    shift = round_index % len(ordered)
    return ordered[shift:] + ordered[:shift]


def _monitor_peak_rss(process: subprocess.Popen[bytes], result: list[int | None]) -> None:
    try:
        import psutil  # type: ignore

        target = psutil.Process(process.pid)
        peak = 0
        while process.poll() is None:
            try:
                current = target.memory_info().rss
                for child in target.children(recursive=True):
                    current += child.memory_info().rss
                peak = max(peak, current)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                break
            time.sleep(0.005)
        result[0] = peak or None
    except ImportError:
        if os.name != "nt":
            result[0] = None
            return
        peak = 0
        while process.poll() is None:
            peak = max(peak, _windows_process_rss(process.pid) or 0)
            time.sleep(0.005)
        result[0] = peak or None


def _windows_process_rss(pid: int) -> int | None:
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes

    class ProcessMemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    process_query_limited_information = 0x1000
    process_vm_read = 0x0010
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    psapi.GetProcessMemoryInfo.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(ProcessMemoryCounters),
        wintypes.DWORD,
    ]
    psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
    handle = kernel32.OpenProcess(
        process_query_limited_information | process_vm_read, False, pid
    )
    if not handle:
        return None
    try:
        counters = ProcessMemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        ok = psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb)
        return int(counters.WorkingSetSize) if ok else None
    finally:
        kernel32.CloseHandle(handle)


def run_once(engine: Engine, source: Path, output: Path, timeout: float) -> dict[str, Any]:
    output.parent.mkdir(parents=True, exist_ok=True)
    output.unlink(missing_ok=True)
    command = engine.command(source, output)
    started = time.perf_counter()
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    peak: list[int | None] = [None]
    monitor = threading.Thread(target=_monitor_peak_rss, args=(process, peak), daemon=True)
    monitor.start()
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        process.kill()
        stdout, stderr = process.communicate()
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    monitor.join(timeout=1)
    if engine.name in ("uparser-native", "pdf-inspector") and process.returncode == 0 and stdout:
        output.write_bytes(stdout)
    size = output.stat().st_size if output.is_file() else 0
    success = not timed_out and process.returncode == 0 and size > 0
    return {
        "engine": engine.name,
        "source": str(source.relative_to(ROOT)),
        "command": command,
        "returncode": process.returncode,
        "timed_out": timed_out,
        "success": success,
        "elapsed_ms": round(elapsed_ms, 3),
        "peak_rss_bytes": peak[0],
        "output_bytes": size,
        "output_sha256": sha256(output),
        "stdout_tail": stdout.decode("utf-8", errors="replace")[-500:],
        "stderr_tail": stderr.decode("utf-8", errors="replace")[-500:],
    }


def suite_files(suite: str, limit: int | None) -> list[Path]:
    if suite == "pdf":
        files = sorted((ROOT / "benchmark" / "opendataloader-bench" / "pdfs").glob("*.pdf"))
    elif suite == "office":
        fixture_root = ROOT / "opensource" / "anydoc" / "tests" / "fixtures"
        formats = ("csv", "doc", "docx", "epub", "odp", "ods", "odt", "ppt", "pptx", "rtf", "xls", "xlsx")
        files = [path for name in formats for path in sorted((fixture_root / name).glob("*")) if path.is_file()]
    else:
        raise ValueError(f"unsupported suite: {suite}")
    return files[:limit] if limit is not None else files


def summarize(runs: list[dict[str, Any]]) -> dict[str, Any]:
    grouped: dict[str, list[dict[str, Any]]] = {}
    for run in runs:
        grouped.setdefault(run["engine"], []).append(run)
    summaries: dict[str, Any] = {}
    for engine, rows in grouped.items():
        elapsed = [row["elapsed_ms"] for row in rows]
        rss = [row["peak_rss_bytes"] for row in rows if row["peak_rss_bytes"] is not None]
        total_seconds = sum(elapsed) / 1000.0
        summaries[engine] = {
            "runs": len(rows),
            "successes": sum(row["success"] for row in rows),
            "success_rate": sum(row["success"] for row in rows) / len(rows),
            "timeouts": sum(row["timed_out"] for row in rows),
            "empty_outputs": sum(row["returncode"] == 0 and row["output_bytes"] == 0 for row in rows),
            "elapsed_ms": {
                "median": round(statistics.median(elapsed), 3),
                "p95": round(percentile(elapsed, 0.95), 3),
                "total": round(sum(elapsed), 3),
            },
            "throughput_docs_per_second": round(len(rows) / total_seconds, 4) if total_seconds else 0.0,
            "peak_rss_bytes": max(rss) if rss else None,
        }
    return summaries


def paired_elapsed_ratios(
    runs: list[dict[str, Any]], competitor_name: str
) -> list[float]:
    indexed = {
        (row["engine"], row["source"], row["round"]): row
        for row in runs
        if row["success"]
    }
    ratios = []
    for (engine, source, round_index), candidate in indexed.items():
        if engine != "uparser-native":
            continue
        competitor = indexed.get((competitor_name, source, round_index))
        if competitor is not None and competitor["elapsed_ms"] > 0:
            ratios.append(candidate["elapsed_ms"] / competitor["elapsed_ms"])
    return ratios


def round_cv(runs: list[dict[str, Any]], engine_name: str) -> float | None:
    grouped: dict[int, list[float]] = {}
    for row in runs:
        if row["engine"] == engine_name and row["success"]:
            grouped.setdefault(row["round"], []).append(row["elapsed_ms"])
    medians = [statistics.median(values) for _, values in sorted(grouped.items())]
    return coefficient_of_variation(medians)


def performance_gate(
    candidate: dict[str, Any],
    competitor: dict[str, Any],
    runs: list[dict[str, Any]],
    competitor_name: str,
    bootstrap_samples: int,
    seed: int,
) -> dict[str, Any]:
    ratios = paired_elapsed_ratios(runs, competitor_name)
    elapsed_ci = bootstrap_mean_ci(ratios, bootstrap_samples, seed)
    throughput_ci = bootstrap_mean_ci(
        [1.0 / ratio for ratio in ratios if ratio > 0], bootstrap_samples, seed + 1
    )
    candidate_cv = round_cv(runs, "uparser-native")
    competitor_cv = round_cv(runs, competitor_name)
    checks = {
        "success_rate": candidate["success_rate"] >= competitor["success_rate"] and candidate["success_rate"] == 1.0,
        "median": candidate["elapsed_ms"]["median"] < competitor["elapsed_ms"]["median"],
        "median_10pct": candidate["elapsed_ms"]["median"] <= competitor["elapsed_ms"]["median"] * 0.9,
        "p95": candidate["elapsed_ms"]["p95"] < competitor["elapsed_ms"]["p95"],
        "throughput": candidate["throughput_docs_per_second"] > competitor["throughput_docs_per_second"],
        "rss": None,
        "elapsed_ratio_ci_upper_below_one": elapsed_ci is not None and elapsed_ci[1] < 1.0,
        "throughput_ratio_ci_lower_above_one": throughput_ci is not None and throughput_ci[0] > 1.0,
        "candidate_round_cv_at_most_3pct": candidate_cv is not None and candidate_cv <= 0.03,
        "competitor_round_cv_at_most_3pct": competitor_cv is not None and competitor_cv <= 0.03,
    }
    if candidate["peak_rss_bytes"] is not None and competitor["peak_rss_bytes"] is not None:
        checks["rss"] = candidate["peak_rss_bytes"] < competitor["peak_rss_bytes"]
    required = [
        checks[key]
        for key in (
            "success_rate",
            "median",
            "median_10pct",
            "p95",
            "throughput",
            "elapsed_ratio_ci_upper_below_one",
            "throughput_ratio_ci_lower_above_one",
            "candidate_round_cv_at_most_3pct",
            "competitor_round_cv_at_most_3pct",
        )
    ]
    status = "PASS" if all(required) and checks["rss"] is True else "FAIL"
    if checks["rss"] is None:
        status = "INSUFFICIENT"
    return {
        "status": status,
        "checks": checks,
        "paired_samples": len(ratios),
        "elapsed_ratio_mean": statistics.mean(ratios) if ratios else None,
        "elapsed_ratio_bootstrap_ci95": elapsed_ci,
        "throughput_ratio_bootstrap_ci95": throughput_ci,
        "round_cv": {
            "uparser-native": candidate_cv,
            competitor_name: competitor_cv,
        },
    }


def load_quality_evaluation(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8-sig"))
    if not isinstance(payload.get("documents"), list):
        raise ValueError(f"quality evaluation has no documents array: {path}")
    primary_metrics = payload.get("primary_metrics")
    if primary_metrics is not None:
        if (
            not isinstance(primary_metrics, list)
            or not primary_metrics
            or not all(isinstance(metric, str) and metric for metric in primary_metrics)
            or len(set(primary_metrics)) != len(primary_metrics)
            or "overall" not in primary_metrics
        ):
            raise ValueError(
                f"quality evaluation primary_metrics must be unique names containing overall: {path}"
            )
    return payload


def quality_metric_names(
    candidate: dict[str, Any], competitor: dict[str, Any]
) -> tuple[str, ...]:
    candidate_metrics = candidate.get("primary_metrics")
    competitor_metrics = competitor.get("primary_metrics")
    if candidate_metrics is None and competitor_metrics is None:
        return QUALITY_METRICS
    if candidate_metrics is None or competitor_metrics is None:
        raise ValueError("paired quality evaluations must both declare primary_metrics")
    if candidate_metrics != competitor_metrics:
        raise ValueError("paired quality evaluations declare different primary_metrics")
    if (
        not isinstance(candidate_metrics, list)
        or not candidate_metrics
        or not all(isinstance(metric, str) and metric for metric in candidate_metrics)
        or len(set(candidate_metrics)) != len(candidate_metrics)
        or "overall" not in candidate_metrics
    ):
        raise ValueError("primary_metrics must be unique names containing overall")
    return tuple(candidate_metrics)


def quality_gate(
    candidate: dict[str, Any],
    competitor: dict[str, Any],
    bootstrap_samples: int,
    seed: int,
) -> dict[str, Any]:
    quality_metrics = quality_metric_names(candidate, competitor)
    candidate_docs = {row["document_id"]: row for row in candidate["documents"]}
    competitor_docs = {row["document_id"]: row for row in competitor["documents"]}
    metric_results: dict[str, Any] = {}
    insufficient = False
    for metric_index, metric in enumerate(quality_metrics):
        deltas = []
        for document_id in sorted(candidate_docs.keys() & competitor_docs.keys()):
            candidate_score = candidate_docs[document_id].get("scores", {}).get(metric)
            competitor_score = competitor_docs[document_id].get("scores", {}).get(metric)
            if isinstance(candidate_score, (int, float)) and isinstance(
                competitor_score, (int, float)
            ):
                deltas.append(float(candidate_score) - float(competitor_score))
        ci = bootstrap_mean_ci(
            deltas, bootstrap_samples, seed + 100 + metric_index
        )
        if ci is None:
            insufficient = True
        mean_delta = statistics.mean(deltas) if deltas else None
        metric_results[metric] = {
            "paired_samples": len(deltas),
            "mean_delta": mean_delta,
            "bootstrap_ci95": ci,
            "strictly_better": ci is not None and ci[0] > 0,
        }

    candidate_missing = int(candidate.get("metrics", {}).get("missing_predictions", 0))
    checks = {
        "candidate_has_no_missing_predictions": candidate_missing == 0,
        "all_metric_ci_lower_bounds_above_zero": all(
            result["strictly_better"] for result in metric_results.values()
        ),
        "overall_lead_at_least_1pct": (
            metric_results["overall"]["mean_delta"] is not None
            and metric_results["overall"]["mean_delta"] >= 0.01
        ),
    }
    status = "PASS" if all(checks.values()) else "FAIL"
    if insufficient:
        status = "INSUFFICIENT"
    return {
        "status": status,
        "checks": checks,
        "metrics": metric_results,
        "candidate_missing_predictions": candidate_missing,
        "candidate_document_count": len(candidate_docs),
        "competitor_document_count": len(competitor_docs),
    }


def parse_named_paths(values: list[str]) -> dict[str, Path]:
    result = {}
    for value in values:
        name, separator, raw_path = value.partition("=")
        if not separator or not name or not raw_path:
            raise ValueError(f"expected ENGINE=PATH, got: {value}")
        result[name] = Path(raw_path).resolve()
    return result


def evaluate_suite_gates(
    suite: dict[str, Any],
    quality_evaluations: dict[str, dict[str, Any]],
    bootstrap_samples: int,
) -> None:
    summaries = suite["summaries"]
    runs = suite["runs"]
    seed = int(suite["seed"])
    candidate = summaries.get("uparser-native")
    if candidate is None:
        suite["gates"] = {}
        return
    candidate_quality = quality_evaluations.get("uparser-native")
    gates = {
        name: {
            "performance_and_reliability": performance_gate(
                candidate,
                summary,
                runs,
                name,
                bootstrap_samples,
                seed,
            ),
            "quality": quality_gate(
                candidate_quality,
                quality_evaluations[name],
                bootstrap_samples,
                seed,
            )
            if candidate_quality is not None and name in quality_evaluations
            else {
                "status": "INSUFFICIENT",
                "reason": "paired semantic evaluations were not provided",
            },
        }
        for name, summary in summaries.items()
        if name != "uparser-native"
    }
    for gate in gates.values():
        statuses = [
            gate["performance_and_reliability"]["status"], gate["quality"]["status"]
        ]
        gate["overall"] = (
            "PASS"
            if all(status == "PASS" for status in statuses)
            else "FAIL"
            if any(status == "FAIL" for status in statuses)
            else "INSUFFICIENT"
        )
    suite["gates"] = gates


def manifests_match(reports: list[dict[str, Any]]) -> bool:
    fingerprints = []
    for report in reports:
        engines = report.get("manifest", {}).get("engines", [])
        fingerprints.append(
            sorted((engine.get("name"), engine.get("binary_sha256")) for engine in engines)
        )
    return bool(fingerprints) and all(value == fingerprints[0] for value in fingerprints[1:])


def manifest_fingerprint(manifest: dict[str, Any]) -> list[tuple[str | None, str | None]]:
    return sorted(
        (engine.get("name"), engine.get("binary_sha256"))
        for engine in manifest.get("engines", [])
    )


def overall_status(suites: list[dict[str, Any]]) -> str:
    statuses = [
        gate["overall"]
        for suite in suites
        for gate in suite.get("gates", {}).values()
    ]
    if not statuses:
        return "INSUFFICIENT"
    if all(status == "PASS" for status in statuses):
        return "PASS"
    if any(status == "FAIL" for status in statuses):
        return "FAIL"
    return "INSUFFICIENT"


def markdown_report(report: dict[str, Any]) -> str:
    lines = [
        "# UParser Competitor Benchmark Report",
        "",
        f"> Generated: `{report['generated_at']}`  ",
        f"> Overall gate: **{report['overall_status']}**",
        "",
        "## Gate Summary",
        "",
        "| Suite | Competitor | Performance/reliability | Quality | Overall |",
        "|---|---|---:|---:|---:|",
    ]
    for suite in report["suites"]:
        for competitor, gate in suite.get("gates", {}).items():
            lines.append(
                f"| {suite['suite']} | {competitor} | "
                f"{gate['performance_and_reliability']['status']} | "
                f"{gate['quality']['status']} | {gate['overall']} |"
            )
    lines.extend(
        [
            "",
            "## Methodology",
            "",
            "- CLI end-to-end timing includes process startup and output emission.",
            "- Output channels match the paired competitor: stdout for PDF comparisons and "
            "in-process file output for the Office comparison.",
            "- Runs use randomized Latin rotation; no startup subtraction or best-of-N selection.",
            "",
            "## Measurements",
            "",
        ]
    )
    for suite in report["suites"]:
        lines.extend(
            [
                f"### {suite['suite']}",
                "",
                "| Engine | Success | Median ms | P95 ms | Docs/s | Peak RSS bytes |",
                "|---|---:|---:|---:|---:|---:|",
            ]
        )
        for engine, summary in suite["summaries"].items():
            lines.append(
                f"| {engine} | {summary['success_rate']:.3f} | "
                f"{summary['elapsed_ms']['median']:.3f} | "
                f"{summary['elapsed_ms']['p95']:.3f} | "
                f"{summary['throughput_docs_per_second']:.4f} | "
                f"{summary['peak_rss_bytes'] or 'N/A'} |"
            )
        lines.append("")
    lines.extend(["## Gate Details", ""])
    for suite in report["suites"]:
        for competitor, gate in suite.get("gates", {}).items():
            performance = gate["performance_and_reliability"]
            failed_performance = [
                name for name, passed in performance.get("checks", {}).items() if passed is False
            ]
            lines.append(f"### {suite['suite']} / {competitor}")
            lines.append("")
            lines.append(
                "- Performance failures: "
                + (", ".join(failed_performance) if failed_performance else "none")
            )
            quality = gate["quality"]
            if "metrics" not in quality:
                lines.append(f"- Quality: {quality.get('reason', 'INSUFFICIENT')}")
                lines.append("")
                continue
            lines.extend(
                [
                    "",
                    "| Quality metric | Mean paired delta | 95% bootstrap CI | Strictly better |",
                    "|---|---:|---:|---:|",
                ]
            )
            for metric, result in quality["metrics"].items():
                ci = result["bootstrap_ci95"]
                ci_text = f"[{ci[0]:.6f}, {ci[1]:.6f}]" if ci else "N/A"
                lines.append(
                    f"| {metric} | {result['mean_delta']:.6f} | {ci_text} | "
                    f"{str(result['strictly_better']).lower()} |"
                )
            failed_quality = [
                name for name, passed in quality.get("checks", {}).items() if passed is False
            ]
            lines.append("")
            lines.append(
                "- Quality failures: "
                + (", ".join(failed_quality) if failed_quality else "none")
            )
            lines.append("")
    lines.extend(
        [
            "## Interpretation",
            "",
            "`FAIL` and `INSUFFICIENT` both block a comprehensive-leading claim. "
            "Quality gates use paired per-document deltas and bootstrap confidence intervals; "
            "missing semantic evaluations are never inferred from non-empty output.",
            "",
        ]
    )
    return "\n".join(lines)


def run_suite(
    suite: str,
    engines: list[Engine],
    files: list[Path],
    rounds: int,
    seed: int,
    timeout: float,
    output_root: Path,
    quality_evaluations: dict[str, dict[str, Any]],
    bootstrap_samples: int,
    prediction_dir: Path | None,
) -> dict[str, Any]:
    runs: list[dict[str, Any]] = []
    warmups = []
    for engine in engines:
        target = output_root / suite / "warmup" / engine.name / f"{files[0].stem}.md"
        print(f"[{suite} warmup] {engine.name}: {files[0].name}", flush=True)
        warmups.append(run_once(engine, files[0], target, timeout))
    for round_index in range(rounds):
        order = latin_order(engines, round_index, seed)
        for source in files:
            for engine in order:
                target = output_root / suite / f"round-{round_index + 1}" / engine.name / f"{source.stem}.md"
                print(f"[{suite} r{round_index + 1}] {engine.name}: {source.name}", flush=True)
                row = run_once(engine, source, target, timeout)
                row["round"] = round_index + 1
                runs.append(row)
    if prediction_dir is not None:
        prediction_dir.mkdir(parents=True, exist_ok=True)
        for source in files:
            generated = (
                output_root
                / suite
                / "round-1"
                / "uparser-native"
                / f"{source.stem}.md"
            )
            if generated.is_file():
                shutil.copyfile(generated, prediction_dir / f"{source.stem}.md")
    summaries = summarize(runs)
    suite = {
        "suite": suite,
        "files": [str(path.relative_to(ROOT)) for path in files],
        "rounds": rounds,
        "seed": seed,
        "warmups": warmups,
        "summaries": summaries,
        "gates": {},
        "runs": runs,
    }
    evaluate_suite_gates(suite, quality_evaluations, bootstrap_samples)
    return suite


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", choices=("pdf", "office", "all"), default="all")
    parser.add_argument("--rounds", type=int, default=7)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--seed", type=int, default=20260821)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--build", action="store_true")
    parser.add_argument(
        "--engine",
        action="append",
        choices=tuple(engine.name for engine in ENGINES),
        help="limit execution to one or more engines",
    )
    parser.add_argument("--lock-only", action="store_true")
    parser.add_argument("--keep-outputs", action="store_true")
    parser.add_argument(
        "--quality-evaluation",
        action="append",
        default=[],
        metavar="ENGINE=PATH",
        help="attach an evaluator JSON for uparser-native or a competitor",
    )
    parser.add_argument("--bootstrap-samples", type=int, default=10_000)
    parser.add_argument(
        "--performance-input",
        action="append",
        default=[],
        type=Path,
        help="reuse suites from an existing harness JSON instead of rerunning",
    )
    parser.add_argument("--lock", type=Path, default=BASELINES / "competitor_lock.json")
    parser.add_argument("--output", type=Path, default=RESULTS / "gate_GC.json")
    parser.add_argument(
        "--markdown-report", type=Path, default=ROOT / "COMPETITOR_BENCHMARK_REPORT.md"
    )
    parser.add_argument(
        "--prediction-dir",
        type=Path,
        help="export first-round uparser-native Markdown for an external evaluator",
    )
    args = parser.parse_args()

    suites = ("pdf", "office") if args.suite == "all" else (args.suite,)
    selected = [engine for engine in ENGINES if any(suite in engine.suites for suite in suites)]
    if args.engine:
        selected = [engine for engine in selected if engine.name in args.engine]
    input_reports = []
    try:
        input_reports = [
            json.loads(path.resolve().read_text(encoding="utf-8-sig"))
            for path in args.performance_input
        ]
    except (OSError, json.JSONDecodeError) as error:
        parser.error(str(error))
    if input_reports and not manifests_match(input_reports):
        parser.error("performance inputs were produced by different binary sets")
    if args.build:
        if input_reports:
            parser.error("--build cannot be combined with --performance-input")
        build_engines(selected)
    manifest = write_lock(args.lock.resolve())
    if input_reports and manifest_fingerprint(manifest) != manifest_fingerprint(
        input_reports[0]["manifest"]
    ):
        parser.error("current binaries do not match the reused performance inputs")
    if args.lock_only:
        print(args.lock.resolve())
        return 0

    missing = [str(engine.binary) for engine in selected if not engine.binary.is_file()]
    if missing:
        parser.error("missing binaries; rerun with --build:\n" + "\n".join(missing))
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    if args.bootstrap_samples < 1:
        parser.error("--bootstrap-samples must be positive")
    try:
        quality_paths = parse_named_paths(args.quality_evaluation)
        quality_evaluations = {
            name: load_quality_evaluation(path) for name, path in quality_paths.items()
        }
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.error(str(error))

    temporary: tempfile.TemporaryDirectory[str] | None = None
    if input_reports:
        output_root = RESULTS / "competitor_outputs"
    elif args.keep_outputs:
        output_root = RESULTS / "competitor_outputs"
        output_root.mkdir(parents=True, exist_ok=True)
    else:
        temporary = tempfile.TemporaryDirectory(prefix="uparser-competitors-")
        output_root = Path(temporary.name)

    results = [suite for report in input_reports for suite in report["suites"]]
    if input_reports:
        for suite in results:
            evaluate_suite_gates(suite, quality_evaluations, args.bootstrap_samples)
    else:
        try:
            for suite in suites:
                engines = [engine for engine in selected if suite in engine.suites]
                files = suite_files(suite, args.limit)
                if not files:
                    parser.error(f"no files found for suite {suite}")
                results.append(
                    run_suite(
                        suite,
                        engines,
                        files,
                        args.rounds,
                        args.seed,
                        args.timeout,
                        output_root,
                        quality_evaluations,
                        args.bootstrap_samples,
                        args.prediction_dir.resolve() if args.prediction_dir else None,
                    )
                )
        finally:
            if temporary is not None:
                temporary.cleanup()

    report = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "competitor_lock_sha256": sha256(args.lock.resolve()),
        "manifest": manifest,
        "methodology": {
            "scope": "CLI end-to-end R0 smoke/full baseline",
            "quality_policy": "INSUFFICIENT until semantic evaluators are attached",
            "rounds": args.rounds,
            "seed": args.seed,
            "timeout_seconds": args.timeout,
            "bootstrap_samples": args.bootstrap_samples,
            "quality_evaluations": {
                name: {"path": str(path), "sha256": sha256(path)}
                for name, path in quality_paths.items()
            },
        },
        "suites": results,
    }
    report["overall_status"] = overall_status(results)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    args.markdown_report.resolve().write_text(markdown_report(report), encoding="utf-8")
    print(args.output.resolve())
    print(args.markdown_report.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
