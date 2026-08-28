import base64
import io
from pathlib import Path
import unittest

import numpy as np
from PIL import Image

from uparser_pipeline_server.mineru345_stage_backends import (
    Mineru345FormulaDetectBackend,
    Mineru345LayoutBackend,
    Mineru345TableBackend,
)
from uparser_pipeline_server.schemas import (
    EncodedImage,
    ImageDimensions,
    OcrSpan,
    Polygon,
    PageImage,
    Region,
    TableRecognitionInput,
)


def page(width=120, height=80):
    buffer = io.BytesIO()
    Image.new("RGB", (width, height), "white").save(buffer, format="PNG")
    return PageImage(
        page_id="page-1",
        image=EncodedImage(
            media_type="image/png",
            base64_data=base64.b64encode(buffer.getvalue()).decode("ascii"),
        ),
        dimensions=ImageDimensions(width=width, height=height),
    )


def region(region_id, label, bbox, confidence=0.9):
    return Region(
        region_id=region_id,
        label=label,
        bbox=bbox,
        confidence=confidence,
        coordinate_space="render_pixels",
    )


def span(span_id, points, text):
    return OcrSpan(
        span_id=span_id,
        polygon=Polygon(points=points),
        text=text,
        confidence=0.8,
        coordinate_space="render_pixels",
    )


class FakeLayoutModel:
    """Mirrors `PPDocLayoutV2LayoutModel.predict()`'s real return shape:
    a list of dicts with `bbox`/`label`/`score`/`index` keys, including
    the joint layout+formula-detection labels real PP-DocLayoutV2 emits
    (fact 1 in the module doc)."""

    def __init__(self):
        self.calls = 0

    def predict(self, image):
        self.calls += 1
        return [
            {"bbox": [0, 0, 100, 20], "label": "paragraph_title", "score": 0.9, "index": 0},
            {"bbox": [0, 25, 100, 45], "label": "text", "score": 0.8, "index": 1},
            {"bbox": [0, 50, 100, 65], "label": "display_formula", "score": 0.6, "index": 2},
            {"bbox": [10, 66, 40, 78], "label": "inline_formula", "score": 0.5, "index": 3},
        ]


class Mineru345LayoutBackendTests(unittest.TestCase):
    def test_returns_native_labels_unmapped(self):
        fake = FakeLayoutModel()
        backend = Mineru345LayoutBackend(
            Path("."), Path("mineru.json"), "cpu", loader=lambda *_: fake
        )
        result = backend(page())
        labels = [region.label for region in result.regions]
        self.assertEqual(
            labels, ["paragraph_title", "text", "display_formula", "inline_formula"]
        )

    def test_raw_predict_is_memoized_per_distinct_page_content(self):
        fake = FakeLayoutModel()
        backend = Mineru345LayoutBackend(
            Path("."), Path("mineru.json"), "cpu", loader=lambda *_: fake
        )
        same_page = page()

        backend.raw_predict(same_page)
        backend.raw_predict(same_page)
        backend.raw_predict(same_page)
        self.assertEqual(fake.calls, 1, "identical page content should hit the cache")

        backend.raw_predict(page(width=200))
        self.assertEqual(fake.calls, 2, "different page content is a genuine cache miss")

    def test_loader_is_only_invoked_once_across_repeated_calls(self):
        load_count = 0

        def loader(_source_root, _config_path, _device):
            nonlocal load_count
            load_count += 1
            return FakeLayoutModel()

        backend = Mineru345LayoutBackend(Path("."), Path("mineru.json"), "cpu", loader=loader)
        backend.raw_predict(page())
        backend.raw_predict(page(width=200))
        self.assertEqual(load_count, 1)


class Mineru345FormulaDetectBackendTests(unittest.TestCase):
    def test_filters_layout_output_for_formula_labels_without_a_second_model_call(self):
        fake = FakeLayoutModel()
        layout_backend = Mineru345LayoutBackend(
            Path("."), Path("mineru.json"), "cpu", loader=lambda *_: fake
        )
        # Prime the shared cache the way `run_workflow`'s concurrent
        # layout+formula_detect dispatch would (layout dispatched first).
        layout_backend.raw_predict(page())

        formula_backend = Mineru345FormulaDetectBackend(layout_backend)
        result = formula_backend(page())

        self.assertEqual(
            [region.label for region in result.regions],
            ["display_formula", "inline_formula"],
        )
        # The layout model was invoked exactly once — formula_detect's
        # request was served entirely from the shared cache, not a
        # redundant second inference.
        self.assertEqual(fake.calls, 1)

    def test_no_formula_regions_returns_an_empty_result_not_an_error(self):
        class NoFormulaLayoutModel:
            def predict(self, image):
                return [{"bbox": [0, 0, 10, 10], "label": "text", "score": 0.9, "index": 0}]

        layout_backend = Mineru345LayoutBackend(
            Path("."), Path("mineru.json"), "cpu", loader=lambda *_: NoFormulaLayoutModel()
        )
        formula_backend = Mineru345FormulaDetectBackend(layout_backend)
        result = formula_backend(page())
        self.assertEqual(result.regions, [])


