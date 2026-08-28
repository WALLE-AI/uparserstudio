import unittest
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from test_recognition_backends import page, region
from uparser_pipeline_server.schemas import FormulaSpan, OcrSpan, Polygon, TableRecognitionInput
from uparser_pipeline_server.table_backend import CurrentMineruSlanetBackend, _structure_tokens


class FakeTableModel:
    def predict(self, image, ocr_result):
        self.image_shape = image.shape
        self.ocr_result = ocr_result
        return SimpleNamespace(
            pred_html='<table><tr><td colspan="2">A</td></tr></table>',
            cell_bboxes=np.array([[0, 0, 60, 0, 60, 30, 0, 30]], dtype=float),
            logic_points=[[0, 0, 0, 1]],
        )


def span(span_id, points, text):
    return OcrSpan(
        span_id=span_id,
        polygon=Polygon(points=points),
        text=text,
        confidence=0.8,
        coordinate_space="render_pixels",
    )


class TableBackendTests(unittest.TestCase):
    def test_structure_tokens_are_parsed_not_split_with_regex(self):
        self.assertEqual(
            _structure_tokens('<table><tr><td rowspan="2">x</td></tr></table>'),
            ["<table>", "<tr>", '<td rowspan="2">', "</td>", "</tr>", "</table>"],
        )

    def test_table_crop_ocr_coordinates_and_cells_map_back_to_page(self):
        fake = FakeTableModel()
        backend = CurrentMineruSlanetBackend(
            Path("slanet.onnx"), Path("reference"), loader=lambda *_: fake
        )
        result = backend(
            TableRecognitionInput(
                page=page(),
                table_regions=[region("table-1", "table", (10, 20, 70, 50))],
                ocr_spans=[
                    span("in", [(20, 25), (30, 25), (30, 35), (20, 35)], "inside"),
                    span("out", [(90, 60), (100, 60), (100, 70), (90, 70)], "outside"),
                ],
                formula_spans=[
                    FormulaSpan(
                        region_id="formula-1",
                        latex="x & y",
                        confidence=0.9,
                        bbox=(35, 25, 45, 35),
                    )
                ],
            )
        )

        table = result.tables[0]
        self.assertEqual(fake.image_shape, (30, 60, 3))
        self.assertEqual(fake.ocr_result[0][0][0], [10.0, 5.0])
        self.assertEqual(fake.ocr_result[1][1], "x &amp; y")
        self.assertEqual(table.cells[0].bbox, (10.0, 20.0, 70.0, 50.0))
        self.assertEqual(table.cells[0].text, "inside")
        self.assertEqual(table.cells[0].column_span, 2)
        self.assertEqual(table.classifier_label, "wireless")


if __name__ == "__main__":
    unittest.main()
