#!/usr/bin/env python3
"""Hash Skopeo's typed OCI config with its insignificant indentation removed."""

import hashlib
import json
import re
import sys


def digest(config: str) -> str:
    json.loads(config)
    compact = re.sub(r'("(?:[^"\\]|\\.)*")|\s+', lambda match: match[1] or "", config)
    return "sha256:" + hashlib.sha256(compact.encode("utf-8")).hexdigest()


if __name__ == "__main__":
    print(digest(sys.stdin.read()))
