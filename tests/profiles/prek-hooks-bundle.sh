#!/usr/bin/env bash
# Verify wrix.prekHooks bundle contents (specs/profiles.md § Prek hook management).
#
#   test_bundle_contents
#     lib.prekHooks is a directory derivation containing executable shims for
#     pre-commit, pre-push, prepare-commit-msg, post-checkout, post-merge —
#     plus the platform-neutral binding helper.
#
#   test_bundle_path_is_context_stable
#     Every supported host/image system resolves the bundle to the same
#     content-addressed Nix-store path.
#
#   test_shims_use_hook_impl
#     The materialized pre-commit and pre-push shims invoke
#     `prek hook-impl --hook-type=<stage>` rather than `prek run`.
#
#   test_shims_no_flock
#     No materialized shim sources lock.sh, calls _prek_acquire_lock, or
#     invokes flock; every shim invokes hook-impl.
#
#   test_shims_resolve_packaged_prek_at_runtime
#     Every shim runs its configured stage when only the packaged `wrix-prek`
#     resolver, not an ambient `prek`, is initially on PATH.
#
#   test_pre_push_exact_transaction_stamp_written_and_consumed
#     The materialized pre-push shim writes .wrix/push-verified after a
#     passing check, then consumes it on the exact same push transaction.
#
#   test_pre_push_stamp_rejects_different_transaction
#     A stamp cannot approve a push with a different remote identity or ref
#     transaction, even when HEAD is unchanged.
#
#   test_pre_push_stale_stamp_removed_on_failure
#     A stale stamp is removed before a failing check and cannot approve a
#     later push.
#
#   test_pre_push_stamp_cannot_revive_after_return_to_sha
#     Returning to a previously approved HEAD after an intervening failure
#     runs the checks again rather than reviving the old approval.
#
#   test_no_verify_bypasses_pre_commit_and_pre_push
#     Git bypasses otherwise-blocking hooks from the materialized bundle when
#     commit or push is invoked with --no-verify.
#
# Usage:
#   tests/profiles/prek-hooks-bundle.sh                  # run all tests
#   tests/profiles/prek-hooks-bundle.sh test_<name>      # run a single test

set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

resolve_system() {
  nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem'
}

bundle_path() {
  local system
  system=$(resolve_system)
  nix build --no-link --print-out-paths --no-warn-dirty \
    "$REPO_ROOT#legacyPackages.$system.lib.prekHooks"
}

require_bundle() {
  if [[ $# -gt 0 ]]; then
    printf '%s\n' "$1"
    return 0
  fi
  bundle_path
}

# ============================================================================
# Bundle contains the five shims (executable) and no _lib/ subdirectory.
# ============================================================================
test_bundle_contents() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local missing=0
  local hook
  for hook in pre-commit pre-push prepare-commit-msg post-checkout post-merge; do
    if [[ ! -f "$bundle/$hook" ]]; then
      echo "FAIL: bundle missing shim: $hook" >&2
      missing=$((missing + 1))
      continue
    fi
    if [[ ! -x "$bundle/$hook" ]]; then
      echo "FAIL: bundle shim not executable: $hook" >&2
      missing=$((missing + 1))
    fi
  done

  local found
  found=$(find "$bundle" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)
  local expected=$'_binding.sh\npost-checkout\npost-merge\npre-commit\npre-push\nprepare-commit-msg'
  if [[ "$found" != "$expected" ]]; then
    echo "FAIL: bundle contains unexpected paths:" >&2
    printf '%s\n' "$found" >&2
    missing=$((missing + 1))
  fi

  [[ "$missing" -eq 0 ]]
}

# ============================================================================
test_bundle_path_is_context_stable() {
  local expected=""
  local actual system
  local failed=0
  for system in aarch64-darwin aarch64-linux x86_64-linux; do
    if ! actual=$(nix eval --raw --no-warn-dirty \
      "$REPO_ROOT#legacyPackages.$system.lib.prekHooks.outPath"); then
      echo "FAIL: could not evaluate lib.prekHooks for $system" >&2
      failed=$((failed + 1))
      continue
    fi
    if [[ -z "$expected" ]]; then
      expected="$actual"
    elif [[ "$actual" != "$expected" ]]; then
      echo "FAIL: $system bundle path $actual differs from $expected" >&2
      failed=$((failed + 1))
    fi
  done
  [[ "$failed" -eq 0 ]]
}

# ============================================================================
test_shims_use_hook_impl() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local failed=0
  local hook
  for hook in pre-commit pre-push; do
    if [[ ! -f "$bundle/$hook" ]]; then
      echo "FAIL: bundle missing shim: $hook" >&2
      failed=$((failed + 1))
      continue
    fi
    if grep -qE '^[[:space:]]*[^#].*\bprek run\b' "$bundle/$hook"; then
      echo "FAIL: $hook invokes 'prek run' instead of hook-impl" >&2
      failed=$((failed + 1))
    fi
    if ! grep -qE "hook-impl .*--hook-type=$hook( |$)" "$bundle/$hook"; then
      echo "FAIL: $hook does not invoke 'prek hook-impl --hook-type=$hook'" >&2
      failed=$((failed + 1))
    fi
  done

  [[ "$failed" -eq 0 ]]
}

