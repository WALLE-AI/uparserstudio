import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "select_pipeline_golden.py"
SPEC = importlib.util.spec_from_file_location("select_pipeline_golden", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def item(sample_id, language, layout, special_issue=None, categories=None):
    return {
        "page_info": {
            "page_attribute": {
                "data_source": "book",
                "language": language,
                "layout": layout,
                "subset": "v1.5",
                "special_issue": special_issue or [],
            },
            "sample_id": sample_id,
            "image_path": f"{sample_id}.jpg",
        },
        "layout_dets": [
            {"category_type": category} for category in (categories or ["text_block"])
        ],
    }


class PipelineGoldenSelectionTests(unittest.TestCase):
    def test_selection_prioritizes_rare_coverage(self):
        items = [
            item(1, "english", "single_column"),
            item(2, "english", "single_column"),
            item(
                3,
                "traditional_chinese",
                "three_column",
                ["table_with_formula"],
                ["table", "equation_isolated"],
            ),
        ]

        selected = MODULE.select_items(items, 1)

        self.assertEqual(selected[0]["page_info"]["sample_id"], 3)

    def test_selection_is_deterministic(self):
        items = [item(index, "english", "single_column") for index in range(10)]

        first = MODULE.select_items(items, 4)
        second = MODULE.select_items(items, 4)

        self.assertEqual(
            [entry["page_info"]["sample_id"] for entry in first],
            [entry["page_info"]["sample_id"] for entry in second],
        )


if __name__ == "__main__":
    unittest.main()
