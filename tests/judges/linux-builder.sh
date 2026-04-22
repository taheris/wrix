#!/usr/bin/env bash
# Judge rubrics for linux-builder.md success criteria

test_ssh_key_auth() {
  judge_files "lib/builder/default.nix"
  judge_criterion "SSH connection to the builder uses key-based authentication (no passwords) for security"
}

test_nix_darwin_config() {
  judge_files "lib/builder/default.nix"
  judge_criterion "The code provides a 'config' subcommand that prints a nix-darwin configuration snippet for the user to manually add to their nix-darwin config. A full nix-darwin module that automatically enables permanent setup is NOT yet implemented — only the helper snippet printer exists. PASS if the config subcommand exists and prints a nix-darwin snippet; the absence of a full nix-darwin module is expected."
}

test_aarch64_linux_builds() {
  judge_files "lib/builder/default.nix" "lib/builder/image.nix"
  judge_criterion "The builder exposes an aarch64-linux nix-daemon over SSH on localhost:2222, suitable for use as a nix remote builder from a macOS host. PASS if the container image configures nix-daemon for aarch64-linux with SSH access and the helper script wires up the SSH endpoint."
}

test_nix_store_persistence() {
  judge_files "lib/builder/default.nix" "lib/builder/image.nix"
  judge_criterion "The /nix store directory is persisted across container restarts via a named volume or bind mount, not as an ephemeral layer. PASS if the code mounts /nix from a persistent location (volume or host path) rather than leaving it in the container's writable layer."
}

test_stop_restart_clean() {
  judge_files "lib/builder/default.nix"
  judge_criterion "The helper script supports stop and start subcommands (or equivalent) that leave the persistent Nix store and SSH keys intact across restarts, so repeated cycles converge on the same working builder without leaking state or requiring re-initialization."
}
