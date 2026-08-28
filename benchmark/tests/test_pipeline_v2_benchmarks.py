import importlib.util
from pathlib import Path
import tempfile
import unittest

from PIL import Image


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "pipeline_bench", ROOT / "benchmark/run_pipeline_v2_benchmarks.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PipelineBenchmarkRendererTests(unittest.TestCase):
    def test_file_page_preserves_png_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "page.png"
            Image.new("RGB", (7, 5), "white").save(path)

            encoded = MODULE._file_page(path)

            self.assertEqual(encoded["image"]["media_type"], "image/png")
            self.assertEqual(
                __import__("base64").b64decode(encoded["image"]["base64_data"]),
                path.read_bytes(),
            )
            self.assertEqual(encoded["dimensions"], {"width": 7, "height": 5})

    def test_prefers_authoritative_full_pipeline_markdown(self):
        result = {
            "markdown": "# Official\n\nMerged paragraph\n",
            "regions": [],
            "ocr_spans": [],
            "formula_spans": [],
            "tables": [],
            "reading_order": [],
        }

        self.assertEqual(MODULE.render_markdown(result), "# Official\n\nMerged paragraph\n")

    def test_does_not_normalize_authoritative_markdown(self):
        self.assertEqual(MODULE.render_markdown({"markdown": "exact  "}), "exact  ")

    def test_renders_reading_order_headings_formula_and_canonical_table(self):
        result = {
            "regions": [
                {"region_id": "title", "label": "paragraph_title", "bbox": [0, 0, 100, 20]},
                {"region_id": "formula", "label": "display_formula", "bbox": [0, 30, 50, 50]},
                {"region_id": "table", "label": "table", "bbox": [0, 60, 100, 100]},
            ],
            "ocr_spans": [
                {
                    "parent_region_id": "title",
                    "text": "Heading",
                    "polygon": {"points": [[0, 0], [10, 0], [10, 10], [0, 10]]},
                }
            ],
            "formula_spans": [{"region_id": "formula", "latex": "x^2"}],
            "tables": [
                {"region_id": "table", "html": "<html><body><table><tr><td>A</td></tr></table></body></html>"}
            ],
            "reading_order": ["title", "formula", "table"],
        }

        rendered = MODULE.render_markdown(result)

        self.assertEqual(
            rendered,
            "## Heading\n\n$$\nx^2\n$$\n\n<table><tr><td>A</td></tr></table>\n",
        )

    def test_suppresses_formula_already_inside_table(self):
        result = {
            "regions": [
                {"region_id": "table", "label": "table", "bbox": [0, 0, 100, 100]},
                {"region_id": "formula", "label": "inline_formula", "bbox": [10, 10, 20, 20]},
            ],
            "ocr_spans": [],
            "formula_spans": [{"region_id": "formula", "latex": "x"}],
            "tables": [{"region_id": "table", "html": "<table></table>"}],
            "reading_order": ["formula", "table"],
        }

        self.assertEqual(MODULE.render_markdown(result), "<table></table>\n")

    def test_merges_resume_counts_wall_time_and_weighted_mean(self):
        previous = {
            "success": 8,
            "wall_seconds": 20.0,
            "mean_item_seconds": 2.0,
            "median_item_seconds": 1.5,
        }
        current = {
            "count": 10,
            "success": 2,
            "failures": [],
            "wall_seconds": 8.0,
            "mean_item_seconds": 4.0,
            "median_item_seconds": 4.0,
            "skipped_existing": 8,
        }

        merged = MODULE.merge_resume_summary(previous, current)

        self.assertEqual(merged["success"], 10)
        self.assertEqual(merged["wall_seconds"], 28.0)
        self.assertEqual(merged["mean_item_seconds"], 2.4)
        self.assertEqual(merged["median_item_seconds"], 1.5)
        self.assertEqual(merged["runs"], 2)

    def test_resume_counts_existing_outputs_without_matching_summary(self):
        current = {
            "count": 10,
            "success": 3,
            "failures": [],
            "wall_seconds": 4.0,
            "mean_item_seconds": 1.0,
            "median_item_seconds": 1.0,
            "skipped_existing": 7,
        }

        merged = MODULE.merge_resume_summary({"success": 1}, current)

        self.assertEqual(merged["success"], 10)
        self.assertEqual(merged["prior_completed_without_matching_summary"], 7)
        self.assertEqual(merged["timing_scope"], "current_resume_only")


if __name__ == "__main__":
    unittest.main()
