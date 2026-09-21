#!/bin/bash
set -euo pipefail

# Run before any setup command or exit trap, including on direct invocation.
if [[ ! -f /run/wrix-network-ready || -L /run/wrix-network-ready ]]; then
  echo "Error: network bootstrap did not complete" >&2
  exit 1
fi

wrix_assert_net_admin_absent() {
  local field value low seen=0
  while read -r field value _rest; do
    case "$field" in
      CapInh:|CapPrm:|CapEff:|CapBnd:|CapAmb:)
        [[ "$value" =~ ^[0-9A-Fa-f]+$ ]] || {
          echo "Error: invalid Linux capability state for $field" >&2
          exit 1
        }
        low="${value: -8}"
        if (( (16#$low & 16#1000) != 0 )); then
          echo "Error: NET_ADMIN survived the network bootstrap ($field)" >&2
          exit 1
        fi
        seen=$((seen + 1))
        ;;
    esac
  done < /proc/self/status
  if [[ "$seen" -ne 5 ]]; then
    echo "Error: Linux capability state could not be verified" >&2
    exit 1
  fi
}

wrix_assert_net_admin_absent
