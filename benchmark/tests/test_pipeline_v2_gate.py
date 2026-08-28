import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "pipeline_v2_gate", ROOT / "benchmark/check_pipeline_v2_gate.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def odl(overall=0.86, nid=0.88, teds=0.92, mhs=0.80):
    return {
        "metrics": {
            "score": {
                "overall_mean": overall,
                "nid_mean": nid,
                "teds_mean": teds,
                "mhs_mean": mhs,
            }
        }
    }


def omni(text=0.05, formula=0.89, table=0.83, table_s=0.89, order=0.14):
    return {
        "text_block": {"page": {"Edit_dist": {"ALL": text}}},
        "display_formula": {"page": {"CDM": {"ALL": formula}}},
        "table": {
            "page": {
                "TEDS": {"ALL": table},
                "TEDS_structure_only": {"ALL": table_s},
            }
        },
        "reading_order": {"page": {"Edit_dist": {"ALL": order}}},
    }


class PipelineV2GateTests(unittest.TestCase):
    def test_pareto_improvement_passes(self):
        result = MODULE.evaluate(
            odl(0.87, 0.89, 0.93, 0.81),
            odl(),
            omni(0.04, 0.90, 0.84, 0.90, 0.13),
            omni(),
        )

        self.assertEqual(result["status"], "PASS")

    def test_overall_gain_cannot_hide_table_regression(self):
        result = MODULE.evaluate(
            odl(0.87, 0.95, 0.91, 0.90),
            odl(),
            omni(0.03, 0.95, 0.82, 0.90, 0.12),
            omni(),
        )

        self.assertEqual(result["status"], "FAIL")
        self.assertFalse(result["opendataloader_bench"]["checks"]["teds"]["passed"])
        self.assertFalse(result["omnidocbench"]["checks"]["table_teds"]["passed"])

    def test_omni_overall_uses_leaderboard_page_aggregates(self):
        metrics = MODULE.omni_metrics(omni(text=0.05, formula=0.90, table=0.80))

        self.assertAlmostEqual(metrics["overall"], 88.3333333333)


if __name__ == "__main__":
    unittest.main()
