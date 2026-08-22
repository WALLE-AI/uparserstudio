import importlib.util
import os
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "compare_competitors.py"
SPEC = importlib.util.spec_from_file_location("compare_competitors", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def evaluation(scores, missing=0):
    return {
        "metrics": {"missing_predictions": missing},
        "documents": [
            {
                "document_id": str(index),
                "scores": {
                    "overall": score,
                    "nid": score,
                    "teds": score,
                    "mhs": score,
                },
            }
            for index, score in enumerate(scores)
        ],
    }


class QualityGateTests(unittest.TestCase):
    def test_clear_paired_lead_passes(self):
        candidate = evaluation([0.9] * 30)
        competitor = evaluation([0.8] * 30)

        gate = MODULE.quality_gate(candidate, competitor, 500, 7)

        self.assertEqual(gate["status"], "PASS")
        self.assertTrue(gate["checks"]["overall_lead_at_least_1pct"])

    def test_small_lead_fails_one_percent_requirement(self):
        candidate = evaluation([0.901] * 30)
        competitor = evaluation([0.9] * 30)

        gate = MODULE.quality_gate(candidate, competitor, 500, 7)

        self.assertEqual(gate["status"], "FAIL")
        self.assertFalse(gate["checks"]["overall_lead_at_least_1pct"])

    def test_missing_candidate_prediction_fails(self):
        candidate = evaluation([0.9] * 30, missing=1)
        competitor = evaluation([0.8] * 30)

        gate = MODULE.quality_gate(candidate, competitor, 500, 7)

        self.assertEqual(gate["status"], "FAIL")
        self.assertFalse(gate["checks"]["candidate_has_no_missing_predictions"])

    def test_no_paired_metric_is_insufficient(self):
        candidate = evaluation([])
        competitor = evaluation([])

        gate = MODULE.quality_gate(candidate, competitor, 500, 7)

        self.assertEqual(gate["status"], "INSUFFICIENT")

    def test_office_primary_metrics_are_evaluated_without_pdf_fields(self):
        candidate = {
            "primary_metrics": ["overall", "text", "headings", "tables", "links"],
            "metrics": {"missing_predictions": 0},
            "documents": [
                {
                    "document_id": str(index),
                    "scores": {
                        "overall": 0.9,
                        "text": 0.9,
                        "headings": 0.9,
                        "tables": 0.9,
                        "links": 0.9,
                    },
                }
                for index in range(30)
            ],
        }
        competitor = {
            "primary_metrics": candidate["primary_metrics"],
            "metrics": {"missing_predictions": 0},
            "documents": [
                {
                    "document_id": str(index),
                    "scores": {
                        "overall": 0.8,
                        "text": 0.8,
                        "headings": 0.8,
                        "tables": 0.8,
                        "links": 0.8,
                    },
                }
                for index in range(30)
            ],
        }

        gate = MODULE.quality_gate(candidate, competitor, 500, 7)

        self.assertEqual(gate["status"], "PASS")
        self.assertEqual(set(gate["metrics"]), set(candidate["primary_metrics"]))

    def test_mismatched_primary_metrics_are_rejected(self):
        candidate = evaluation([0.9] * 30)
        competitor = evaluation([0.8] * 30)
        candidate["primary_metrics"] = ["overall", "text"]
        competitor["primary_metrics"] = ["overall", "tables"]

        with self.assertRaisesRegex(ValueError, "different primary_metrics"):
            MODULE.quality_gate(candidate, competitor, 500, 7)


class PerformanceGateTests(unittest.TestCase):
    def test_candidate_matches_suite_output_channel(self):
        engine = next(engine for engine in MODULE.ENGINES if engine.name == "uparser-native")
        output = Path("result.md")

        pdf_command = engine.command(Path("input.pdf"), output)
        office_command = engine.command(Path("input.docx"), output)

        self.assertTrue(str(engine.binary).endswith("uparser.exe"))
        self.assertIn("--mode", pdf_command)
        self.assertNotIn("--output", pdf_command)
        self.assertEqual(office_command[-2:], ["--output", str(output)])

    def test_stable_ten_percent_paired_lead_passes(self):
        runs = []
        for round_index in range(7):
            for source_index in range(10):
                source = f"document-{source_index}"
                runs.extend(
                    [
                        {
                            "engine": "uparser-native",
                            "source": source,
                            "round": round_index,
                            "success": True,
                            "elapsed_ms": 8.0,
                        },
                        {
                            "engine": "competitor",
                            "source": source,
                            "round": round_index,
                            "success": True,
                            "elapsed_ms": 10.0,
                        },
                    ]
                )
        candidate = {
            "success_rate": 1.0,
            "elapsed_ms": {"median": 8.0, "p95": 8.0},
            "throughput_docs_per_second": 125.0,
            "peak_rss_bytes": 100,
        }
        competitor = {
            "success_rate": 1.0,
            "elapsed_ms": {"median": 10.0, "p95": 10.0},
            "throughput_docs_per_second": 100.0,
            "peak_rss_bytes": 200,
        }

        gate = MODULE.performance_gate(
            candidate, competitor, runs, "competitor", 500, 7
        )

        self.assertEqual(gate["status"], "PASS")
        self.assertEqual(gate["paired_samples"], 70)

    def test_missing_rss_is_insufficient(self):
        candidate = {
            "success_rate": 1.0,
            "elapsed_ms": {"median": 8.0, "p95": 8.0},
            "throughput_docs_per_second": 125.0,
            "peak_rss_bytes": None,
        }
        competitor = {
            "success_rate": 1.0,
            "elapsed_ms": {"median": 10.0, "p95": 10.0},
            "throughput_docs_per_second": 100.0,
            "peak_rss_bytes": 200,
        }

        gate = MODULE.performance_gate(
            candidate, competitor, [], "competitor", 10, 7
        )

        self.assertEqual(gate["status"], "INSUFFICIENT")


@unittest.skipUnless(os.name == "nt", "Windows RSS fallback")
class WindowsRssTests(unittest.TestCase):
    def test_reads_current_process_working_set(self):
        rss = MODULE._windows_process_rss(os.getpid())
        self.assertIsNotNone(rss)
        self.assertGreater(rss, 0)


if __name__ == "__main__":
    unittest.main()
