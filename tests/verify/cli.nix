{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;
  # Retain the existing store source without coercing/copying its path again.
  nixpkgsSource =
    let
      path = toString pkgs.path;
    in
    builtins.appendContext path { "${path}".path = true; };
in
{
  "cli.package-surface" = ''
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

  "cli.shared-verifier-app" = ''
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
    python3 "$root/tests/verify/test_results.py" "$SELF" "$loom/bin/loom" ${../lib/verifier.sh}
    nix run --no-warn-dirty "$root#test-ci" -- --list >/dev/null
    test_ci="$(nix eval --raw --no-warn-dirty "$root#apps.${system}.test-ci.program")"
    python3 "$root/tests/verify/test_inputs.py" "$SELF" "$test_ci" "$loom/bin/loom" "$root" ${escapeShellArg nixpkgsSource} ${escapeShellArg system}
  '';

  "cli.verify-runner-batching" = ''
    local root
    local list_output
    root="$(repo_root)"
    list_output="$("$SELF" --list)"
    printf '%s\n' "$list_output" | python3 ${./check-runner-config.py} "$root/loom.toml"
    assert_contains "verify inventory" "$list_output" "verify:cli.package-surface"
    assert_contains "verify inventory" "$list_output" "verify:cli.shared-verifier-app"
    assert_contains "verify inventory" "$list_output" "verify:cli.verify-runner-batching"
  '';
}