# ============================================================================
test_shims_no_flock() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local failed=0
  local hook
  for hook in pre-commit pre-push prepare-commit-msg post-checkout post-merge; do
    if [[ ! -f "$bundle/$hook" ]]; then
      echo "FAIL: bundle missing shim: $hook" >&2
      failed=$((failed + 1))
      continue
    fi
    if grep -qE 'lock\.sh' "$bundle/$hook"; then
      echo "FAIL: $hook still references lock.sh" >&2
      failed=$((failed + 1))
    fi
    if grep -q '_prek_acquire_lock' "$bundle/$hook"; then
      echo "FAIL: $hook still calls _prek_acquire_lock" >&2
      failed=$((failed + 1))
    fi
    if grep -qE '\bflock\b' "$bundle/$hook"; then
      echo "FAIL: $hook still invokes flock" >&2
      failed=$((failed + 1))
    fi
    if ! grep -qE "hook-impl .*--hook-type=$hook( |$)" "$bundle/$hook"; then
      echo "FAIL: $hook does not invoke 'prek hook-impl --hook-type=$hook'" >&2
      failed=$((failed + 1))
    fi
  done

  [[ "$failed" -eq 0 ]]
}

# ============================================================================
test_shims_resolve_packaged_prek_at_runtime() (
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local work tools repo resolver command stage head_sha zero_sha actual
  local expected=$'pre-commit\nprepare-commit-msg\npost-checkout\npost-merge\npre-push'
  work=$(mktemp -d)
  tools="$work/bin"
  repo="$work/repo"
  resolver=$(command -v wrix-prek)
  trap 'rm -rf "$work"' EXIT

  mkdir -p "$tools"
  for command in bash cat chmod dirname git mkdir rm uname; do
    ln -s "$(command -v "$command")" "$tools/$command"
  done
  export PATH="${resolver%/*}:$tools"
  export GIT_CONFIG_GLOBAL=/dev/null
  export GIT_CONFIG_NOSYSTEM=1
  if command -v prek >/dev/null; then
    echo "FAIL: runtime-resolution test still has ambient prek on PATH" >&2
    return 1
  fi

  git -C "$work" init -q -b main repo
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  cat >"$repo/.git/hook-probe" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "${WRIX_TEST_STAGE:?}" >>.git/hook-probe.log
SCRIPT
  chmod +x "$repo/.git/hook-probe"
  printf 'repos:\n' >"$repo/.pre-commit-config.yaml"
  printf '  - repo: local\n    hooks:\n' >>"$repo/.pre-commit-config.yaml"
  for stage in pre-commit pre-push prepare-commit-msg post-checkout post-merge; do
    cat >>"$repo/.pre-commit-config.yaml" <<YAML
      - id: $stage-probe
        name: $stage-probe
        entry: .git/hook-probe
        language: system
        stages: [$stage]
        always_run: true
        pass_filenames: false
YAML
  done
  printf 'seed\n' >"$repo/seed.txt"
  git -C "$repo" add .
  git -C "$repo" commit -qm initial
  head_sha=$(git -C "$repo" rev-parse HEAD)
  zero_sha=0000000000000000000000000000000000000000

  (
    cd "$repo"
    WRIX_TEST_STAGE=pre-commit "$bundle/pre-commit"
    printf 'message\n' >.git/message
    WRIX_TEST_STAGE=prepare-commit-msg "$bundle/prepare-commit-msg" .git/message message
    WRIX_TEST_STAGE=post-checkout "$bundle/post-checkout" "$head_sha" "$head_sha" 1
    WRIX_TEST_STAGE=post-merge "$bundle/post-merge" 0
    printf 'refs/heads/main %s refs/heads/main %s\n' "$head_sha" "$zero_sha" \
      | WRIX_TEST_STAGE=pre-push "$bundle/pre-push" origin example
  )

  actual=$(cat "$repo/.git/hook-probe.log")
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: packaged prek runtime stage log was: $actual" >&2
    return 1
  fi
)

