"""Resolve runtime store roots using the declared native input closure only."""

import json
from pathlib import Path
import re
import sys

STORE_PATH = re.compile(r"/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[-a-zA-Z0-9+._?=]+")


def resolve(candidates, closure):
    if not isinstance(candidates, list) or not all(isinstance(item, str) for item in candidates):
        raise ValueError("configuration references must be a string list")
    paths = set(closure)
    if not all(STORE_PATH.fullmatch(path) for path in paths):
        raise ValueError("invalid native input-closure path")
    selected = set()
    for candidate in candidates:
        if not STORE_PATH.fullmatch(candidate):
            raise ValueError("invalid contextual store token")
        matches = [path for path in paths if candidate.startswith(path)]
        if not matches:
            raise ValueError(f"configuration reference is outside its input closure: {candidate}")
        # Preserve native names containing '?' while also handling annotation
        # suffixes. The closure supplies real names; no drv-name/hash guessing.
        selected.add(max(matches, key=len))
    return sorted(selected)


def main():
    candidates = json.loads(Path(sys.argv[1]).read_text())
    closure = Path(sys.argv[2]).read_text().splitlines()
    roots = set(resolve(candidates, closure))
    sources = json.loads(Path(sys.argv[4]).read_text())
    if not isinstance(sources, list) or not all(isinstance(path, str) and STORE_PATH.fullmatch(path) for path in sources):
        raise ValueError("invalid native source roots")
    for source in sources:
        # Nix supplies these exact opaque context paths as declared inputs.
        # Never follow a source symlink to establish its presence.
        Path(source).lstat()
        roots.add(source)
    destination = Path(sys.argv[3]) / "share/wrix/image-inputs"
    destination.mkdir(parents=True)
    for root in sorted(roots):
        (destination / Path(root).name).symlink_to(root)


if __name__ == "__main__":
    main()
