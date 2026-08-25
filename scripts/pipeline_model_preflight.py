#!/usr/bin/env python3
"""Validate a legacy magic-pdf pipeline configuration and freeze its assets."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import importlib.util
import json
import os
import platform
import re
import subprocess
import sys
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable


SCHEMA_VERSION = 1

MODEL_PATHS = {
    "doclayout_yolo": ("Layout/YOLO/doclayout_yolo_docstructbench_imgsz1280_2501.pt",),
    "yolo_v8_mfd": ("MFD/YOLO/yolo_v8_ft.pt",),
    "unimernet_small": (
        "MFR/unimernet_hf_small_2503/config.json",
        "MFR/unimernet_hf_small_2503/generation_config.json",
        "MFR/unimernet_hf_small_2503/model.safetensors",
        "MFR/unimernet_hf_small_2503/tokenizer.json",
        "MFR/unimernet_hf_small_2503/tokenizer_config.json",
    ),
}

OCR_LANGUAGES = {
    "ch": (
        "ch_PP-OCRv3_det_infer.pth",
        "ch_PP-OCRv4_rec_server_infer.pth",
        "ppocr_keys_v1.txt",
    ),
    "ch_lite": (
        "ch_PP-OCRv3_det_infer.pth",
        "ch_PP-OCRv4_rec_infer.pth",
        "ppocr_keys_v1.txt",
    ),
    "en": ("en_PP-OCRv3_det_infer.pth", "en_PP-OCRv4_rec_infer.pth", "en_dict.txt"),
    "korean": (
        "Multilingual_PP-OCRv3_det_infer.pth",
        "korean_PP-OCRv3_rec_infer.pth",
        "korean_dict.txt",
    ),
    "japan": (
        "Multilingual_PP-OCRv3_det_infer.pth",
        "japan_PP-OCRv3_rec_infer.pth",
        "japan_dict.txt",
    ),
    "chinese_cht": (
        "Multilingual_PP-OCRv3_det_infer.pth",
        "chinese_cht_PP-OCRv3_rec_infer.pth",
        "chinese_cht_dict.txt",
    ),
    "latin": (
        "en_PP-OCRv3_det_infer.pth",
        "latin_PP-OCRv3_rec_infer.pth",
        "latin_dict.txt",
    ),
    "arabic": (
        "Multilingual_PP-OCRv3_det_infer.pth",
        "arabic_PP-OCRv3_rec_infer.pth",
        "arabic_dict.txt",
    ),
    "cyrillic": (
        "Multilingual_PP-OCRv3_det_infer.pth",
        "cyrillic_PP-OCRv3_rec_infer.pth",
        "cyrillic_dict.txt",
    ),
    "devanagari": (
        "Multilingual_PP-OCRv3_det_infer.pth",
        "devanagari_PP-OCRv3_rec_infer.pth",
        "devanagari_dict.txt",
    ),
}

LICENSE_REVIEW = {
    "layout": "AGPL-3.0 model; BYOM/internal profile only until legal review",
    "formula_detection": "AGPL-3.0 model; BYOM/internal profile only until legal review",
    "formula_recognition": "review model card and weight terms before redistribution",
    "ocr": "review PaddleOCR model and bundled conversion terms before redistribution",
    "table": "review RapidTable/SLANet model terms before redistribution",
    "reading_order": "CC-BY-NC-SA-4.0 model; non-commercial/BYOM restriction",
}

RUNTIME_PACKAGES = (
    "mineru",
    "magic-pdf",
    "torch",
    "torchvision",
    "transformers",
    "ultralytics",
    "rapid-table",
    "onnxruntime",
    "opencv-python-headless",
    "pypdfium2",
)


@dataclass(frozen=True)
class Diagnostic:
    level: str
    code: str
    message: str
    path: str | None = None


@dataclass(frozen=True)
class Asset:
    logical_model: str
    role: str
    path: str
    exists: bool
    size_bytes: int | None
    sha256: str | None


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_config(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as stream:
        config = json.load(stream)
    if not isinstance(config, dict):
        raise ValueError("magic-pdf config must be a JSON object")
    return config


def discover_magic_pdf_root(explicit: Path | None) -> Path | None:
    if explicit is not None:
        return explicit.resolve()
    spec = importlib.util.find_spec("magic_pdf")
    if spec is None or spec.origin is None:
        return None
    return Path(spec.origin).resolve().parent


def discover_mineru_root(explicit: Path | None) -> Path | None:
    if explicit is not None:
        return explicit.resolve()
    candidate = Path(__file__).resolve().parents[1] / "opensource/MinerU"
    return candidate if (candidate / "mineru/version.py").is_file() else None


def mineru_reference(root: Path | None) -> tuple[dict, list[Diagnostic]]:
    if root is None or not (root / "mineru/version.py").is_file():
        return {}, [
            Diagnostic(
                "error",
                "mineru_reference_missing",
                "the current MinerU source tree is required as the pipeline behavior reference",
                str(root) if root else None,
            )
        ]
    version_source = (root / "mineru/version.py").read_text(encoding="utf-8")
    match = re.search(r'__version__\s*=\s*["\']([^"\']+)', version_source)
    version = match.group(1) if match else None
    commit = None
    try:
        completed = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if completed.returncode == 0:
            commit = completed.stdout.strip() or None
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    diagnostics = []
    if version is None:
        diagnostics.append(
            Diagnostic("error", "mineru_version_missing", "cannot read MinerU __version__", str(root))
        )
    if commit is None:
        diagnostics.append(
            Diagnostic(
                "warning",
                "mineru_commit_unavailable",
                "MinerU source is not a Git checkout; source hash must be frozen separately",
                str(root),
            )
        )
    return {
        "role": "architecture_and_end_to_end_behavior_reference",
        "root": str(root),
        "version": version,
        "git_commit": commit,
        "legacy_magic_pdf_role": "model_weight_compatibility_only",
    }, diagnostics


def asset(logical_model: str, role: str, path: Path, hash_file: bool) -> Asset:
    exists = path.is_file()
    return Asset(
        logical_model=logical_model,
        role=role,
        path=str(path),
        exists=exists,
        size_bytes=path.stat().st_size if exists else None,
        sha256=sha256_file(path) if exists and hash_file else None,
    )


def required_assets(
    config: dict,
    magic_pdf_root: Path | None,
    hash_files: bool,
) -> tuple[list[Asset], list[Diagnostic], dict]:
    diagnostics: list[Diagnostic] = []
    assets: list[Asset] = []
    models_dir_value = config.get("models-dir")
    if not isinstance(models_dir_value, str) or not models_dir_value:
        diagnostics.append(Diagnostic("error", "models_dir_missing", "models-dir is required"))
        return assets, diagnostics, {}
    models_dir = Path(models_dir_value).expanduser().resolve()
    if not models_dir.is_dir():
        diagnostics.append(
            Diagnostic("error", "models_dir_not_found", "models-dir does not exist", str(models_dir))
        )

    logical = {
        "layout": config.get("layout-config", {}).get("model", "doclayout_yolo"),
        "formula_detection": config.get("formula-config", {}).get("mfd_model", "yolo_v8_mfd"),
        "formula_recognition": config.get("formula-config", {}).get("mfr_model", "unimernet_small"),
        "ocr": config.get("ocr-config", {}).get("lang", "ch"),
        "table": config.get("table-config", {}).get("sub_model", "slanet_plus"),
        "reading_order": "layoutreader",
    }

    for stage, model_name in (
        ("layout", logical["layout"]),
        ("formula_detection", logical["formula_detection"]),
        ("formula_recognition", logical["formula_recognition"]),
    ):
        relative_paths = MODEL_PATHS.get(model_name)
        if relative_paths is None:
            diagnostics.append(
                Diagnostic("error", "unsupported_model", f"unsupported {stage} model: {model_name}")
            )
            continue
        for relative_path in relative_paths:
            assets.append(asset(stage, "weight_or_config", models_dir / relative_path, hash_files))

    ocr_lang = logical["ocr"]
    ocr_spec = OCR_LANGUAGES.get(ocr_lang)
    if ocr_spec is None:
        diagnostics.append(
            Diagnostic("error", "unsupported_ocr_language", f"unsupported OCR language: {ocr_lang}")
        )
    else:
        if "ocr-config" not in config:
            diagnostics.append(
                Diagnostic(
                    "warning",
                    "ocr_language_defaulted",
                    "ocr-config.lang is absent; legacy magic-pdf defaults to ch",
                )
            )
        det_name, rec_name, dict_name = ocr_spec
        ocr_dir = models_dir / "OCR/paddleocr_torch"
        assets.append(asset("ocr", "detector_weight", ocr_dir / det_name, hash_files))
        assets.append(asset("ocr", "recognizer_weight", ocr_dir / rec_name, hash_files))
        if magic_pdf_root is None:
            diagnostics.append(
                Diagnostic(
                    "error",
                    "magic_pdf_runtime_missing",
                    "magic_pdf package root is required for OCR dictionaries and runtime assets",
                )
            )
        else:
            resource_root = (
                magic_pdf_root
                / "model/sub_modules/ocr/paddleocr2pytorch/pytorchocr/utils/resources"
            )
            assets.append(asset("ocr", "character_dictionary", resource_root / "dict" / dict_name, hash_files))
            assets.append(asset("ocr", "model_mapping", resource_root / "models_config.yml", hash_files))

    formula_enabled = bool(config.get("formula-config", {}).get("enable", True))
    table_enabled = bool(config.get("table-config", {}).get("enable", False))
    if not formula_enabled:
        diagnostics.append(Diagnostic("warning", "formula_disabled", "formula stages are disabled"))
    if table_enabled:
        if logical["table"] != "slanet_plus":
            diagnostics.append(
                Diagnostic("error", "unsupported_table_model", f"unsupported table sub-model: {logical['table']}")
            )
        else:
            repository_table = models_dir / "TabRec/SlanetPlus/slanet-plus.onnx"
            runtime_table = (
                magic_pdf_root / "resources/slanet_plus/slanet-plus.onnx"
                if magic_pdf_root is not None
                else None
            )
            table_path = repository_table if repository_table.is_file() else runtime_table
            if table_path is None:
                table_path = repository_table
            elif table_path == runtime_table:
                diagnostics.append(
                    Diagnostic(
                        "warning",
                        "table_weight_from_runtime",
                        "SLANet-plus is supplied by the magic_pdf runtime, not models-dir",
                        str(table_path),
                    )
                )
            assets.append(asset("table", "structure_weight", table_path, hash_files))

    layoutreader_value = config.get("layoutreader-model-dir")
    if isinstance(layoutreader_value, str) and layoutreader_value:
        layoutreader_dir = Path(layoutreader_value).expanduser().resolve()
        assets.append(asset("reading_order", "config", layoutreader_dir / "config.json", hash_files))
        assets.append(asset("reading_order", "weight", layoutreader_dir / "model.safetensors", hash_files))
    else:
        diagnostics.append(
            Diagnostic("error", "layoutreader_dir_missing", "layoutreader-model-dir is required")
        )

    for required in assets:
        if not required.exists:
            diagnostics.append(
                Diagnostic(
                    "error",
                    "asset_missing",
                    f"required {required.logical_model} {required.role} is missing",
                    required.path,
                )
            )
    return assets, diagnostics, logical


def scan_files(roots: Iterable[Path], required_paths: set[str]) -> list[dict]:
    scanned: list[dict] = []
    seen: set[Path] = set()
    for root in roots:
        if not root.is_dir():
            continue
        for path in sorted(item for item in root.rglob("*") if item.is_file()):
            resolved = path.resolve()
            if resolved in seen:
                continue
            seen.add(resolved)
            scanned.append(
                {
                    "path": str(resolved),
                    "relative_to_root": str(path.relative_to(root)),
                    "size_bytes": path.stat().st_size,
                    "sha256": sha256_file(path),
                    "required": str(resolved) in required_paths,
                }
            )
    return scanned


def check_device(config: dict, skip: bool) -> list[Diagnostic]:
    if skip or not str(config.get("device-mode", "cpu")).startswith("cuda"):
        return []
    try:
        completed = subprocess.run(
            ["nvidia-smi", "-L"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
        return [Diagnostic("error", "cuda_unavailable", f"nvidia-smi check failed: {exc}")]
    if completed.returncode != 0:
        message = completed.stderr.strip() or completed.stdout.strip() or "nvidia-smi returned an error"
        return [Diagnostic("error", "cuda_unavailable", message)]
    return []


def installed_package_versions() -> dict[str, str | None]:
    versions = {}
    for package in RUNTIME_PACKAGES:
        try:
            versions[package] = importlib.metadata.version(package)
        except importlib.metadata.PackageNotFoundError:
            versions[package] = None
    return versions


def build_manifest(args: argparse.Namespace) -> dict:
    config_path = args.config.resolve()
    config = load_config(config_path)
    magic_pdf_root = discover_magic_pdf_root(args.magic_pdf_root)
    mineru_root = discover_mineru_root(args.mineru_root)
    reference, reference_diagnostics = mineru_reference(mineru_root)
    assets, diagnostics, logical = required_assets(config, magic_pdf_root, True)
    diagnostics.extend(reference_diagnostics)
    diagnostics.extend(check_device(config, args.skip_device_check))
    models_dir_value = config.get("models-dir")
    roots = [Path(models_dir_value).expanduser().resolve()] if isinstance(models_dir_value, str) else []
    layoutreader_value = config.get("layoutreader-model-dir")
    if isinstance(layoutreader_value, str):
        roots.append(Path(layoutreader_value).expanduser().resolve())
    if magic_pdf_root is not None:
        roots.append(magic_pdf_root / "resources")
    scanned = []
    if args.hash_scope == "all":
        scanned = scan_files(roots, {item.path for item in assets})

    errors = sum(item.level == "error" for item in diagnostics)
    warnings = sum(item.level == "warning" for item in diagnostics)
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "status": "ready" if errors == 0 else "blocked",
        "summary": {"errors": errors, "warnings": warnings},
        "source": {
            "config_path": str(config_path),
            "config_sha256": sha256_file(config_path),
            "config_version": config.get("config_version"),
            "models_dir": config.get("models-dir"),
            "layoutreader_model_dir": config.get("layoutreader-model-dir"),
            "magic_pdf_root": str(magic_pdf_root) if magic_pdf_root else None,
        },
        "runtime": {
            "device": config.get("device-mode", "cpu"),
            "python": platform.python_version(),
            "executable": sys.executable,
            "platform": platform.platform(),
            "packages": installed_package_versions(),
        },
        "reference_pipeline": reference,
        "logical_models": logical,
        "license_review": LICENSE_REVIEW,
        "assets": [asdict(item) for item in assets],
        "inventory": scanned,
        "diagnostics": [asdict(item) for item in diagnostics],
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True, help="path to magic-pdf.json")
    parser.add_argument("--output", type=Path, help="write the JSON manifest to this path")
    parser.add_argument(
        "--magic-pdf-root",
        type=Path,
        help="installed magic_pdf package directory containing resources/",
    )
    parser.add_argument(
        "--mineru-root",
        type=Path,
        help="current MinerU source tree used as the architecture and behavior reference",
    )
    parser.add_argument(
        "--hash-scope",
        choices=("required", "all"),
        default="required",
        help="hash required files only, or inventory every file under model roots",
    )
    parser.add_argument("--skip-device-check", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        manifest = build_manifest(args)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"pipeline preflight failed: {exc}", file=sys.stderr)
        return 2
    rendered = json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    summary = manifest["summary"]
    print(
        f"pipeline preflight: {manifest['status']} "
        f"({summary['errors']} errors, {summary['warnings']} warnings)",
        file=sys.stderr,
    )
    return 0 if manifest["status"] == "ready" else 2


if __name__ == "__main__":
    raise SystemExit(main())
