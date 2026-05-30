#!/usr/bin/env bash
# Verifier for the Flock Serialization Contract in specs/pre-commit.md:
#
#   Two concurrent pre-commit hooks against the same workspace serialize
#   (the second blocks until the first releases) without losing
#   working-tree edits.
#
# Drives lib/prek/hooks/pre-commit directly against a fresh temp git
# repo. Replaces `prek` on PATH with a stand-in whose `hook-impl`
# subcommand logs critical-section enter/exit timestamps and sleeps long
# enough that two parallel windows would interleave if flock did not
# serialize them. Asserts the second ENTER follows the first EXIT and
# that a working-tree edit made between the two invocations survives.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

if ! command -v flock >/dev/null; then
    echo "SKIP: flock(1) missing — enter nix develop or install util-linux" >&2
    exit 77
fi

HOOK="$REPO_ROOT/lib/prek/hooks/pre-commit"
if [[ ! -x "$HOOK" ]]; then
    echo "FAIL: pre-commit shim not executable at $HOOK" >&2
    exit 1
fi

TEST_TMP="$(mktemp -d -t wrapix-prek-concurrent.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

BASH_BIN="$(command -v bash)"

WORK="$TEST_TMP/work"
git init -q -b main "$WORK"
(
    cd "$WORK"
    git -c user.email=test@example.invalid -c user.name=test \
        commit -q --allow-empty -m init
)

LOG="$TEST_TMP/prek.log"
: > "$LOG"

PREK_SHIM_DIR="$TEST_TMP/prek-bin"
mkdir -p "$PREK_SHIM_DIR"
cat > "$PREK_SHIM_DIR/prek" <<EOF
#!$BASH_BIN
set -euo pipefail
if [[ "\${1:-}" != "hook-impl" ]]; then
    echo "prek shim: unexpected args: \$*" >&2
    exit 2
fi
printf '%d ENTER %s\n' "\$\$" "\$(date +%s.%N)" >> "$LOG"
sleep 1
printf '%d EXIT %s\n' "\$\$" "\$(date +%s.%N)" >> "$LOG"
EOF
chmod +x "$PREK_SHIM_DIR/prek"

PATH_OVERRIDE="$PREK_SHIM_DIR:$PATH"

run_hook() {
    (
        cd "$WORK"
        PATH="$PATH_OVERRIDE" exec "$HOOK"
    )
}

run_hook &
PID1=$!

waited=0
while [[ ! -s "$LOG" ]]; do
    if (( waited > 50 )); then
        # best-effort: reap the orphan shim before we exit non-zero.
        kill "$PID1" 2>/dev/null || true
        echo "FAIL: shim 1 did not enter critical section within 5s" >&2
        exit 1
    fi
    sleep 0.1
    waited=$((waited + 1))
done

echo "edit between commits" > "$WORK/sentinel.txt"

run_hook &
PID2=$!

rc1=0
wait "$PID1" || rc1=$?
rc2=0
wait "$PID2" || rc2=$?

if [[ "$rc1" -ne 0 || "$rc2" -ne 0 ]]; then
    echo "FAIL: hooks exited non-zero: pid1=$rc1 pid2=$rc2" >&2
    sed 's/^/  /' "$LOG" >&2
    exit 1
fi

mapfile -t rows < <(sort -k3 -n "$LOG")
if [[ "${#rows[@]}" -ne 4 ]]; then
    echo "FAIL: expected 4 log rows (2 ENTER + 2 EXIT), got ${#rows[@]}" >&2
    sed 's/^/  /' "$LOG" >&2
    exit 1
fi

read -r p1 k1 _ <<<"${rows[0]}"
read -r p2 k2 _ <<<"${rows[1]}"
read -r p3 k3 _ <<<"${rows[2]}"
read -r p4 k4 _ <<<"${rows[3]}"

if [[ "$k1" != "ENTER" || "$k2" != "EXIT" \
        || "$k3" != "ENTER" || "$k4" != "EXIT" \
        || "$p1" != "$p2" || "$p3" != "$p4" \
        || "$p1" == "$p3" ]]; then
    echo "FAIL: critical sections overlapped — lock did not serialize" >&2
    sed 's/^/  /' "$LOG" >&2
    exit 1
fi

if [[ ! -f "$WORK/sentinel.txt" ]] \
        || [[ "$(cat "$WORK/sentinel.txt")" != "edit between commits" ]]; then
    echo "FAIL: working-tree edit lost between concurrent hook invocations" >&2
    exit 1
fi

echo "PASS: concurrent pre-commit hooks serialized; working-tree edit preserved"
