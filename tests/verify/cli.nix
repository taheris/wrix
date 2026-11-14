{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;
  verifyTools = import ../../lib/verify/tools.nix { inherit pkgs; };
  browserScopeTools = import ../prek/browser-scope-tools.nix { inherit pkgs; };
  # Retain the existing store source without coercing/copying its path again.
  nixpkgsSource =
    let
      path = toString pkgs.path;
    in
    builtins.appendContext path { "${path}".path = true; };
in
{
  "cli.package-surface".script = ''
    local package
    local forbidden
    local repo_beads_bin
    package="$(build_flake_package wrix)"
    assert_executable "$package/bin/wrix"
    assert_executable "$package/bin/wrix-prek"
    for forbidden in beads-dolt beads-push wrix-svc; do
      if [[ -e "$package/bin/$forbidden" ]]; then
        fail "wrix package exposes forbidden binary $forbidden"
      fi
      assert_package_attr_absent "$forbidden"
    done
    repo_beads_bin="$(find "$package/bin" -maxdepth 1 -type f -name '*-beads' -print -quit)"
    if [[ -n "$repo_beads_bin" ]]; then
      fail "wrix package exposes forbidden repo-beads binary $repo_beads_bin"
    fi
  '';

  "cli.shared-verifier-app".script = ''
    local list_output
    local batch_output
    local verdict_count
    list_output="$("$SELF" --list)"
    assert_contains "verify list" "$list_output" "verify:cli.package-surface"
    assert_contains "verify list" "$list_output" "verify:cli.shared-verifier-app"
    assert_contains "verify list" "$list_output" "verify:cli.verify-runner-batching"

    batch_output="$("$SELF" cli.verify-runner-batching verify:cli.verify-runner-batching)"
    assert_json_verdict "batched verifier" "$batch_output" "cli.verify-runner-batching"
    verdict_count="$(printf '%s\n' "$batch_output" | jq -rs '[.[] | select(.target == "cli.verify-runner-batching" and .outcome == "passed")] | length')"
    if [[ "$verdict_count" -ne 2 ]]; then
      fail "batched verifier emitted $verdict_count passing verdicts; expected 2"
    fi

    local root loom test_ci
    root="$(repo_root)"
    nix build --no-link --no-warn-dirty "$root#checks.${system}.verifier-inputs"
    loom="$(build_flake_package loom)"
    nix run --no-warn-dirty "$root#test-ci" -- --list >/dev/null
    test_ci="$(nix eval --raw --no-warn-dirty "$root#apps.${system}.test-ci.program")"
    (
      local snapshot database caller_cache
      snapshot="$(mktemp -d)"
      trap 'rm -rf "$snapshot"' EXIT
      # Nix's pinned-source metadata is not an admission/verdict cache. Take
      # consistent read-only backups before nextest isolates HOME; each Rust
      # fixture copies standalone databases into its private cache. A suite-owned
      # Git object cache avoids repeated network fetches; it never links back to
      # caller files. Do not copy configuration, Wrix state, or live databases.
      caller_cache="''${XDG_CACHE_HOME:-$HOME/.cache}/nix"
      if [[ -d "$caller_cache/gitv3" ]]; then
        ${pkgs.coreutils}/bin/cp --recursive --reflink=auto --preserve=mode,timestamps \
          "$caller_cache/gitv3" "$snapshot/gitv3"
        if [[ -n "$(${pkgs.findutils}/bin/find "$snapshot/gitv3" -type l -print -quit)" ]] ||
          [[ -n "$(${pkgs.findutils}/bin/find "$snapshot/gitv3" -path '*/objects/info/alternates' -type f -size +0c -print -quit)" ]]; then
          fail "Nix source-cache snapshot must not reference external mutable objects"
        fi
      fi
      for database in "$caller_cache"/fetcher-cache-v[0-9]*.sqlite; do
        [[ -f "$database" ]] || continue
        ${pkgs.sqlite}/bin/sqlite3 -readonly "$database" ".backup '$snapshot/''${database##*/}'"
      done
      export WRIX_TEST_FETCHER_CACHE_SNAPSHOT="$snapshot"
      export WRIX_TEST_VERIFY="$SELF"
      export WRIX_TEST_LOOM="$loom/bin/loom"
      export WRIX_TEST_VERIFIER_LIBRARY=${../lib/verifier.sh}
      export WRIX_TEST_BUILDER_SUITE=${../standalone/builder-test.sh}
      export WRIX_TEST_CI="$test_ci"
      export WRIX_TEST_SOURCE="$root"
      export WRIX_TEST_NIXPKGS=${escapeShellArg nixpkgsSource}
      export WRIX_TEST_SYSTEM=${escapeShellArg system}
      cargo nextest run --offline --locked --manifest-path "$root/Cargo.toml" \
        --package wrix-verify-tools --test verifier_results --test verifier_inputs \
        --run-ignored only --test-threads 4 --no-fail-fast
    )
  '';

  "cli.browser-gate-scope" = {
    inherit (import ../lib/browser-resources.nix { }) inputs;
    platforms = (import ../lib/verifier.nix).linux;
    script = ''
      local root loom
      export PATH="${pkgs.procps}/bin:$PATH"
      root="$(repo_root)"
      loom="$(build_flake_package loom)"
      export WRIX_TEST_BROWSER_SOURCE="$root"
      export WRIX_TEST_BROWSER_LOOM="$loom/bin/loom"
      export WRIX_TEST_BROWSER_SYSTEM=${escapeShellArg system}
      ${browserScopeTools}/bin/browser-gate-scope-tests --include-ignored --test-threads=1
    '';
  };

  "cli.verify-runner-batching".script = ''
    local root
    local list_output
    root="$(repo_root)"
    list_output="$("$SELF" --list)"
    printf '%s\n' "$list_output" | ${pkgs.lib.getExe verifyTools} runner-policy "$root/loom.toml"
    assert_contains "verify inventory" "$list_output" "verify:cli.package-surface"
    assert_contains "verify inventory" "$list_output" "verify:cli.shared-verifier-app"
    assert_contains "verify inventory" "$list_output" "verify:cli.verify-runner-batching"
  '';
}
