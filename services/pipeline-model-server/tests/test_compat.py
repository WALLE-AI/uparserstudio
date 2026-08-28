import unittest

from uparser_pipeline_server.compat import (
    LegacyDetection,
    merge_layout_and_mfd,
    normalize_layout,
    normalize_mfd,
    poly_to_bbox,
)


class LegacyCompatibilityTests(unittest.TestCase):
    def test_doclayout_categories_map_to_current_mineru_labels(self):
        detections = [
            LegacyDetection(0, (0, 0, 10, 10), 0.9),
            LegacyDetection(1, (0, 10, 10, 20), 0.8),
            LegacyDetection(5, (0, 20, 10, 30), 0.7),
        ]

        regions = normalize_layout("page-1", detections)

        self.assertEqual([region.label for region in regions], ["paragraph_title", "text", "table"])
        self.assertEqual(regions[0].coordinate_space, "render_pixels")
        self.assertEqual(regions[0].region_id, "page-1/layout-0")

    def test_mfd_embedding_and_isolated_labels_are_preserved(self):
        regions = normalize_mfd(
            "page-1",
            [
                LegacyDetection(0, (1, 1, 4, 4), 0.8),
                LegacyDetection(1, (5, 5, 9, 9), 0.9),
            ],
        )

        self.assertEqual([region.label for region in regions], ["inline_formula", "display_formula"])

    def test_mfd_wins_over_overlapping_layout_formula(self):
        layout = normalize_layout("page-1", [LegacyDetection(8, (0, 0, 100, 20), 0.6)])
        mfd = normalize_mfd("page-1", [LegacyDetection(1, (1, 1, 99, 19), 0.95)])

        merged = merge_layout_and_mfd(layout, mfd)

        self.assertEqual(len(merged), 1)
        self.assertEqual(merged[0].region_id, "page-1/mfd-0")
        self.assertEqual(merged[0].confidence, 0.95)

    def test_non_overlapping_layout_formula_is_retained(self):
        layout = normalize_layout("page-1", [LegacyDetection(8, (0, 0, 10, 10), 0.6)])
        mfd = normalize_mfd("page-1", [LegacyDetection(1, (20, 20, 30, 30), 0.9)])

        self.assertEqual(len(merge_layout_and_mfd(layout, mfd)), 2)

    def test_legacy_polygon_converts_to_axis_aligned_bbox(self):
        self.assertEqual(poly_to_bbox([1, 2, 4, 1, 5, 8, 0, 7]), (0.0, 1.0, 5.0, 8.0))


if __name__ == "__main__":
    unittest.main()
