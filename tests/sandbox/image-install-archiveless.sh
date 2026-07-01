#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/rust-launcher-live.sh" test_linux_archiveless_install_uses_oci_layout "$@"
