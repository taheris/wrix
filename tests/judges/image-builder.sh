#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for image-builder.md success criteria

test_oci_format() {
  judge_files "lib/sandbox/image.nix"
  judge_criterion "Generated images conform to valid OCI format specification"
}

test_profile_packages() {
  judge_files "lib/sandbox/image.nix" "lib/sandbox/profiles.nix"
  judge_criterion "All packages specified in the selected profile are available inside the built container image"
}

test_claude_code_starts() {
  judge_files "lib/sandbox/image.nix" "lib/sandbox/default.nix"
  judge_criterion "Claude Code binary is present and starts correctly inside the container (note: entrypointPkg is set to linuxPkgs.claude-code in default.nix and included in allPackages in image.nix)"
}

test_nix_in_container() {
  judge_files "lib/sandbox/image.nix"
  judge_criterion "Nix commands (nix build, nix develop, etc.) work inside the container with a functional nix store"
}
