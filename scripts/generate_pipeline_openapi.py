#!/usr/bin/env python3
"""Generate the checked-in Pipeline V2 OpenAPI contract."""

from __future__ import annotations

import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "services" / "pipeline-model-server" / "src"))

from uparser_pipeline_server.app import create_app  # noqa: E402


def main() -> int:
    output = ROOT / "pipeline" / "openapi-v2.json"
    output.write_text(
        json.dumps(create_app().openapi(), ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