# ============================================================================
init_pre_push_probe_repo() {
  local worktree="$1"

  mkdir -p "$worktree"
  git -C "$worktree" init -q -b main
  git -C "$worktree" config user.email test@example.com
  git -C "$worktree" config user.name Test
  cat >"$worktree/.pre-commit-config.yaml" <<'YAML'
repos:
  - repo: local
    hooks:
      - id: pre-push-probe
        name: pre-push-probe
        entry: .git/pre-push-probe
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
YAML
  cat >"$worktree/.git/pre-push-probe" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
count=0
if [[ -f .git/pre-push-count ]]; then
  count="$(<.git/pre-push-count)"
fi
printf '%s\n' "$((count + 1))" >.git/pre-push-count
if [[ -f .git/pre-push-fail ]]; then
  exit 1
fi
SCRIPT
  chmod +x "$worktree/.git/pre-push-probe"
  printf 'first\n' >"$worktree/tracked.txt"
  git -C "$worktree" add .
  git -C "$worktree" commit -q -m initial
}

run_pre_push_transaction() {
  local bundle="$1"
  local worktree="$2"
  local remote_name="$3"
  local remote_location="$4"
  local ref_transaction="$5"

  (
    cd "$worktree"
    printf '%s\n' "$ref_transaction" | "$bundle/pre-push" "$remote_name" "$remote_location"
  )
}

run_pre_push_for_head() {
  local bundle="$1"
  local worktree="$2"
  local head_sha
  local old_sha="0000000000000000000000000000000000000000"
  local ref_line

  head_sha="$(git -C "$worktree" rev-parse HEAD)"
  ref_line="refs/heads/main $head_sha refs/heads/main $old_sha"
  run_pre_push_transaction "$bundle" "$worktree" origin example "$ref_line"
}

assert_pre_push_exact_transaction_stamp_written_and_consumed() {
  local bundle="$1"
  local worktree="$2"
  local branch="$3"
  local label="$4"

  local failed=0
  local head_sha old_sha ref_line stamp first_out first_err second_out second_err
  head_sha=$(git -C "$worktree" rev-parse HEAD)
  old_sha=0000000000000000000000000000000000000000
  ref_line="refs/heads/$branch $head_sha refs/heads/$branch $old_sha"
  stamp="$worktree/.wrix/push-verified"
  first_out="$worktree/$label-first.out"
  first_err="$worktree/$label-first.err"
  second_out="$worktree/$label-second.out"
  second_err="$worktree/$label-second.err"

  rm -rf "$worktree/.wrix"
  if ! (cd "$worktree" && printf '%s\n' "$ref_line" | "$bundle/pre-push" origin example) >"$first_out" 2>"$first_err"; then
    echo "FAIL: $label first pre-push invocation failed" >&2
    cat "$first_out" >&2
    cat "$first_err" >&2
    failed=$((failed + 1))
  elif [[ ! -f "$stamp" ]]; then
    echo "FAIL: $label pre-push did not write $stamp" >&2
    failed=$((failed + 1))
  fi

  if [[ "$failed" -eq 0 ]]; then
    if ! (cd "$worktree" && printf '%s\n' "$ref_line" | "$bundle/pre-push" origin example) >"$second_out" 2>"$second_err"; then
      echo "FAIL: $label stamped pre-push invocation failed" >&2
      cat "$second_out" >&2
      cat "$second_err" >&2
      failed=$((failed + 1))
    elif [[ -e "$stamp" ]]; then
      echo "FAIL: $label pre-push did not consume the matching stamp" >&2
      failed=$((failed + 1))
    elif [[ -s "$second_out" || -s "$second_err" ]]; then
      echo "FAIL: $label stamped pre-push invocation did not short-circuit cleanly" >&2
      cat "$second_out" >&2
      cat "$second_err" >&2
      failed=$((failed + 1))
    fi
  fi

  [[ "$failed" -eq 0 ]]
}

