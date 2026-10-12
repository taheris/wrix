#!/usr/bin/env bash
set -euo pipefail

known_hosts_source="${1:?known_hosts source path is required}"
root="${2:-.}"
mode="${3:-build}"
if [[ "$mode" != "build" && "$mode" != "runtime" ]]; then
  echo "wrix: invalid known_hosts installation mode: $mode" >&2
  exit 1
fi

if [[ ! -f "$known_hosts_source" ]]; then
  echo "wrix: known_hosts source not found: $known_hosts_source" >&2
  exit 1
fi
if [[ -e "$root/etc/ssh" && ! -d "$root/etc/ssh" ]]; then
  echo "wrix: cannot install known_hosts because $root/etc/ssh is not a directory" >&2
  exit 1
fi

if [[ "$mode" == "runtime" ]]; then
  if [[ -L "$root/etc/ssh" ]]; then
    private_ssh_dir=$(mktemp -d "$root/etc/.wrix-ssh.XXXXXX")
    cp -rL --preserve=mode "$root/etc/ssh/." "$private_ssh_dir/"
    chmod 0755 "$private_ssh_dir"
    rm "$root/etc/ssh"
    mv "$private_ssh_dir" "$root/etc/ssh"
  fi
  install -d -m 0755 "$root/etc/ssh"
  rm -f "$root/etc/ssh/ssh_known_hosts"
  install -m 0644 "$known_hosts_source" "$root/etc/ssh/ssh_known_hosts"
  exit 0
fi

install -d -m 0755 "$root/etc/wrix/known_hosts_dir"
install -m 0644 "$known_hosts_source" "$root/etc/wrix/known_hosts_dir/known_hosts"

if [[ ! -e "$root/etc/ssh" ]]; then
  install -d -m 0755 "$root/etc/ssh"
  install -m 0644 "$known_hosts_source" "$root/etc/ssh/ssh_known_hosts"
fi
