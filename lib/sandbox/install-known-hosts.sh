#!/usr/bin/env bash
set -euo pipefail

known_hosts_source="${1:?known_hosts source path is required}"
root="${2:-.}"

if [[ ! -f "$known_hosts_source" ]]; then
  echo "wrix: known_hosts source not found: $known_hosts_source" >&2
  exit 1
fi
if [[ -e "$root/etc/ssh" && ! -d "$root/etc/ssh" ]]; then
  echo "wrix: cannot install known_hosts because $root/etc/ssh is not a directory" >&2
  exit 1
fi

install -d -m 0755 "$root/etc/wrix/known_hosts_dir"
install -m 0644 "$known_hosts_source" "$root/etc/wrix/known_hosts_dir/known_hosts"

if [[ ! -e "$root/etc/ssh" ]]; then
  install -d -m 0755 "$root/etc/ssh"
  install -m 0644 "$known_hosts_source" "$root/etc/ssh/ssh_known_hosts"
fi
