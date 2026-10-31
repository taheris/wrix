#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --realise && "${2:-}" == "${WRIX_TEST_IMAGE_DERIVATION:-}" && -f "${WRIX_TEST_FAIL_IMAGE_REALISATION:-}" ]]; then
  printf 'realise\n' >>"$WRIX_TEST_FAIL_IMAGE_REALISATION"
  printf 'fixture image realisation interrupted\n' >&2
  exit 75
fi