test_pre_push_exact_transaction_stamp_written_and_consumed() {
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local work main linked
  work=$(mktemp -d)
  main="$work/main"
  linked="$work/linked"
  mkdir -p "$main"

  local failed=0
  git -C "$main" init -q -b main
  git -C "$main" config user.email test@example.com
  git -C "$main" config user.name Test
  cat >"$main/.pre-commit-config.yaml" <<'YAML'
repos:
  - repo: local
    hooks:
      - id: pass
        name: pass
        entry: true
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
YAML
  echo seed >"$main/seed.txt"
  git -C "$main" add .
  git -C "$main" commit -q -m initial

  if ! assert_pre_push_exact_transaction_stamp_written_and_consumed "$bundle" "$main" main main-worktree; then
    failed=$((failed + 1))
  fi

  git -C "$main" worktree add -q -b linked "$linked"
  if ! assert_pre_push_exact_transaction_stamp_written_and_consumed "$bundle" "$linked" linked linked-worktree; then
    failed=$((failed + 1))
  fi

  rm -rf "$work"
  [[ "$failed" -eq 0 ]]
}

# ============================================================================
test_pre_push_stamp_rejects_different_transaction() (
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local work
  local worktree
  local stamp
  local count_file
  local output
  local head_sha
  local previous_sha
  local zero_sha="0000000000000000000000000000000000000000"
  local canonical_ref
  local extra_ref
  local index
  local -a variant_labels
  local -a variant_remote_names
  local -a variant_remote_locations
  local -a variant_ref_transactions

  work="$(mktemp -d)"
  worktree="$work/worktree"
  stamp="$worktree/.wrix/push-verified"
  count_file="$worktree/.git/pre-push-count"
  output="$work/pre-push.out"
  trap 'rm -rf "$work"' EXIT

  init_pre_push_probe_repo "$worktree"
  previous_sha="$(git -C "$worktree" rev-parse HEAD)"
  printf 'second\n' >"$worktree/tracked.txt"
  git -C "$worktree" add tracked.txt
  git -C "$worktree" commit -q -m second
  head_sha="$(git -C "$worktree" rev-parse HEAD)"
  canonical_ref="refs/heads/main $head_sha refs/heads/main $zero_sha"
  extra_ref="refs/heads/topic $previous_sha refs/heads/topic $zero_sha"

  variant_labels=(
    remote-name
    remote-location
    local-ref
    local-object
    remote-ref
    remote-object
    ref-set
  )
  variant_remote_names=(
    mirror
    origin
    origin
    origin
    origin
    origin
    origin
  )
  variant_remote_locations=(
    example
    alternate
    example
    example
    example
    example
    example
  )
  variant_ref_transactions=(
    "$canonical_ref"
    "$canonical_ref"
    "refs/heads/topic $head_sha refs/heads/main $zero_sha"
    "refs/heads/main $previous_sha refs/heads/main $zero_sha"
    "refs/heads/main $head_sha refs/heads/topic $zero_sha"
    "refs/heads/main $head_sha refs/heads/main $previous_sha"
    "$canonical_ref"$'\n'"$extra_ref"
  )

  for index in "${!variant_labels[@]}"; do
    rm -rf "$worktree/.wrix"
    rm -f "$count_file"

    if ! run_pre_push_transaction "$bundle" "$worktree" origin example "$canonical_ref" >"$output" 2>&1; then
      echo "FAIL: ${variant_labels[$index]} setup transaction failed" >&2
      cat "$output" >&2
      return 1
    fi
    if [[ ! -f "$stamp" || "$(<"$count_file")" != "1" ]]; then
      echo "FAIL: ${variant_labels[$index]} setup did not mint one approval" >&2
      return 1
    fi

    if ! run_pre_push_transaction \
      "$bundle" \
      "$worktree" \
      "${variant_remote_names[$index]}" \
      "${variant_remote_locations[$index]}" \
      "${variant_ref_transactions[$index]}" >"$output" 2>&1; then
      echo "FAIL: ${variant_labels[$index]} variant transaction failed" >&2
      cat "$output" >&2
      return 1
    fi
    if [[ "$(<"$count_file")" != "2" ]]; then
      echo "FAIL: ${variant_labels[$index]} variant reused a different transaction's approval" >&2
      return 1
    fi
    if [[ ! -f "$stamp" ]]; then
      echo "FAIL: ${variant_labels[$index]} checked transaction did not mint a replacement approval" >&2
      return 1
    fi
  done
)