class FakeTableClsModel:
    def __init__(self, label):
        self.label = label
        self.calls = 0

    def predict(self, crop):
        self.calls += 1
        return self.label, 0.97


class FakeWirelessModel:
    def predict(self, image, ocr_result):
        self.ocr_result = ocr_result
        return (
            '<table><tr><td>A</td></tr></table>',
            np.array([[0, 0, 60, 0, 60, 30, 0, 30]], dtype=float),
            [[0, 0, 0, 1]],
            0.01,
        )


class FakeWiredModel:
    def predict(self, input_img, ocr_result, wireless_html_code, return_metadata=False):
        self.wireless_html_code = wireless_html_code
        self.ocr_result = ocr_result
        assert return_metadata is True
        return {
            "html": '<table><tr><td colspan="2">B</td></tr></table>',
            "selected_model": "wired",
            "wired_cell_bboxes": np.array([[0, 0, 60, 0, 60, 30, 0, 30]], dtype=float),
            "wired_logic_points": [[0, 0, 0, 1]],
            "wired_html": '<table><tr><td colspan="2">B</td></tr></table>',
        }


class Mineru345TableBackendTests(unittest.TestCase):
    def test_wireless_classification_uses_the_plain_tuple_return_shape(self):
        cls_model = FakeTableClsModel("wireless_table")
        wireless = FakeWirelessModel()
        wired = FakeWiredModel()
        backend = Mineru345TableBackend(
            Path("."),
            Path("mineru.json"),
            "cpu",
            loader=lambda *_: (cls_model, wireless, wired),
        )
        result = backend(
            TableRecognitionInput(
                page=page(),
                table_regions=[region("table-1", "table", (10, 20, 70, 50))],
                ocr_spans=[span("s1", [(20, 25), (30, 25), (30, 35), (20, 35)], "inside")],
                formula_spans=[],
            )
        )
        self.assertEqual(len(result.tables), 1)
        table = result.tables[0]
        self.assertEqual(table.classifier_label, "wireless")
        self.assertEqual(table.html, "<table><tr><td>A</td></tr></table>")
        self.assertEqual(len(table.cells), 1)
        self.assertEqual(table.cells[0].bbox, (10.0, 20.0, 70.0, 50.0))

    def test_wired_classification_uses_the_metadata_dict_return_shape(self):
        cls_model = FakeTableClsModel("wired_table")
        wireless = FakeWirelessModel()
        wired = FakeWiredModel()
        backend = Mineru345TableBackend(
            Path("."),
            Path("mineru.json"),
            "cpu",
            loader=lambda *_: (cls_model, wireless, wired),
        )
        result = backend(
            TableRecognitionInput(
                page=page(),
                table_regions=[region("table-1", "table", (10, 20, 70, 50))],
                ocr_spans=[],
                formula_spans=[],
            )
        )
        table = result.tables[0]
        self.assertEqual(table.classifier_label, "wired")
        self.assertEqual(table.html, '<table><tr><td colspan="2">B</td></tr></table>')
        # The empty wireless_html_code sentinel was passed through so the
        # model's own switch heuristic can never override our classifier.
        self.assertEqual(wired.wireless_html_code, "")

    def test_no_table_regions_returns_empty_without_loading_models(self):
        load_count = 0

        def loader(*_args):
            nonlocal load_count
            load_count += 1
            return (FakeTableClsModel("wired_table"), FakeWirelessModel(), FakeWiredModel())

        backend = Mineru345TableBackend(Path("."), Path("mineru.json"), "cpu", loader=loader)
        result = backend(
            TableRecognitionInput(page=page(), table_regions=[], ocr_spans=[], formula_spans=[])
        )
        self.assertEqual(result.tables, [])
        self.assertEqual(load_count, 0)
