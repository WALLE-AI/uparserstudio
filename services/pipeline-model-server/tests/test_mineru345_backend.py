import base64
from pathlib import Path
import tempfile
import unittest

from PIL import Image

from uparser_pipeline_server.mineru345_backend import MinerU345PageBackend
from uparser_pipeline_server.schemas import PageAnalyzeInput, PageImage


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
    def test_returns_official_markdown_and_forwards_pipeline_options(self):
        calls = []

        def parser(output_dir, names, payloads, languages, **options):
            calls.append((names, payloads, languages, options))
            target = Path(output_dir) / "page" / "ocr"
            target.mkdir(parents=True)
            (target / "page.md").write_text("# Finalized\n", encoding="utf-8")

        backend = MinerU345PageBackend(
            Path("."),
            Path("missing.json"),
            parser=parser,
            image_to_pdf=lambda payload: b"%PDF-fixture\n" + payload,
        )
        result = backend(page_input())

        self.assertEqual(result.markdown, "# Finalized\n")
        self.assertEqual(result.regions, [])
        self.assertEqual(calls[0][0], ["page"])
        self.assertTrue(calls[0][1][0].startswith(b"%PDF-fixture"))
        self.assertEqual(calls[0][2], ["ch"])
        self.assertEqual(calls[0][3]["backend"], "pipeline")
        self.assertTrue(calls[0][3]["formula_enable"])
        self.assertTrue(calls[0][3]["table_enable"])

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
