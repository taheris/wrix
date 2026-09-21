#!/usr/bin/env bash
set -euo pipefail

wrix_builder_write_nix_config() {
  local builder_user="$1"
  local output_path="$2"

  cat >"$output_path" <<EOF
experimental-features = nix-command flakes
sandbox = false
build-users-group = nixbld
trusted-users = root $builder_user
max-jobs = auto
cores = 0
min-free = 1073741824
max-free = 3221225472
EOF
}
