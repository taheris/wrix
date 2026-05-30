#!/usr/bin/env bash
# Verifier for criterion 136 of specs/pre-commit.md:
#
#   A commit fired from a linked git worktree acquires the main repo's
#   lock rather than a sibling lock under the worktree dir.
#
# Sources lib/prek/lock.sh from inside a linked worktree, captures the
# resolved `_lock_file`, and asserts both that it lands under the main
# repo's `.wrapix/` and that no sibling `.wrapix/` was created under the
# linked worktree. Matches the source-the-real-implementation convention
# used by the other tests/prek/* verifiers.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

LOCK_LIB="$REPO_ROOT/lib/prek/lock.sh"
if [[ ! -f "$LOCK_LIB" ]]; then
    echo "FAIL: lock library not found at $LOCK_LIB" >&2
    exit 1
fi

TEST_TMP="$(mktemp -d -t wrapix-prek-worktree-lock.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

MAIN_REPO="$TEST_TMP/main"
WORKTREE="$TEST_TMP/worktree"

git init -q -b main "$MAIN_REPO"
(
    cd "$MAIN_REPO"
    git -c user.email=test@example.invalid -c user.name=test \
        commit -q --allow-empty -m init
    git worktree add -q "$WORKTREE" -b worktree-branch
)

MAIN_REPO_REAL="$(cd "$MAIN_REPO" && pwd -P)"
WORKTREE_REAL="$(cd "$WORKTREE" && pwd -P)"

ACTUAL_LOCK="$(
    cd "$WORKTREE"
    # shellcheck source=../../lib/prek/lock.sh
    source "$LOCK_LIB"
    # shellcheck disable=SC2154 # _lock_file is assigned by the sourced lock.sh
    printf '%s\n' "$_lock_file"
)"

ACTUAL_LOCK_DIR_REAL="$(cd "$(dirname "$ACTUAL_LOCK")" && pwd -P)"
ACTUAL_LOCK_REAL="$ACTUAL_LOCK_DIR_REAL/$(basename "$ACTUAL_LOCK")"
EXPECTED_LOCK_REAL="$MAIN_REPO_REAL/.wrapix/prek.lock"

if [[ "$ACTUAL_LOCK_REAL" != "$EXPECTED_LOCK_REAL" ]]; then
    echo "FAIL: linked worktree did not resolve lock to main repo's .wrapix/" >&2
    echo "  expected: $EXPECTED_LOCK_REAL" >&2
    echo "  actual:   $ACTUAL_LOCK_REAL" >&2
    echo "  main repo: $MAIN_REPO_REAL" >&2
    echo "  worktree:  $WORKTREE_REAL" >&2
    exit 1
fi

if [[ -e "$WORKTREE_REAL/.wrapix" ]]; then
    echo "FAIL: sibling .wrapix/ was created under the linked worktree" >&2
    echo "  path: $WORKTREE_REAL/.wrapix" >&2
    exit 1
fi

echo "PASS: linked-worktree commit resolves to main repo lock ($ACTUAL_LOCK_REAL)"