# ============================================================================
test_pre_push_stale_stamp_removed_on_failure() (
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local work
  local worktree
  local stamp
  local output
  work="$(mktemp -d)"
  worktree="$work/worktree"
  stamp="$worktree/.wrix/push-verified"
  output="$work/pre-push.out"
  trap 'rm -rf "$work"' EXIT

  init_pre_push_probe_repo "$worktree"
  mkdir -p "$worktree/.wrix"
  printf '%s\n' "0000000000000000000000000000000000000001" >"$stamp"
  : >"$worktree/.git/pre-push-fail"

  if run_pre_push_for_head "$bundle" "$worktree" >"$output" 2>&1; then
    echo "FAIL: stale-stamp pre-push unexpectedly passed" >&2
    cat "$output" >&2
    return 1
  fi
  if [[ -e "$stamp" ]]; then
    echo "FAIL: stale stamp survived a failing pre-push check" >&2
    return 1
  fi
  if [[ "$(<"$worktree/.git/pre-push-count")" != "1" ]]; then
    echo "FAIL: failing pre-push check did not run exactly once" >&2
    return 1
  fi
)

# ============================================================================
test_pre_push_stamp_cannot_revive_after_return_to_sha() (
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local work
  local worktree
  local first_sha
  local stamp
  local output
  work="$(mktemp -d)"
  worktree="$work/worktree"
  stamp="$worktree/.wrix/push-verified"
  output="$work/pre-push.out"
  trap 'rm -rf "$work"' EXIT

  init_pre_push_probe_repo "$worktree"
  first_sha="$(git -C "$worktree" rev-parse HEAD)"
  run_pre_push_for_head "$bundle" "$worktree" >"$output" 2>&1

  printf 'second\n' >"$worktree/tracked.txt"
  git -C "$worktree" add tracked.txt
  git -C "$worktree" commit -q -m second
  : >"$worktree/.git/pre-push-fail"
  if run_pre_push_for_head "$bundle" "$worktree" >"$output" 2>&1; then
    echo "FAIL: intervening pre-push unexpectedly passed" >&2
    cat "$output" >&2
    return 1
  fi

  git -C "$worktree" reset -q --hard "$first_sha"
  rm -f "$worktree/.git/pre-push-fail"
  run_pre_push_for_head "$bundle" "$worktree" >"$output" 2>&1

  if [[ "$(<"$worktree/.git/pre-push-count")" != "3" ]]; then
    echo "FAIL: returning to the previously stamped HEAD reused the old approval" >&2
    return 1
  fi
  if [[ ! -f "$stamp" ]]; then
    echo "FAIL: fresh check after returning to the old HEAD did not write a new stamp" >&2
    return 1
  fi
)

