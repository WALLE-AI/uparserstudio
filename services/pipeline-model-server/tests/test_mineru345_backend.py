import base64
from pathlib import Path
import tempfile
import unittest

from PIL import Image

from uparser_pipeline_server.mineru345_backend import (
    MinerU345PageBackend,
    validate_transformers_runtime,
)
from uparser_pipeline_server.schemas import DocumentAnalyzeInput, PageAnalyzeInput, PageImage


def page_input() -> PageAnalyzeInput:
    with tempfile.NamedTemporaryFile(suffix=".png") as image_file:
        Image.new("RGB", (8, 6), "white").save(image_file, format="PNG")
        image_file.seek(0)
        encoded = base64.b64encode(image_file.read()).decode("ascii")
    return PageAnalyzeInput.model_validate(
        {
            "page": {
                "page_id": "fixture/page-1",
                "image": {"media_type": "image/png", "base64_data": encoded},
                "dimensions": {"width": 8, "height": 6},
                "rotation_degrees": 0,
            },
            "language": "ch",
            "formula_enabled": True,
            "table_enabled": True,
        }
    )


class MinerU345PageBackendTests(unittest.TestCase):
    def test_requires_transformers_with_hgnet_v2(self):
        calls = []
        validate_transformers_runtime("4.57.6", lambda model_type: calls.append(model_type))
        self.assertEqual(calls, ["hgnet_v2"])

        with self.assertRaisesRegex(RuntimeError, "requires transformers"):
            validate_transformers_runtime("4.51.3", lambda model_type: object())

    def test_rejects_transformers_without_hgnet_v2(self):
        def missing_backbone(model_type):
            raise KeyError(model_type)

        with self.assertRaisesRegex(RuntimeError, "cannot resolve"):
            validate_transformers_runtime("4.57.6", missing_backbone)

    def test_returns_official_markdown_and_forwards_pipeline_options(self):
        calls = []

        def parser(output_dir, names, payloads, languages, **options):
            calls.append((names, payloads, languages, options))
            for name in names:
                target = Path(output_dir) / name / "ocr"
                target.mkdir(parents=True)
                (target / f"{name}.md").write_text("# Finalized\n", encoding="utf-8")
                image_dir = target / "images"
                image_dir.mkdir()
                (image_dir / "figure.jpg").write_bytes(b"jpeg-fixture")

        backend = MinerU345PageBackend(
            Path("."),
            Path("missing.json"),
            parser=parser,
            image_to_pdf=lambda payload: b"%PDF-fixture\n" + payload,
        )
        result = backend(page_input())

        self.assertEqual(result.markdown, "# Finalized\n")
        self.assertEqual(result.regions, [])
        self.assertEqual(result.assets[0].path, "images/figure.jpg")
        self.assertEqual(base64.b64decode(result.assets[0].base64_data), b"jpeg-fixture")
        self.assertEqual(calls[0][0], ["document_0"])
        self.assertTrue(calls[0][1][0].startswith(b"%PDF-fixture"))
        self.assertEqual(calls[0][2], ["ch"])
        self.assertEqual(calls[0][3]["backend"], "pipeline")
        self.assertTrue(calls[0][3]["formula_enable"])
        self.assertTrue(calls[0][3]["table_enable"])

    def test_batches_multiple_pages_in_one_mineru_call(self):
        calls = []

        def parser(output_dir, names, payloads, languages, **options):
            calls.append(names)
            for name in names:
                target = Path(output_dir) / name / "ocr"
                target.mkdir(parents=True)
                (target / f"{name}.md").write_text(f"# {name}\n", encoding="utf-8")

        backend = MinerU345PageBackend(
            Path("."), Path("missing.json"), parser=parser, image_to_pdf=lambda payload: payload
        )
        results = backend.infer_batch([page_input(), page_input()])

        self.assertEqual(calls, [["document_0", "document_1"]])
        self.assertEqual(
            [result.markdown for result in results],
            ["# document_0\n", "# document_1\n"],
        )

    def test_document_path_preserves_multipage_pdf_payload(self):
        calls = []

        def parser(output_dir, names, payloads, languages, **options):
            calls.append(payloads)
            target = Path(output_dir) / names[0] / "ocr"
            target.mkdir(parents=True)
            (target / f"{names[0]}.md").write_text("pages merged", encoding="utf-8")

        payload = b"%PDF-1.7\nfixture"
        item = DocumentAnalyzeInput.model_validate(
            {
                "document": {
                    "document_id": "doc-1",
                    "media_type": "application/pdf",
                    "base64_data": base64.b64encode(payload).decode("ascii"),
                },
                "language": "ch",
                "formula_enabled": True,
                "table_enabled": True,
            }
        )
        backend = MinerU345PageBackend(Path("."), Path("missing.json"), parser=parser)

        result = backend.infer_document(item)

        self.assertEqual(calls, [[payload]])
        self.assertEqual(result.markdown, "pages merged")

    def test_rejects_declared_dimension_mismatch_before_inference(self):
        item = page_input()
        item.page = PageImage.model_validate(
            item.page.model_dump(mode="python") | {"dimensions": {"width": 9, "height": 6}}
        )
        backend = MinerU345PageBackend(Path("."), Path("missing.json"), parser=lambda *a, **k: None)

        with self.assertRaisesRegex(ValueError, "do not match declared"):
            backend(item)


if __name__ == "__main__":
    unittest.main()
