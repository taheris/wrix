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
# Ancestor symlinks must not redirect writes outside the image.
for path in etc etc/wrix etc/wrix/known_hosts_dir; do
  if [[ -L "$root/$path" ]]; then
    echo "wrix: refusing known_hosts installation through symlink: $root/$path" >&2
    exit 1
  fi
done
if [[ ( -e "$root/etc/ssh" || -L "$root/etc/ssh" ) && ! -d "$root/etc/ssh" ]]; then
  echo "wrix: cannot install known_hosts because $root/etc/ssh is not a directory" >&2
  exit 1
fi

if [[ "$mode" == "build" ]]; then
  install -d -m 0755 "$root/etc/wrix/known_hosts_dir"
  rm -f "$root/etc/wrix/known_hosts_dir/known_hosts"
  install -m 0644 "$known_hosts_source" "$root/etc/wrix/known_hosts_dir/known_hosts"
  if [[ -d "$root/etc/ssh" && ! -L "$root/etc/ssh" ]]; then
    exit 0
  fi
fi

if [[ -L "$root/etc/ssh" ]]; then
  private_ssh_dir=$(mktemp -d "$root/etc/.wrix-ssh.XXXXXX")
  trap 'rm -rf -- "$private_ssh_dir"' EXIT
  cp -rL --preserve=mode "$root/etc/ssh/." "$private_ssh_dir/"
  chmod 0755 "$private_ssh_dir"
  rm "$root/etc/ssh"
  mv "$private_ssh_dir" "$root/etc/ssh"
  trap - EXIT
fi
install -d -m 0755 "$root/etc/ssh"
rm -f "$root/etc/ssh/ssh_known_hosts"
install -m 0644 "$known_hosts_source" "$root/etc/ssh/ssh_known_hosts"
