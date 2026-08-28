import base64
import io
import unittest
from pathlib import Path

import numpy as np
from PIL import Image

from uparser_pipeline_server.recognition_backends import (
    CurrentMineruMfrBackend,
    LegacyOcrBackend,
    mask_formula_regions,
)
from uparser_pipeline_server.schemas import (
    EncodedImage,
    FormulaRecognitionInput,
    ImageDimensions,
    OcrPageInput,
    PageImage,
    Region,
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


class FakeOcr:
    def __init__(self):
        self.inputs = []

    def ocr(self, image, **kwargs):
        self.inputs.append((image.copy(), kwargs))
        return [[
            [[[50, 50], [70, 50], [70, 60], [50, 60]], ("kept", 0.91)],
            [[[50, 65], [70, 65], [70, 75], [50, 75]], ("low", 0.2)],
        ]]


class FakeMfr:
    def predict(self, items, image, batch_size):
        self.batch_size = batch_size
        self.shape = image.shape
        return [dict(item, latex=f"latex-{index}") for index, item in enumerate(items)]


class RecognitionBackendTests(unittest.TestCase):
    def test_formula_mask_clips_and_does_not_mutate_input(self):
        image = np.zeros((10, 10, 3), dtype=np.uint8)
        masked = mask_formula_regions(
            image, [region("f", "inline_formula", (-2.2, 1.2, 4.1, 6.8))]
        )

        self.assertTrue(np.all(masked[1:7, 0:5] == 255))
        self.assertTrue(np.all(image == 0))

    def test_ocr_masks_formula_filters_confidence_and_restores_page_coordinates(self):
        fake = FakeOcr()
        backend = LegacyOcrBackend(
            Path("models"), "cpu", loader=lambda *_: fake, padding=50
        )
        result = backend(
            OcrPageInput(
                page=page(),
                layout_regions=[region("text-1", "text", (10, 20, 90, 70))],
                formula_regions=[region("formula-1", "inline_formula", (20, 30, 30, 40))],
                language="ch",
            )
        )

        self.assertEqual(len(result.spans), 1)
        self.assertEqual(result.spans[0].text, "kept")
        self.assertEqual(result.spans[0].parent_region_id, "text-1")
        self.assertEqual(result.spans[0].polygon.points[0], (10.0, 20.0))
        submitted, kwargs = fake.inputs[0]
        self.assertTrue(np.all(submitted[60:70, 60:70] == 255))
        self.assertEqual(kwargs["mfd_res"][0]["bbox"], [60.0, 60.0, 70.0, 70.0])

    def test_ocr_skips_non_text_layout_regions(self):
        fake = FakeOcr()
        backend = LegacyOcrBackend(Path("models"), "cpu", loader=lambda *_: fake)
        result = backend(
            OcrPageInput(
                page=page(),
                layout_regions=[region("image-1", "image", (10, 10, 80, 60))],
                language="ch",
            )
        )

        self.assertEqual(result.spans, [])
        self.assertEqual(fake.inputs, [])

    def test_mfr_preserves_region_identity_and_uses_current_predict_contract(self):
        fake = FakeMfr()
        backend = CurrentMineruMfrBackend(
            Path("weights"), Path("reference"), "cpu", loader=lambda *_: fake, batch_size=8
        )
        result = backend(
            FormulaRecognitionInput(
                page=page(),
                formula_regions=[
                    region("f-1", "inline_formula", (1, 2, 20, 12)),
                    region("f-2", "display_formula", (4, 20, 80, 40), 0.7),
                ],
            )
        )

        self.assertEqual([span.region_id for span in result.spans], ["f-1", "f-2"])
        self.assertEqual([span.latex for span in result.spans], ["latex-0", "latex-1"])
        self.assertEqual(result.spans[0].bbox, (1.0, 2.0, 20.0, 12.0))
        self.assertEqual(fake.batch_size, 8)


if __name__ == "__main__":
    unittest.main()
