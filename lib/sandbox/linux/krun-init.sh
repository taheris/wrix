#!/bin/bash
set -euo pipefail

# The relay supplies the PTY; argv decoding and UID spoofing belong to stage two.
export WRIX_KRUN_INIT=1
exec /network-bootstrap.sh
