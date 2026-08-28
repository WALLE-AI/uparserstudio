import base64
import io
import os
import unittest

from PIL import Image

from uparser_pipeline_server.legacy_backends import (
    LegacyLayoutBackend,
    _decode_page,
    _detector_device,
)
from uparser_pipeline_server.schemas import EncodedImage, ImageDimensions, PageImage


def page(width=8, height=6):
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


class FakeLayoutModel:
    def predict(self, image):
        return [
            {
                "category_id": 1,
                "poly": [0, 0, image.width, 0, image.width, image.height, 0, image.height],
                "score": 0.75,
            }
        ]


class LegacyBackendTests(unittest.TestCase):
    def test_cuda_detector_device_is_torch_device_to_preserve_visibility_mask(self):
        import torch

        self.assertEqual(_detector_device("cuda"), torch.device("cuda:0"))
        self.assertEqual(_detector_device("cpu"), "cpu")

    def test_layout_backend_decodes_and_normalizes_real_image_input(self):
        load_count = 0

        def loader(_weight, _device):
            nonlocal load_count
            load_count += 1
            return FakeLayoutModel()

        backend = LegacyLayoutBackend("fixture.pt", "cpu", loader=loader)

        first = backend(page())
        second = backend(page())

        self.assertEqual(load_count, 1)
        self.assertEqual(first.regions[0].label, "text")
        self.assertEqual(first.regions[0].bbox, (0.0, 0.0, 8.0, 6.0))
        self.assertEqual(second.regions[0].region_id, "page-1/layout-0")

    def test_declared_image_dimensions_must_match_payload(self):
        invalid = page()
        invalid.dimensions.width = 9

        with self.assertRaisesRegex(ValueError, "do not match"):
            _decode_page(invalid)

    def test_declared_pixel_limit_is_enforced_before_decode(self):
        invalid = page()
        invalid.dimensions.width = 9
        previous = os.environ.get("UPARSER_PIPELINE_MAX_IMAGE_PIXELS")
        os.environ["UPARSER_PIPELINE_MAX_IMAGE_PIXELS"] = "10"
        try:
            with self.assertRaisesRegex(ValueError, "exceeds pixel limit"):
                _decode_page(invalid)
        finally:
            if previous is None:
                os.environ.pop("UPARSER_PIPELINE_MAX_IMAGE_PIXELS", None)
            else:
                os.environ["UPARSER_PIPELINE_MAX_IMAGE_PIXELS"] = previous


if __name__ == "__main__":
    unittest.main()
