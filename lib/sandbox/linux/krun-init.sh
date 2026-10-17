#!/bin/bash
set -euo pipefail

# The relay supplies the PTY; argv decoding belongs to stage two.
export WRIX_KRUN_INIT=1
exec /network-bootstrap.sh
