import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "pipeline_model_preflight.py"
SPEC = importlib.util.spec_from_file_location("pipeline_model_preflight", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class PipelineModelPreflightTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.models = self.root / "models"
        self.runtime = self.root / "magic_pdf"
        self.reader = self.root / "layoutreader"
        self.mineru = self.root / "MinerU"
        self.config = self.root / "magic-pdf.json"
        self._write_required_assets()
        self._touch(self.mineru / "mineru/version.py", b'__version__ = "test-current"\n')
        self.config.write_text(
            json.dumps(
                {
                    "models-dir": str(self.models),
                    "layoutreader-model-dir": str(self.reader),
                    "device-mode": "cpu",
                    "layout-config": {"model": "doclayout_yolo"},
                    "formula-config": {
                        "mfd_model": "yolo_v8_mfd",
                        "mfr_model": "unimernet_small",
                        "enable": True,
                    },
                    "table-config": {
                        "model": "rapid_table",
                        "sub_model": "slanet_plus",
                        "enable": True,
                    },
                    "config_version": "1.2.0",
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self):
        self.tempdir.cleanup()

    def _touch(self, path: Path, content: bytes = b"fixture"):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)

    def _write_required_assets(self):
        for paths in MODULE.MODEL_PATHS.values():
            for relative in paths:
                self._touch(self.models / relative)
        det, rec, dictionary = MODULE.OCR_LANGUAGES["ch"]
        self._touch(self.models / "OCR/paddleocr_torch" / det)
        self._touch(self.models / "OCR/paddleocr_torch" / rec)
        resources = (
            self.runtime
            / "model/sub_modules/ocr/paddleocr2pytorch/pytorchocr/utils/resources"
        )
        self._touch(resources / "dict" / dictionary)
        self._touch(resources / "models_config.yml")
        self._touch(self.runtime / "resources/slanet_plus/slanet-plus.onnx")
        self._touch(self.reader / "config.json", b"{}")
        self._touch(self.reader / "model.safetensors")

    def args(self, **overrides):
        values = {
            "config": self.config,
            "output": None,
            "magic_pdf_root": self.runtime,
            "mineru_root": self.mineru,
            "hash_scope": "required",
            "skip_device_check": False,
        }
        values.update(overrides)
        return type("Args", (), values)()

    def test_complete_legacy_assets_are_ready(self):
        manifest = MODULE.build_manifest(self.args())

        self.assertEqual(manifest["status"], "ready")
        self.assertEqual(manifest["summary"]["errors"], 0)
        self.assertTrue(all(item["sha256"] for item in manifest["assets"]))
        self.assertNotIn("bucket_info", manifest["source"])
        self.assertEqual(manifest["reference_pipeline"]["version"], "test-current")
        self.assertIn("torch", manifest["runtime"]["packages"])

    def test_runtime_slanet_is_resolved_and_reported(self):
        manifest = MODULE.build_manifest(self.args())

        table = next(item for item in manifest["assets"] if item["logical_model"] == "table")
        self.assertIn("magic_pdf/resources/slanet_plus", table["path"])
        self.assertIn(
            "table_weight_from_runtime",
            {item["code"] for item in manifest["diagnostics"]},
        )

    def test_missing_model_is_a_blocking_diagnostic(self):
        missing = self.models / MODULE.MODEL_PATHS["yolo_v8_mfd"][0]
        missing.unlink()

        manifest = MODULE.build_manifest(self.args())

        self.assertEqual(manifest["status"], "blocked")
        diagnostic = next(
            item for item in manifest["diagnostics"] if item["path"] == str(missing)
        )
        self.assertEqual(diagnostic["code"], "asset_missing")

    def test_all_scope_inventories_non_required_files(self):
        extra = self.models / "optional.bin"
        self._touch(extra, b"optional")

        manifest = MODULE.build_manifest(self.args(hash_scope="all"))

        item = next(entry for entry in manifest["inventory"] if entry["path"] == str(extra))
        self.assertFalse(item["required"])
        self.assertEqual(item["sha256"], MODULE.sha256_file(extra))


if __name__ == "__main__":
    unittest.main()