# ============================================================================
test_no_verify_bypasses_pre_commit_and_pre_push() (
  local bundle
  if ! bundle=$(require_bundle "$@"); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local test_tmp worktree remote commit_sentinel push_sentinel
  test_tmp=$(mktemp -d)
  worktree="$test_tmp/worktree"
  remote="$test_tmp/remote.git"
  commit_sentinel="$worktree/.git/pre-commit-fired"
  push_sentinel="$worktree/.git/pre-push-fired"
  trap 'rm -rf "$test_tmp"' EXIT

  mkdir -p "$worktree"
  git -C "$worktree" init -q -b main
  git -C "$worktree" config user.email test@example.com
  git -C "$worktree" config user.name Test
  printf 'seed\n' >"$worktree/seed.txt"
  git -C "$worktree" add seed.txt
  git -C "$worktree" commit -q -m initial

  cat >"$worktree/.git/fail-pre-commit" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
touch .git/pre-commit-fired
exit 1
SCRIPT
  cat >"$worktree/.git/fail-pre-push" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
touch .git/pre-push-fired
exit 1
SCRIPT
  chmod +x "$worktree/.git/fail-pre-commit" "$worktree/.git/fail-pre-push"
  cat >"$worktree/.pre-commit-config.yaml" <<'YAML'
repos:
  - repo: local
    hooks:
      - id: fail-pre-commit
        name: fail-pre-commit
        entry: .git/fail-pre-commit
        language: system
        stages: [pre-commit]
        always_run: true
        pass_filenames: false
      - id: fail-pre-push
        name: fail-pre-push
        entry: .git/fail-pre-push
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
YAML
  git -C "$worktree" config --local core.hooksPath "$bundle"
  git -C "$worktree" add .pre-commit-config.yaml

  if git -C "$worktree" commit -m blocked; then
    echo "FAIL: pre-commit control unexpectedly passed" >&2
    return 1
  fi
  [[ -f "$commit_sentinel" ]] || {
    echo "FAIL: pre-commit control did not execute the blocking hook" >&2
    return 1
  }
  rm -f "$commit_sentinel"
  git -C "$worktree" commit --no-verify -q -m bypass-commit
  [[ ! -e "$commit_sentinel" ]] || {
    echo "FAIL: git commit --no-verify executed the pre-commit hook" >&2
    return 1
  }

  git -C "$worktree" init --bare -q "$remote"
  git -C "$worktree" remote add origin "$remote"
  if git -C "$worktree" push origin main; then
    echo "FAIL: pre-push control unexpectedly passed" >&2
    return 1
  fi
  [[ -f "$push_sentinel" ]] || {
    echo "FAIL: pre-push control did not execute the blocking hook" >&2
    return 1
  }
  rm -f "$push_sentinel"
  git -C "$worktree" push --no-verify -q origin main
  [[ ! -e "$push_sentinel" ]] || {
    echo "FAIL: git push --no-verify executed the pre-push hook" >&2
    return 1
  }
  [[ "$(git -C "$remote" rev-parse refs/heads/main)" = "$(git -C "$worktree" rev-parse HEAD)" ]]
)

# ----------------------------------------------------------------------------

ALL_TESTS=(
  test_bundle_contents
  test_bundle_path_is_context_stable
  test_shims_use_hook_impl
  test_shims_no_flock
  test_shims_resolve_packaged_prek_at_runtime
  test_pre_push_exact_transaction_stamp_written_and_consumed
  test_pre_push_stamp_rejects_different_transaction
  test_pre_push_stale_stamp_removed_on_failure
  test_pre_push_stamp_cannot_revive_after_return_to_sha
  test_no_verify_bypasses_pre_commit_and_pre_push
)

run_all() {
  local failed=0
  local bundle
  if ! bundle=$(bundle_path); then
    echo "FAIL: nix build lib.prekHooks failed" >&2
    return 1
  fi

  local fn
  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    if "$fn" "$bundle"; then
      echo "PASS: $fn"
    else
      echo "FAIL: $fn"
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    echo "$failed test(s) failed" >&2
    return 1
  fi
}

if [[ $# -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  "$fn"
fi
