"""Run the fixture-owned external engine with its caller's captured environment."""
import json
import os
from pathlib import Path
import sys

configuration = Path(os.environ["WRIX_TEST_BACKEND_ENV"])
with configuration.open() as stream:
    values = json.load(stream)
program = configuration.parent / "bin" / Path(sys.argv[0]).name
if program.name not in ("container", "podman") or not program.is_file():
    raise SystemExit("missing owned external runtime fixture")
environment = dict(os.environ)
environment.update(values)
os.execve(program, [str(program), *sys.argv[1:]], environment)
