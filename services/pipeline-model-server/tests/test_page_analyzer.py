import unittest
from pathlib import Path

from test_recognition_backends import page, region
from uparser_pipeline_server.page_analyzer import LayoutReader, PipelinePageAnalyzer
from uparser_pipeline_server.registry import BackendRegistry, RegisteredBackend
from uparser_pipeline_server.schemas import (
    FormulaDetectionResult,
    FormulaRecognitionResult,
    FormulaSpan,
    LayoutResult,
    ModelMetadata,
    OcrResult,
    OcrSpan,
    PageAnalyzeInput,
    Polygon,
    TableRecognitionResult,
)


def backend(infer):
    return RegisteredBackend(
        infer=infer,
        metadata=ModelMetadata(name="fake", revision="test", runtime="test"),
    )


class PageAnalyzerTests(unittest.TestCase):
    def test_layoutreader_scales_boxes_and_validates_permutation(self):
        captured = []
        reader = LayoutReader(
            Path("weights"),
            "cpu",
            loader=lambda *_: object(),
            predictor=lambda boxes, _model: captured.extend(boxes) or [1, 0],
        )
        regions = [
            region("a", "text", (0, 0, 50, 100)),
            region("b", "text", (50, 100, 100, 200)),
        ]

        self.assertEqual(reader(regions, 100, 200), ["b", "a"])
        self.assertEqual(captured, [[0, 0, 500, 500], [500, 500, 1000, 1000]])

    def test_page_analyzer_honors_formula_and_table_flags(self):
        registry = BackendRegistry()
        calls = []
        layout_regions = [region("text", "text", (0, 0, 80, 30))]
        registry.register("layout", backend(lambda _: LayoutResult(regions=layout_regions)))
        registry.register(
            "formula_detect",
            backend(lambda _: calls.append("mfd") or FormulaDetectionResult(regions=[])),
        )
        registry.register("ocr", backend(lambda _: OcrResult(spans=[])))
        registry.register(
            "formula_recognize",
            backend(lambda _: calls.append("mfr") or FormulaRecognitionResult(spans=[])),
        )
        registry.register(
            "table", backend(lambda _: calls.append("table") or TableRecognitionResult(tables=[]))
        )
        analyzer = PipelinePageAnalyzer(
            registry,
            LayoutReader(
                Path("weights"), "cpu", loader=lambda *_: object(), predictor=lambda *_: [0]
            ),
        )

        result = analyzer(
            PageAnalyzeInput(
                page=page(), language="ch", formula_enabled=False, table_enabled=False
            )
        )

        self.assertEqual(calls, [])
        self.assertEqual(result.reading_order, ["text"])
        self.assertEqual(result.formula_spans, [])
        self.assertEqual(result.tables, [])


if __name__ == "__main__":
    unittest.main()
