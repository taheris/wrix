#!/usr/bin/env bash
set -euo pipefail

workspace="$1"
member="$2"
status="$3"
printf 'execution-probe:%s\n' "$member"
printf 'execution-probe-stderr:%s\n' "$member" >&2
touch "$workspace/ready-$member"
deadline=$((SECONDS + 120))
while [[ ! -f "$workspace/exit-$member" ]]; do
  if [[ "$SECONDS" -ge "$deadline" ]]; then
    printf 'execution probe: exit gate timed out\n' >&2
    exit 124
  fi
  sleep 0.05
done
exit "$status"
