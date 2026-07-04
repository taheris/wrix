{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  ciApp = app: ''
    local app=${escapeShellArg app}
    local build_dir
    local root
    local runner
    local status
    root="$(repo_root)"
    build_dir="$(mktemp -d -t wrix-verify-ci-app.XXXXXX)"
    nix build --out-link "$build_dir/result" --no-warn-dirty "$root#legacyPackages.${system}.ciApps.$app"
    runner="$(readlink -f "$build_dir/result")"
    status=0
    "$runner/bin/$app" || status="$?"
    rm -rf "$build_dir"
    return "$status"
  '';

  entrypoint = function: ''
    run_repo_script ${escapeShellArg "tests/sandbox/entrypoint-contract.sh"} ${escapeShellArg function}
  '';
in
{
  "images.agent-claude-runtime" = ciApp "test-agent-claude-runtime";
  "images.agent-direct-runner" = ciApp "test-agent-direct-runner";
  "images.agent-exclusive" = ciApp "test-agent-exclusive";
  "images.agent-marker" = ciApp "test-image-agent-marker";
  "images.agent-pkg-threaded" = ciApp "test-agent-pkg-threaded";
  "images.agent-tier-isolated" = ciApp "test-agent-tier-isolated";
  "images.archiveless-generated-change" = ciApp "test-archiveless-generated-change";
  "images.base-hash-stable" = ciApp "test-base-image-hash-stable";
  "images.base-universal" = ciApp "test-base-image-universal";
  "images.ca-certificates" = ciApp "test-image-ca-certificates";
  "images.darwin-entrypoint-core-hooks-path" = entrypoint "test_darwin_core_hooks_path";
  "images.digest-no-tar" = ciApp "test-image-digest-no-tar";
  "images.downstream-change-leaf-only" = ciApp "test-downstream-change-leaf-only";
  "images.entrypoint-command" = ciApp "test-image-entrypoint-command";
  "images.install-digest-skip" = ciApp "test-image-install-digest-skip";
  "images.labels" = ciApp "test-wrix-image-labels";
  "images.linux-archiveless-source" = ciApp "test-linux-image-archiveless-source";
  "images.linux-entrypoint-core-hooks-path" = entrypoint "test_linux_core_hooks_path";
  "images.nix-config" = ciApp "test-image-nix-config";
  "images.nix-db-consistent" = ciApp "test-image-nix-db-consistent";
  "images.nix-db-no-dangling" = ciApp "test-image-nix-db-no-dangling";
  "images.pinned-toolchain-stable-tier" = ciApp "test-pinned-toolchain-stable-tier";
  "images.prek-hooks-closure" = ciApp "test-prek-hooks-closure";
  "images.source-kind" = ciApp "test-wrix-images-source-kind";
  "images.stable-profile-hash-stable" = ciApp "test-stable-profile-hash-stable";
  "images.stable-profile-membership" = ciApp "test-stable-profile-membership";
  "images.tier-graph" = ciApp "test-image-tier-graph";
  "images.tier-membership" = ciApp "test-image-tier-membership";
}
