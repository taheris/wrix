"""Check actual constructed root farms, not an evaluator path-list model."""

import json
import os
from pathlib import Path
import sys

cases = json.loads(Path(sys.argv[1]).read_text())
for case in cases:
    if case["root"] is None:
        assert case["expected"] == []
        continue
    directory = Path(case["root"]) / "share/wrix/image-inputs"
    observed = sorted(os.readlink(path) for path in directory.iterdir())
    assert observed == sorted(case["expected"]), (observed, case["expected"])
print(f"{len(cases)} native configuration-reference cases passed")
