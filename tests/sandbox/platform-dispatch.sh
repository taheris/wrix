#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "$(uname -s)" in
  Linux)
    exec "$SCRIPT_DIR/rust-launcher-live.sh" test_linux_archiveless_install_uses_oci_layout "$@"
    ;;
  Darwin)
    exec "$SCRIPT_DIR/image-install-darwin-load.sh" "$@"
    ;;
  *)
    printf 'SKIP: unsupported platform for sandbox dispatch verifier\n' >&2
    exit 77
    ;;
esac
