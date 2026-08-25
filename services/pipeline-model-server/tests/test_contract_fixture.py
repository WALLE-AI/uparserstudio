import json
import unittest
from pathlib import Path

from uparser_pipeline_server.schemas import PageAnalyzeBatchRequest


ROOT = Path(__file__).resolve().parents[3]


class ContractFixtureTests(unittest.TestCase):
    def test_page_analyze_request_fixture_roundtrips(self):
        payload = json.loads(
            (ROOT / "pipeline/fixtures/page-analyze-request.json").read_text(encoding="utf-8")
        )
        request = PageAnalyzeBatchRequest.model_validate(payload)

        self.assertEqual(request.model_dump(mode="json"), payload)


if __name__ == "__main__":
    unittest.main()
