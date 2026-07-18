_:

{
  "cli.package-surface" = ''
    local package
    local forbidden
    local repo_beads_bin
    package="$(build_flake_package wrix)"
    assert_executable "$package/bin/wrix"
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
    local unknown_out
    local unknown_err
    local unknown_message
    local skipped_out
    local skipped_err
    local skipped_status
    list_output="$("$SELF" --list)"
    assert_contains "verify list" "$list_output" "verify:cli.package-surface"
    assert_contains "verify list" "$list_output" "verify:cli.shared-verifier-app"
    assert_contains "verify list" "$list_output" "verify:cli.verify-runner-batching"

    batch_output="$("$SELF" cli.verify-runner-batching verify:cli.verify-runner-batching)"
    assert_json_verdict "batched verifier" "$batch_output" "cli.verify-runner-batching"
    verdict_count="$(printf '%s\n' "$batch_output" | jq -rs '[.[] | select(.target == "cli.verify-runner-batching" and .pass == true)] | length')"
    if [[ "$verdict_count" -ne 2 ]]; then
      fail "batched verifier emitted $verdict_count passing verdicts; expected 2"
    fi

    unknown_out="$(mktemp -t wrix-verify-unknown.XXXXXX)"
    unknown_err="$(mktemp -t wrix-verify-unknown.XXXXXX)"
    if "$SELF" verify:cli.not-registered >"$unknown_out" 2>"$unknown_err"; then
      rm -f "$unknown_out" "$unknown_err"
      fail "unknown verify target unexpectedly succeeded"
    fi
    unknown_message="$(cat "$unknown_err")"
    rm -f "$unknown_out" "$unknown_err"
    assert_contains "unknown target" "$unknown_message" "Unknown verify target: verify:cli.not-registered"
    assert_contains "unknown target" "$unknown_message" "nix run .#verify -- --list"
    assert_contains "unknown target" "$unknown_message" "verify:cli.package-surface"

    skipped_err="$(mktemp -t wrix-verify-skipped.XXXXXX)"
    run_target() {
      printf 'SKIP: synthetic unavailable prerequisite\n'
      return 77
    }
    set +e
    skipped_out="$(run_one synthetic.skip 2>"$skipped_err")"
    skipped_status="$?"
    set -e
    rm -f "$skipped_err"
    if [[ "$skipped_status" -ne 1 ]]; then
      fail "skipped verifier exited $skipped_status; expected 1"
    fi
    if ! printf '%s\n' "$skipped_out" | jq -e '
      .target == "synthetic.skip"
      and .pass == false
      and (.evidence | startswith("skipped: SKIP:"))
    ' >/dev/null; then
      fail "skipped verifier did not emit a failing JSON verdict: $skipped_out"
    fi
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
