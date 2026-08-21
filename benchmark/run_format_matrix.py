#!/usr/bin/env python3
"""Generate and exercise all 16 uparser V2 format-contract variants."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = ROOT / "benchmark" / "format_matrix" / "fixtures"
DEFAULT_RESULTS = ROOT / "benchmark" / "format_matrix" / "results"


@dataclass(frozen=True)
class Case:
    format: str
    filename: str
    marker: str | None
    quality_route: str | None
    speed_route: str | None
    structured: bool = False
    native: bool = False


CASES = [
    Case("pdf", "01-pdf.pdf", "Format Matrix PDF", "native", "native", native=True),
    Case("doc", "02-doc.doc", "Format Matrix DOC", "native", "native", True, True),
    Case("docx", "03-docx.docx", "Format Matrix DOCX", "native", "native", True, True),
    Case("ppt", "04-ppt.ppt", "Format Matrix PPT", "mineru-vlm", "native", True, True),
    Case("pptx", "05-pptx.pptx", "Format Matrix PPTX", "mineru-vlm", "native", True, True),
    Case("excel", "06-excel.xlsx", "alpha", "native", "native", True, True),
    Case("odt", "07-odt.odt", "Format Matrix ODT", "native", "native", True, True),
    Case("ods", "08-ods.ods", "alpha", "native", "native", True, True),
    Case("odp", "09-odp.odp", "Format Matrix ODP", "mineru-vlm", "native", True, True),
    Case("rtf", "10-rtf.rtf", "Format Matrix RTF", "native", "native", True, True),
    Case("epub", "11-epub.epub", "Format Matrix EPUB", "native", "native", True, True),
    Case("csv", "12-csv.csv", "alpha", "native", "native", True, True),
    Case("tsv", "13-tsv.tsv", "alpha", "native", "native", True, True),
    Case("png", "14-png.png", None, "mineru-vlm", "mineru-vlm"),
    Case("jpeg", "15-jpeg.jpg", None, "mineru-vlm", "mineru-vlm"),
    Case("unknown", "16-unknown.bin", None, None, None),
]


class Matrix:
    def __init__(self, binary: Path, fixtures: Path, results: Path, endpoint: str | None, model: str):
        self.binary = binary
        self.fixtures = fixtures
        self.results = results
        self.endpoint = endpoint
        self.model = model
        self.checks: list[dict[str, Any]] = []
        self.real_results: dict[str, dict[str, Any]] = {}
        self.format_results: dict[str, dict[str, Any]] = {
            case.format: {"format": case.format, "file": case.filename, "checks": []}
            for case in CASES
        }

    def run(self, args: list[str], timeout: int = 30, env: dict[str, str] | None = None) -> dict[str, Any]:
        started = time.perf_counter()
        completed = subprocess.run(
            [str(self.binary), *args],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            env=env,
            check=False,
        )
        return {
            "args": args,
            "returncode": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 3),
        }

    def record(
        self,
        case: Case | None,
        name: str,
        status: str,
        detail: str,
        elapsed_ms: float | None = None,
    ) -> None:
        check = {
            "format": case.format if case else "cross_format",
            "name": name,
            "status": status,
            "detail": detail,
            "elapsed_ms": elapsed_ms,
        }
        self.checks.append(check)
        if case:
            self.format_results[case.format]["checks"].append(check)
        print(f"{status:4} {check['format']:12} {name}: {detail}")

    def expect(self, case: Case | None, name: str, condition: bool, detail: str, elapsed_ms: float | None = None) -> None:
        self.record(case, name, "PASS" if condition else "FAIL", detail, elapsed_ms)

    def skip(self, case: Case, name: str, detail: str) -> None:
        self.record(case, name, "SKIP", detail)

    @staticmethod
    def parse_json(result: dict[str, Any]) -> Any:
        try:
            return json.loads(result["stdout"])
        except json.JSONDecodeError:
            return None

    def test_case(self, case: Case) -> None:
        source = self.fixtures / case.filename
        if case.format == "unknown":
            classified = self.run(["classify", str(source)])
            self.expect(case, "classify_rejects_unknown", classified["returncode"] == 1, f"exit={classified['returncode']}", classified["elapsed_ms"])
            for preference in ("quality", "speed", "cost"):
                planned = self.run(["plan", "--mode", "auto", "--prefer", preference, str(source)])
                payload = self.parse_json(planned)
                good = planned["returncode"] == 1 and payload and payload.get("error", {}).get("code") == "plan_failed"
                self.expect(case, f"plan_{preference}_rejects_unknown", bool(good), f"exit={planned['returncode']}", planned["elapsed_ms"])
            parsed = self.run(["parse", "--mode", "native", "--format", "json", str(source)])
            self.expect(case, "native_rejects_unknown", parsed["returncode"] == 1, f"exit={parsed['returncode']}", parsed["elapsed_ms"])
            return

        classified = self.run(["classify", str(source)])
        profile = self.parse_json(classified)
        detected = profile.get("source_format") if profile else None
        self.expect(case, "classify_format", classified["returncode"] == 0 and detected == case.format, f"detected={detected}", classified["elapsed_ms"])
        self.expect(case, "classify_has_analysis", bool(profile and profile.get("analysis_level") and profile.get("source_quality")), f"level={profile.get('analysis_level') if profile else None}")

        plans: dict[str, dict[str, Any]] = {}
        for preference in ("quality", "speed", "cost"):
            planned = self.run(["plan", "--mode", "auto", "--prefer", preference, str(source)])
            payload = self.parse_json(planned)
            plans[preference] = payload or {}
            route = payload.get("plan", {}).get("route", {}).get("protocol") if payload else None
            expected = case.quality_route if preference == "quality" else case.speed_route
            good = planned["returncode"] == 0 and payload and payload.get("format", {}).get("format") == case.format and route == expected
            self.expect(case, f"plan_{preference}", bool(good), f"route={route}, expected={expected}", planned["elapsed_ms"])
            complete = bool(payload and payload.get("profile") and payload.get("plan", {}).get("preprocess") and payload.get("plan", {}).get("route", {}).get("candidates"))
            self.expect(case, f"plan_{preference}_metadata", complete, "profile+preprocess+candidates")

        if not case.native:
            rejected = self.run(["parse", "--mode", "native", "--format", "json", "--no-assets", str(source)])
            self.expect(case, "native_infeasible", rejected["returncode"] == 1, f"exit={rejected['returncode']}", rejected["elapsed_ms"])
            if self.endpoint:
                parsed = self.run([
                    "parse", "--mode", "protocol", "--protocol", "mineru-vlm",
                    "--endpoint", self.endpoint, "--model", self.model,
                    "--format", "json", "--no-assets", "--no-cache", str(source),
                ], timeout=180)
                self.expect(case, "vlm_parse", parsed["returncode"] in (0, 3), f"exit={parsed['returncode']}", parsed["elapsed_ms"])
            else:
                self.skip(case, "vlm_parse", "no reachable/configured VLM endpoint")
            return

        native_json = self.run(["parse", "--mode", "native", "--format", "json", "--no-assets", "--no-cache", str(source)])
        parsed = self.parse_json(native_json)
        route = parsed.get("route_decision", {}).get("protocol") if parsed else None
        output_format = parsed.get("document_profile", {}).get("source_format") if parsed else None
        good = native_json["returncode"] == 0 and route == "native" and output_format == case.format and parsed.get("pages") and not parsed.get("page_errors")
        self.expect(case, "native_json", bool(good), f"exit={native_json['returncode']}, route={route}, pages={len(parsed.get('pages', [])) if parsed else 0}", native_json["elapsed_ms"])

        markdown = self.run(["parse", "--mode", "native", "--format", "markdown", "--no-assets", "--no-cache", str(source)])
        self.expect(case, "native_markdown", markdown["returncode"] == 0 and bool(case.marker and case.marker in markdown["stdout"]), f"bytes={len(markdown['stdout'].encode())}", markdown["elapsed_ms"])

        wrapper = ROOT / "skills" / "uparser" / "scripts" / "uparser-parse.sh"
        wrapper_env = os.environ.copy()
        wrapper_env["UPARSER_BIN"] = str(self.binary)
        wrapped_started = time.perf_counter()
        wrapped = subprocess.run(
            [str(wrapper), str(source), "--mode", "native", "--format", "markdown", "--no-assets", "--no-cache"],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=wrapper_env,
            timeout=30,
            check=False,
        )
        wrapped_ms = round((time.perf_counter() - wrapped_started) * 1000, 3)
        self.expect(case, "skill_wrapper", wrapped.returncode == 0 and bool(case.marker and case.marker in wrapped.stdout), f"exit={wrapped.returncode}, bytes={len(wrapped.stdout.encode())}", wrapped_ms)

        if case.quality_route == "native":
            automatic = self.run(["parse", "--mode", "auto", "--format", "json", "--no-assets", "--no-cache", str(source)])
            payload = self.parse_json(automatic)
            auto_route = payload.get("route_decision", {}).get("protocol") if payload else None
            self.expect(case, "auto_quality_parse", automatic["returncode"] == 0 and auto_route == "native", f"exit={automatic['returncode']}, route={auto_route}", automatic["elapsed_ms"])
        else:
            tool = "soffice" if case.structured else "VLM endpoint"
            self.skip(case, "auto_quality_parse", f"quality route is mineru-vlm; unavailable dependency: {tool}")

        if case.structured:
            canonical = self.run(["parse", "--mode", "native", "--format", "document-json", "--no-assets", str(source)])
            payload = self.parse_json(canonical)
            self.expect(case, "document_json", canonical["returncode"] == 0 and bool(payload and payload.get("units")), f"exit={canonical['returncode']}, units={len(payload.get('units', [])) if payload else 0}", canonical["elapsed_ms"])

    def cross_format_checks(self) -> None:
        pdf = self.fixtures / "01-pdf.pdf"
        rejected = self.run(["parse", "--mode", "native", "--format", "document-json", str(pdf)])
        self.expect(None, "pdf_rejects_document_json", rejected["returncode"] == 1, f"exit={rejected['returncode']}", rejected["elapsed_ms"])

        with tempfile.TemporaryDirectory(prefix="uparser-format-matrix-") as temporary:
            boundary = Path(temporary)
            disguised = boundary / "content-wins.pdf"
            shutil.copyfile(self.fixtures / "03-docx.docx", disguised)
            planned = self.run(["plan", "--mode", "auto", "--prefer", "speed", str(disguised)])
            payload = self.parse_json(planned)
            detection = payload.get("format", {}) if payload else {}
            self.expect(None, "content_wins_extension", planned["returncode"] == 0 and detection.get("format") == "docx" and bool(detection.get("warnings")), f"detected={detection.get('format')}, warnings={len(detection.get('warnings', []))}", planned["elapsed_ms"])

            malformed = boundary / "malformed.csv"
            malformed.write_text("a,b\nonly-one-column\n", encoding="utf-8")
            classified = self.run(["classify", str(malformed)])
            self.expect(None, "malformed_csv_is_unknown", classified["returncode"] == 1, f"exit={classified['returncode']}", classified["elapsed_ms"])

        rtf = self.fixtures / "10-rtf.rtf"
        with_notes = self.run(["parse", "--mode", "native", "--format", "markdown", "--no-assets", str(rtf)])
        without_notes = self.run(["parse", "--mode", "native", "--format", "markdown", "--no-assets", "--no-notes", str(rtf)])
        self.expect(None, "no_notes", "Matrix note" in with_notes["stdout"] and "Matrix note" not in without_notes["stdout"], "footnote removed only with --no-notes")

        docx = self.fixtures / "03-docx.docx"
        default = self.run(["parse", "--mode", "native", "--format", "markdown", "--no-assets", str(docx)])
        headers = self.run(["parse", "--mode", "native", "--format", "markdown", "--no-assets", "--headers-footers", str(docx)])
        good = "Matrix running header" not in default["stdout"] and "Matrix running footer" not in default["stdout"] and "Matrix running header" in headers["stdout"] and "Matrix running footer" in headers["stdout"]
        self.expect(None, "headers_footers", good, "excluded by default and restored by flag")

        side_effects = list(self.fixtures.glob("*_images"))
        self.expect(None, "no_assets", not side_effects, f"asset_directories={len(side_effects)}")

    def record_real(self, source: Path, name: str, status: str, detail: str, elapsed_ms: float | None = None) -> None:
        key = source.name
        check = {"format": f"real:{key}", "name": name, "status": status, "detail": detail, "elapsed_ms": elapsed_ms}
        self.checks.append(check)
        self.real_results.setdefault(key, {"file": str(source), "checks": []})["checks"].append(check)
        print(f"{status:4} {'real':12} {key} / {name}: {detail}")

    def expect_real(self, source: Path, name: str, condition: bool, detail: str, elapsed_ms: float | None = None) -> None:
        self.record_real(source, name, "PASS" if condition else "FAIL", detail, elapsed_ms)

    def test_real_file(self, source: Path) -> None:
        classified = self.run(["classify", str(source)], timeout=180)
        profile = self.parse_json(classified)
        detected = profile.get("source_format") if profile else None
        quality = profile.get("source_quality") if profile else None
        self.expect_real(source, "classify", classified["returncode"] == 0 and bool(detected), f"format={detected}, quality={quality}", classified["elapsed_ms"])
        if classified["returncode"] != 0 or not profile:
            return

        routes: dict[str, str | None] = {}
        for preference in ("quality", "speed", "cost"):
            planned = self.run(["plan", "--mode", "auto", "--prefer", preference, str(source)], timeout=180)
            payload = self.parse_json(planned)
            route = payload.get("plan", {}).get("route", {}).get("protocol") if payload else None
            routes[preference] = route
            complete = bool(payload and payload.get("profile") and payload.get("plan", {}).get("preprocess") and payload.get("plan", {}).get("route", {}).get("candidates"))
            self.expect_real(source, f"plan_{preference}", planned["returncode"] == 0 and bool(route) and complete, f"route={route}", planned["elapsed_ms"])

        if quality == "structured" or quality == "native_text":
            native = self.run(["parse", "--mode", "native", "--format", "json", "--no-assets", "--no-cache", str(source)], timeout=180)
            payload = self.parse_json(native)
            pages = payload.get("pages", []) if payload else []
            blocks = sum(len(page.get("blocks", [])) for page in pages)
            good = native["returncode"] == 0 and payload and payload.get("route_decision", {}).get("protocol") == "native" and pages and not payload.get("page_errors")
            self.expect_real(source, "native_json", bool(good), f"exit={native['returncode']}, pages={len(pages)}, blocks={blocks}, warnings={len(payload.get('warnings', [])) if payload else 0}", native["elapsed_ms"])

            markdown = self.run(["parse", "--mode", "native", "--format", "markdown", "--no-assets", "--no-cache", str(source)], timeout=180)
            self.expect_real(source, "native_markdown", markdown["returncode"] == 0 and bool(markdown["stdout"].strip()), f"bytes={len(markdown['stdout'].encode())}", markdown["elapsed_ms"])
            if native["returncode"] == 0 and markdown["returncode"] == 0:
                output_dir = self.results / "real_data_outputs"
                output_dir.mkdir(parents=True, exist_ok=True)
                (output_dir / f"{source.name}.native.json").write_text(native["stdout"], encoding="utf-8")
                (output_dir / f"{source.name}.native.md").write_text(markdown["stdout"], encoding="utf-8")

            if quality == "structured":
                canonical = self.run(["parse", "--mode", "native", "--format", "document-json", "--no-assets", str(source)], timeout=180)
                document = self.parse_json(canonical)
                self.expect_real(source, "document_json", canonical["returncode"] == 0 and bool(document and document.get("units")), f"exit={canonical['returncode']}, units={len(document.get('units', [])) if document else 0}", canonical["elapsed_ms"])

            wrapper = ROOT / "skills" / "uparser" / "scripts" / "uparser-parse.sh"
            wrapper_env = os.environ.copy()
            wrapper_env["UPARSER_BIN"] = str(self.binary)
            started = time.perf_counter()
            wrapped = subprocess.run(
                [str(wrapper), str(source), "--mode", "native", "--format", "markdown", "--no-assets", "--no-cache"],
                cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env=wrapper_env, timeout=180, check=False,
            )
            self.expect_real(source, "skill_wrapper", wrapped.returncode == 0 and bool(wrapped.stdout.strip()), f"exit={wrapped.returncode}, bytes={len(wrapped.stdout.encode())}", round((time.perf_counter() - started) * 1000, 3))

            if routes.get("quality") == "native":
                automatic = self.run(["parse", "--mode", "auto", "--format", "json", "--no-assets", "--no-cache", str(source)], timeout=180)
                auto_payload = self.parse_json(automatic)
                route = auto_payload.get("route_decision", {}).get("protocol") if auto_payload else None
                self.expect_real(source, "auto_quality_parse", automatic["returncode"] == 0 and route == "native", f"exit={automatic['returncode']}, route={route}", automatic["elapsed_ms"])
            else:
                self.record_real(source, "auto_quality_parse", "SKIP", "quality route requires VLM endpoint and Office conversion")
        else:
            rejected = self.run(["parse", "--mode", "native", "--format", "json", "--no-assets", str(source)], timeout=180)
            self.expect_real(source, "native_infeasible", rejected["returncode"] == 1, f"exit={rejected['returncode']}", rejected["elapsed_ms"])
            if self.endpoint and detected in ("png", "jpeg", "pdf"):
                parsed = self.run([
                    "parse", "--mode", "protocol", "--protocol", "mineru-vlm",
                    "--endpoint", self.endpoint, "--model", self.model,
                    "--format", "json", "--no-assets", "--no-cache", str(source),
                ], timeout=900)
                self.expect_real(source, "vlm_parse", parsed["returncode"] in (0, 3), f"exit={parsed['returncode']}", parsed["elapsed_ms"])
            else:
                self.record_real(source, "vlm_parse", "SKIP", "no reachable/configured VLM endpoint")

    def write_report(self) -> tuple[Path, Path]:
        self.results.mkdir(parents=True, exist_ok=True)
        passed = sum(check["status"] == "PASS" for check in self.checks)
        failed = sum(check["status"] == "FAIL" for check in self.checks)
        skipped = sum(check["status"] == "SKIP" for check in self.checks)
        version_result = self.run(["--version"])
        binary_version = version_result["stdout"].strip() or "current source build (CLI exposes no version flag)"
        report = {
            "contract": {"variants": 16, "recognized": 15},
            "environment": {
                "binary": str(self.binary),
                "binary_version": binary_version,
                "vlm_endpoint": self.endpoint,
                "soffice": shutil.which("soffice"),
            },
            "summary": {"checks": len(self.checks), "passed": passed, "failed": failed, "skipped": skipped},
            "formats": list(self.format_results.values()),
            "real_data": list(self.real_results.values()),
            "checks": self.checks,
        }
        json_path = self.results / "FORMAT_MATRIX_REPORT.json"
        json_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

        lines = [
            "# uparser V2 16-format skill matrix",
            "",
            f"- Checks: {len(self.checks)}",
            f"- Passed: {passed}",
            f"- Failed: {failed}",
            f"- Skipped: {skipped}",
            f"- Binary: `{self.binary}`",
            f"- VLM endpoint: `{self.endpoint or 'unavailable'}`",
            f"- LibreOffice: `{shutil.which('soffice') or 'unavailable'}`",
            "",
            "| Format | Pass | Fail | Skip |",
            "|---|---:|---:|---:|",
        ]
        for case in CASES:
            checks = self.format_results[case.format]["checks"]
            lines.append(
                f"| {case.format} | {sum(c['status'] == 'PASS' for c in checks)} | "
                f"{sum(c['status'] == 'FAIL' for c in checks)} | {sum(c['status'] == 'SKIP' for c in checks)} |"
            )
        if self.real_results:
            lines.extend(["", "## Real data", "", "| File | Pass | Fail | Skip |", "|---|---:|---:|---:|"])
            for item in self.real_results.values():
                checks = item["checks"]
                lines.append(
                    f"| {Path(item['file']).name} | {sum(c['status'] == 'PASS' for c in checks)} | "
                    f"{sum(c['status'] == 'FAIL' for c in checks)} | {sum(c['status'] == 'SKIP' for c in checks)} |"
                )
        failures = [check for check in self.checks if check["status"] == "FAIL"]
        skips = [check for check in self.checks if check["status"] == "SKIP"]
        lines.extend(["", "## Failures", ""])
        lines.extend([f"- `{item['format']} / {item['name']}`: {item['detail']}" for item in failures] or ["None."])
        lines.extend(["", "## Skipped external paths", ""])
        lines.extend([f"- `{item['format']} / {item['name']}`: {item['detail']}" for item in skips] or ["None."])
        markdown_path = self.results / "FORMAT_MATRIX_REPORT.md"
        markdown_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return json_path, markdown_path


def generate(fixtures: Path) -> None:
    subprocess.run(
        ["cargo", "run", "-q", "-p", "uparser-document-engine", "--example", "generate_format_matrix", "--", str(fixtures)],
        cwd=ROOT / "uparser",
        check=True,
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--uparser", type=Path, default=ROOT / "uparser" / "target" / "release" / "uparser")
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--results", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("--skip-generate", action="store_true")
    parser.add_argument("--endpoint", default=os.environ.get("UPARSER_ENDPOINT"))
    parser.add_argument("--model", default=os.environ.get("UPARSER_MODEL", "MinerU2.5-Pro-2605-1.2B"))
    parser.add_argument("--real-data", type=Path, default=ROOT / "bench" / "data")
    parser.add_argument("--skip-real-data", action="store_true")
    args = parser.parse_args()

    binary = args.uparser.resolve()
    fixtures = args.fixtures.resolve()
    results = args.results.resolve()
    if not binary.is_file():
        parser.error(f"uparser binary not found: {binary}")
    if not args.skip_generate:
        generate(fixtures)

    matrix = Matrix(binary, fixtures, results, args.endpoint, args.model)
    for case in CASES:
        matrix.test_case(case)
    matrix.cross_format_checks()
    if not args.skip_real_data and args.real_data.is_dir():
        for source in sorted(path for path in args.real_data.iterdir() if path.is_file()):
            matrix.test_real_file(source.resolve())
    json_path, markdown_path = matrix.write_report()
    failures = sum(check["status"] == "FAIL" for check in matrix.checks)
    print(f"reports: {json_path} {markdown_path}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
