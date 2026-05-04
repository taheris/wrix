#!/usr/bin/env bash
# Verify tests for the Loom harness spec.
#
# Each function exits 0 on PASS, non-zero on FAIL, 77 to skip.
# Invoked by `ralph spec --verify` as `tests/loom-test.sh <function_name>`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOOM_DIR="$REPO_ROOT/loom"
WORKSPACE_TOML="$LOOM_DIR/Cargo.toml"

# Member crates expected at loom/crates/<name>/.
LOOM_CRATES=(loom loom-core loom-agent loom-workflow loom-templates)

# Workspace-pinned third-party deps (14, per spec).
LOOM_DEPS=(
    tokio serde serde_json thiserror displaydoc anyhow
    tracing tracing-subscriber rusqlite toml askama clap gix fd-lock
)

# Run cargo with a Rust toolchain regardless of whether the devShell has
# rust on PATH yet (Nix integration lands in a separate issue).
cargo_run() {
    if command -v cargo >/dev/null 2>&1; then
        ( cd "$LOOM_DIR" && cargo "$@" )
    else
        nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc nixpkgs#clippy \
            --command bash -c "cd '$LOOM_DIR' && cargo $*"
    fi
}

# Read a key from a TOML file via `toml` python module if available, else
# fall back to grep. Used only for shape checks in `test_workspace_*`.
toml_grep() {
    local pattern="$1" file="$2"
    grep -E "$pattern" "$file"
}

#-----------------------------------------------------------------------------
# test_workspace_builds — `cargo build` from loom/ root succeeds.
#-----------------------------------------------------------------------------
test_workspace_builds() {
    cargo_run build --workspace --quiet
}

#-----------------------------------------------------------------------------
# test_crate_structure — all five member crates exist with src/{lib,main}.rs.
#-----------------------------------------------------------------------------
test_crate_structure() {
    local missing=0
    for crate in "${LOOM_CRATES[@]}"; do
        local dir="$LOOM_DIR/crates/$crate"
        if [ ! -d "$dir" ]; then
            echo "missing crate dir: $dir" >&2
            missing=$((missing + 1))
            continue
        fi
        if [ ! -f "$dir/Cargo.toml" ]; then
            echo "missing Cargo.toml: $dir/Cargo.toml" >&2
            missing=$((missing + 1))
        fi
        if [ "$crate" = "loom" ]; then
            if [ ! -f "$dir/src/main.rs" ]; then
                echo "missing main.rs: $dir/src/main.rs" >&2
                missing=$((missing + 1))
            fi
        else
            if [ ! -f "$dir/src/lib.rs" ]; then
                echo "missing lib.rs: $dir/src/lib.rs" >&2
                missing=$((missing + 1))
            fi
        fi
        if [ -f "$dir/src/types.rs" ]; then
            echo "forbidden central types.rs at: $dir/src/types.rs" >&2
            missing=$((missing + 1))
        fi
        if [ -f "$dir/src/error.rs" ]; then
            echo "forbidden central error.rs at: $dir/src/error.rs" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_workspace_edition — workspace declares edition 2024 + resolver "3".
#-----------------------------------------------------------------------------
test_workspace_edition() {
    if ! toml_grep '^resolver[[:space:]]*=[[:space:]]*"3"' "$WORKSPACE_TOML" >/dev/null; then
        echo "workspace resolver is not \"3\" in $WORKSPACE_TOML" >&2
        return 1
    fi
    if ! toml_grep '^edition[[:space:]]*=[[:space:]]*"2024"' "$WORKSPACE_TOML" >/dev/null; then
        echo "workspace.package.edition is not \"2024\" in $WORKSPACE_TOML" >&2
        return 1
    fi
    # Each member must inherit edition from the workspace.
    local crate dir missing=0
    for crate in "${LOOM_CRATES[@]}"; do
        dir="$LOOM_DIR/crates/$crate"
        if ! grep -E '^edition\.workspace[[:space:]]*=[[:space:]]*true' "$dir/Cargo.toml" >/dev/null; then
            echo "$crate: edition.workspace = true missing in $dir/Cargo.toml" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_workspace_deps_pinned — every spec-listed third-party crate is pinned
# under [workspace.dependencies] in loom/Cargo.toml.
#-----------------------------------------------------------------------------
test_workspace_deps_pinned() {
    # Extract the [workspace.dependencies] section into a buffer and check
    # each expected dep appears as a key. We stop at the next [section].
    local section
    section=$(awk '
        /^\[workspace\.dependencies\][[:space:]]*$/ { in_section = 1; next }
        in_section && /^\[/ { in_section = 0 }
        in_section { print }
    ' "$WORKSPACE_TOML")

    if [ -z "$section" ]; then
        echo "[workspace.dependencies] section missing in $WORKSPACE_TOML" >&2
        return 1
    fi

    local missing=0 dep
    for dep in "${LOOM_DEPS[@]}"; do
        if ! grep -E "^${dep}[[:space:]]*=" <<<"$section" >/dev/null; then
            echo "dep not pinned in [workspace.dependencies]: $dep" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_workspace_lints — workspace declares strict lint block; every member
# crate inherits it via `[lints] workspace = true`.
#-----------------------------------------------------------------------------
test_workspace_lints() {
    local missing=0

    if ! grep -E '^\[workspace\.lints\.rust\][[:space:]]*$' "$WORKSPACE_TOML" >/dev/null; then
        echo "[workspace.lints.rust] section missing in $WORKSPACE_TOML" >&2
        missing=$((missing + 1))
    fi
    if ! grep -E '^\[workspace\.lints\.clippy\][[:space:]]*$' "$WORKSPACE_TOML" >/dev/null; then
        echo "[workspace.lints.clippy] section missing in $WORKSPACE_TOML" >&2
        missing=$((missing + 1))
    fi
    # Spec NF-9: panics banned. The four denials must appear in clippy block.
    local lint clippy_section
    clippy_section=$(awk '
        /^\[workspace\.lints\.clippy\][[:space:]]*$/ { in_section = 1; next }
        in_section && /^\[/ { in_section = 0 }
        in_section { print }
    ' "$WORKSPACE_TOML")
    for lint in unwrap_used todo unimplemented panic; do
        if ! grep -E "^${lint}[[:space:]]*=[[:space:]]*\"deny\"" <<<"$clippy_section" >/dev/null; then
            echo "clippy lint $lint not denied in $WORKSPACE_TOML" >&2
            missing=$((missing + 1))
        fi
    done

    local crate dir
    for crate in "${LOOM_CRATES[@]}"; do
        dir="$LOOM_DIR/crates/$crate"
        if ! awk '
            /^\[lints\][[:space:]]*$/ { in_section = 1; next }
            in_section && /^\[/ { in_section = 0 }
            in_section && /^workspace[[:space:]]*=[[:space:]]*true/ { found = 1 }
            END { exit (found ? 0 : 1) }
        ' "$dir/Cargo.toml"; then
            echo "$crate: [lints] workspace = true missing in $dir/Cargo.toml" >&2
            missing=$((missing + 1))
        fi
    done

    return "$missing"
}

#-----------------------------------------------------------------------------
# test_nix_build — `nix build .#loom` succeeds and produces a loom binary.
#-----------------------------------------------------------------------------
test_nix_build() {
    local out
    out=$(nix build "$REPO_ROOT#loom" --no-link --print-out-paths)
    if [ ! -x "$out/bin/loom" ]; then
        echo "expected executable at $out/bin/loom" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_devshell_includes_loom — loom binary on PATH inside `nix develop`,
# alongside ralph (dual-path transition).
#-----------------------------------------------------------------------------
test_devshell_includes_loom() {
    local paths
    paths=$(nix develop "$REPO_ROOT" --command bash -c 'command -v loom; command -v ralph')
    if ! grep -q '/loom$' <<<"$paths"; then
        echo "loom not found on devShell PATH" >&2
        echo "$paths" >&2
        return 1
    fi
    if ! grep -q '/ralph$' <<<"$paths"; then
        echo "ralph not found on devShell PATH (dual-path requires both)" >&2
        echo "$paths" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_clippy_clean — `cargo clippy --workspace` passes with workspace lints
# (warnings denied).
#-----------------------------------------------------------------------------
test_clippy_clean() {
    cargo_run clippy --workspace --all-targets -- -D warnings
}

#-----------------------------------------------------------------------------
# test_cargo_test — `cargo test --workspace` passes for all crates.
#-----------------------------------------------------------------------------
test_cargo_test() {
    cargo_run test --workspace
}

#-----------------------------------------------------------------------------
# Helpers for wrapix run-bead acceptance tests.
#
# The wrapix script has a WRAPIX_DRY_RUN=1 mode that parses the SpawnConfig
# and prints resolved spawn state without invoking the container runtime.
# That lets us verify SpawnConfig consumption on any host (incl. CI without
# /dev/kvm) without booting a real container.
#-----------------------------------------------------------------------------
wrapix_bin() {
    nix build "$REPO_ROOT#sandbox" --no-link --print-out-paths 2>/dev/null
}

write_spawn_config() {
    local path="$1" image="$2" workspace="${3:-/some/workspace}"
    cat >"$path" <<EOF
{
  "image": "$image",
  "workspace": "$workspace",
  "env": [["WRAPIX_AGENT","claude-code"],["TERM","dumb"]],
  "initial_prompt": "do the thing",
  "agent_args": ["--print","--output-format","stream-json"],
  "repin": {"orientation":"o","pinned_context":"pc","partial_bodies":{}}
}
EOF
}

#-----------------------------------------------------------------------------
# test_wrapix_run_bead_subcommand — `wrapix run-bead --spawn-config <f> --stdio`
# parses the JSON, omits TTY (STDIO=1), and exposes the resolved spawn state.
#-----------------------------------------------------------------------------
test_wrapix_run_bead_subcommand() {
    local out sandbox tmp
    sandbox=$(wrapix_bin)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    write_spawn_config "$tmp/spawn.json" "localhost/wrapix-test:run-bead"
    out=$(WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/spawn.json" --stdio)

    grep -qx 'SUBCOMMAND=run-bead' <<<"$out" || { echo "missing SUBCOMMAND=run-bead" >&2; echo "$out" >&2; return 1; }
    grep -qx 'STDIO=1' <<<"$out" || { echo "missing STDIO=1 (expected --stdio to set it)" >&2; echo "$out" >&2; return 1; }
    grep -qx 'IMAGE_OVERRIDE=localhost/wrapix-test:run-bead' <<<"$out" || { echo "image override not honored" >&2; echo "$out" >&2; return 1; }

    # --spawn-config is required.
    if WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" run-bead --stdio 2>/dev/null; then
        echo "wrapix run-bead without --spawn-config should fail" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_spawn_config_json_stability — every documented SpawnConfig field
# round-trips through the JSON shape without rename or loss.
#-----------------------------------------------------------------------------
test_spawn_config_json_stability() {
    local sandbox tmp keys
    sandbox=$(wrapix_bin)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    cat >"$tmp/spawn.json" <<'EOF'
{
  "image": "localhost/wrapix-stability:tag",
  "workspace": "/work/ws",
  "env": [["A","1"],["B","2"]],
  "initial_prompt": "prompt body",
  "agent_args": ["one","two"],
  "repin": {
    "orientation": "ori",
    "pinned_context": "ctx",
    "partial_bodies": {"foo": "bar"}
  }
}
EOF

    # All six top-level keys must be present in the fixture itself.
    keys=$(jq -r 'keys | sort | join(",")' "$tmp/spawn.json")
    [ "$keys" = "agent_args,env,image,initial_prompt,repin,workspace" ] \
        || { echo "fixture key set drifted: $keys" >&2; return 1; }

    # Round-trip via jq must preserve every key+value (jq -S sorts keys for diff).
    jq -S . "$tmp/spawn.json" >"$tmp/canon.json"
    jq -S . "$tmp/canon.json" >"$tmp/canon2.json"
    if ! diff -q "$tmp/canon.json" "$tmp/canon2.json" >/dev/null; then
        echo "round-trip diverged" >&2
        diff "$tmp/canon.json" "$tmp/canon2.json" >&2
        return 1
    fi

    # repin sub-keys must also round-trip.
    keys=$(jq -r '.repin | keys | sort | join(",")' "$tmp/spawn.json")
    [ "$keys" = "orientation,partial_bodies,pinned_context" ] \
        || { echo "repin key set drifted: $keys" >&2; return 1; }

    # The launcher must accept the full fixture (no unknown-field errors).
    WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/spawn.json" --stdio >/dev/null
}

#-----------------------------------------------------------------------------
# test_per_bead_profile_spawn — two SpawnConfigs with different `image`
# yield two `wrapix run-bead` invocations that resolve to different images.
#-----------------------------------------------------------------------------
test_per_bead_profile_spawn() {
    local sandbox tmp out_a out_b
    sandbox=$(wrapix_bin)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    write_spawn_config "$tmp/a.json" "localhost/wrapix-base:tagA"
    write_spawn_config "$tmp/b.json" "localhost/wrapix-rust:tagB"

    out_a=$(WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/a.json" --stdio | grep '^IMAGE_OVERRIDE=')
    out_b=$(WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/b.json" --stdio | grep '^IMAGE_OVERRIDE=')

    [ "$out_a" = "IMAGE_OVERRIDE=localhost/wrapix-base:tagA" ] || { echo "bead A: $out_a" >&2; return 1; }
    [ "$out_b" = "IMAGE_OVERRIDE=localhost/wrapix-rust:tagB" ] || { echo "bead B: $out_b" >&2; return 1; }
    [ "$out_a" != "$out_b" ] || { echo "two beads produced identical image" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_wrapix_run_bead_spawn — drives `loom todo --agent pi` through a
# shim wrapix that records argv, then asserts the loom binary handed the
# wrapper exactly `run-bead --spawn-config <file> --stdio` and that the
# JSON file carries the resolved profile image. The shim then exec's
# mock-pi probe-ok so the backend's startup handshake completes and the
# loom process exits 0 (otherwise the test would observe a hang/timeout
# instead of the recorded argv).
#-----------------------------------------------------------------------------
test_wrapix_run_bead_spawn() {
    cargo_run test -p loom --test spawn_dispatch -- --test-threads=1 \
        wrapix_run_bead_invocation_records_correct_argv
}

#-----------------------------------------------------------------------------
# test_container_stdio_pipe — the wrapix child receives stdin as a pipe
# (not a TTY), and EOF on that pipe is what loom uses to signal "no more
# input" to the agent. The shim records `[ -t 0 ]` and `[ -p /dev/stdin ]`
# state into a sidecar file before handing off to mock-pi; the test reads
# the file back and asserts stdin_is_tty=0 / stdin_is_pipe=1. The
# round-trip through mock-pi (probe + prompt + agent_end) is the second
# half of the contract: a tty-backed handle would either block forever
# or feed wrong bytes, and the loom binary would never exit 0.
#-----------------------------------------------------------------------------
test_container_stdio_pipe() {
    cargo_run test -p loom --test spawn_dispatch -- --test-threads=1 \
        child_stdin_is_a_pipe_not_a_tty
}

#-----------------------------------------------------------------------------
# Helpers for entrypoint dispatch tests.
#
# The entrypoint hardcodes /workspace and /etc/wrapix paths, so we don't run
# it directly. Instead, structural checks confirm the WRAPIX_AGENT branch
# wires pi --mode rpc, gates claude config behind the claude branch, and
# keeps git/beads/network setup ahead of the agent dispatch.
#-----------------------------------------------------------------------------
ENTRYPOINT_SH="$REPO_ROOT/lib/sandbox/linux/entrypoint.sh"

# Print the first line number where the (extended) regex matches, or empty.
entrypoint_line() {
    grep -nE "$1" "$ENTRYPOINT_SH" | head -1 | cut -d: -f1
}

#-----------------------------------------------------------------------------
# test_entrypoint_pi_mode — WRAPIX_AGENT=pi launches pi --mode rpc and
# skips Claude config merging.
#-----------------------------------------------------------------------------
test_entrypoint_pi_mode() {
    [ -f "$ENTRYPOINT_SH" ] || { echo "missing: $ENTRYPOINT_SH" >&2; return 1; }
    bash -n "$ENTRYPOINT_SH" || { echo "syntax error" >&2; return 1; }

    # Pi RPC mode invocation must be present.
    local pi_ln
    pi_ln=$(entrypoint_line 'pi[[:space:]]+--mode[[:space:]]+rpc')
    [ -n "$pi_ln" ] || { echo "missing 'pi --mode rpc' invocation" >&2; return 1; }

    # The pi invocation must be guarded by WRAPIX_AGENT=pi within the
    # preceding 5 lines (covers the elif/case dispatch shape).
    awk -v ln="$pi_ln" '
        NR >= ln-5 && NR < ln && /WRAPIX_AGENT/ && /"pi"/ {found=1}
        END {exit !found}
    ' "$ENTRYPOINT_SH" \
        || { echo "'pi --mode rpc' not gated by WRAPIX_AGENT=pi" >&2; return 1; }

    # claude-config.json copy must live inside an `if "$WRAPIX_AGENT" =
    # "claude"` block — pi must not trigger that copy.
    local cp_ln guard_open guard_close
    cp_ln=$(entrypoint_line 'cp[[:space:]]+/etc/wrapix/claude-config\.json')
    [ -n "$cp_ln" ] || { echo "missing claude-config.json copy" >&2; return 1; }

    guard_open=$(awk '
        /^[[:space:]]*if[[:space:]]+\[[[:space:]]+"\$WRAPIX_AGENT"[[:space:]]+=[[:space:]]+"claude"[[:space:]]+\][[:space:]]*;[[:space:]]*then/ {print NR; exit}
    ' "$ENTRYPOINT_SH")
    [ -n "$guard_open" ] \
        || { echo "no 'if [ \$WRAPIX_AGENT = claude ]; then' guard found" >&2; return 1; }

    # Find the matching `fi` at the same indent (top-level fi line >= cp_ln).
    guard_close=$(awk -v open="$guard_open" '
        NR > open && /^fi$/ {print NR; exit}
    ' "$ENTRYPOINT_SH")
    [ -n "$guard_close" ] || { echo "claude guard 'fi' not found" >&2; return 1; }

    if [ "$cp_ln" -le "$guard_open" ] || [ "$cp_ln" -ge "$guard_close" ]; then
        echo "claude-config.json copy (line $cp_ln) not inside WRAPIX_AGENT=claude guard ($guard_open..$guard_close)" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_entrypoint_claude_mode — WRAPIX_AGENT defaults to claude and the
# existing claude launch (claude --dangerously-skip-permissions, config
# merging) is preserved.
#-----------------------------------------------------------------------------
test_entrypoint_claude_mode() {
    [ -f "$ENTRYPOINT_SH" ] || { echo "missing: $ENTRYPOINT_SH" >&2; return 1; }
    bash -n "$ENTRYPOINT_SH" || { echo "syntax error" >&2; return 1; }

    # Default value of WRAPIX_AGENT must be "claude" for backward compat.
    grep -qE 'WRAPIX_AGENT="\$\{WRAPIX_AGENT:-claude\}"' "$ENTRYPOINT_SH" \
        || { echo "WRAPIX_AGENT default 'claude' not set via \${WRAPIX_AGENT:-claude}" >&2; return 1; }

    # Claude binary launch with --dangerously-skip-permissions still present.
    grep -qE 'claude[[:space:]]+--dangerously-skip-permissions' "$ENTRYPOINT_SH" \
        || { echo "missing 'claude --dangerously-skip-permissions' launch" >&2; return 1; }

    # Claude config merge is preserved (jq-based settings.json merge).
    grep -qE 'cp[[:space:]]+/etc/wrapix/claude-settings\.json' "$ENTRYPOINT_SH" \
        || { echo "missing claude-settings.json copy" >&2; return 1; }
    grep -qE 'cp[[:space:]]+/etc/wrapix/claude-config\.json' "$ENTRYPOINT_SH" \
        || { echo "missing claude-config.json copy" >&2; return 1; }

    # Symlink loop for persistent claude session data is preserved.
    grep -qE 'projects[[:space:]]+plans[[:space:]]+todos' "$ENTRYPOINT_SH" \
        || { echo "missing claude session data symlink loop" >&2; return 1; }

    # Reject unknown WRAPIX_AGENT values — fail-fast diagnostic must exist.
    grep -qE 'unknown[[:space:]]+WRAPIX_AGENT' "$ENTRYPOINT_SH" \
        || { echo "missing fail-fast for unknown WRAPIX_AGENT value" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_entrypoint_shared_setup — git SSH, beads-dolt connection, and
# network filtering all run before the agent dispatch (and therefore before
# both pi and claude invocations).
#-----------------------------------------------------------------------------
test_entrypoint_shared_setup() {
    [ -f "$ENTRYPOINT_SH" ] || { echo "missing: $ENTRYPOINT_SH" >&2; return 1; }

    local git_ln beads_ln net_ln pi_ln claude_ln agent_dispatch_ln
    git_ln=$(entrypoint_line '^\. /git-ssh-setup\.sh')
    beads_ln=$(entrypoint_line '/workspace/\.beads/config\.yaml')
    net_ln=$(entrypoint_line 'WRAPIX_NETWORK:-open')
    pi_ln=$(entrypoint_line 'pi[[:space:]]+--mode[[:space:]]+rpc')
    claude_ln=$(entrypoint_line 'claude[[:space:]]+--dangerously-skip-permissions')

    [ -n "$git_ln" ] || { echo "git-ssh-setup not sourced" >&2; return 1; }
    [ -n "$beads_ln" ] || { echo "beads-dolt config check missing" >&2; return 1; }
    [ -n "$net_ln" ] || { echo "WRAPIX_NETWORK gate missing" >&2; return 1; }
    [ -n "$pi_ln" ] || { echo "pi --mode rpc not present" >&2; return 1; }
    [ -n "$claude_ln" ] || { echo "claude launch not present" >&2; return 1; }

    # Earliest agent dispatch line — both branches must follow shared setup.
    agent_dispatch_ln=$(( pi_ln < claude_ln ? pi_ln : claude_ln ))

    if [ "$git_ln" -ge "$agent_dispatch_ln" ]; then
        echo "git-ssh-setup (line $git_ln) runs after agent dispatch (line $agent_dispatch_ln)" >&2
        return 1
    fi
    if [ "$beads_ln" -ge "$agent_dispatch_ln" ]; then
        echo "beads-dolt setup (line $beads_ln) runs after agent dispatch (line $agent_dispatch_ln)" >&2
        return 1
    fi
    if [ "$net_ln" -ge "$agent_dispatch_ln" ]; then
        echo "network filtering (line $net_ln) runs after agent dispatch (line $agent_dispatch_ln)" >&2
        return 1
    fi

    # Shared setup must be unconditional w.r.t. WRAPIX_AGENT — i.e. NOT
    # nested inside the `if [ "$WRAPIX_AGENT" = "claude" ]; then ... fi`
    # block. Find that block's range and assert the shared-setup lines fall
    # outside it.
    local guard_open guard_close
    guard_open=$(awk '
        /^[[:space:]]*if[[:space:]]+\[[[:space:]]+"\$WRAPIX_AGENT"[[:space:]]+=[[:space:]]+"claude"[[:space:]]+\][[:space:]]*;[[:space:]]*then/ {print NR; exit}
    ' "$ENTRYPOINT_SH")
    [ -n "$guard_open" ] || { echo "no top-level WRAPIX_AGENT=claude guard" >&2; return 1; }
    guard_close=$(awk -v open="$guard_open" '
        NR > open && /^fi$/ {print NR; exit}
    ' "$ENTRYPOINT_SH")
    [ -n "$guard_close" ] || { echo "WRAPIX_AGENT=claude guard 'fi' missing" >&2; return 1; }

    local ln name
    for pair in "git_ln=$git_ln" "beads_ln=$beads_ln" "net_ln=$net_ln"; do
        name=${pair%=*}
        ln=${pair#*=}
        if [ "$ln" -gt "$guard_open" ] && [ "$ln" -lt "$guard_close" ]; then
            echo "$name (line $ln) is inside WRAPIX_AGENT=claude guard ($guard_open..$guard_close); should be shared" >&2
            return 1
        fi
    done
}

#-----------------------------------------------------------------------------
# test_loom_does_not_invoke_podman — loom Rust sources never invoke podman
# directly; only documentation/comments may reference it. Both backend
# spawn paths (PiBackend, ClaudeBackend) MUST drive the wrapper via
# `wrapix run-bead` — the positive contract that complements the negative
# grep above. A future refactor that bypasses the wrapper to call podman
# directly would either reintroduce a podman match or drop the run-bead
# string; this test catches both.
#-----------------------------------------------------------------------------
test_loom_does_not_invoke_podman() {
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    # The contract is: loom never spawns a podman process. Look for the
    # actual invocation patterns — Command::new("podman" or a bare
    # "podman" string passed to a process spawn — rather than every
    # mention of the word, so legitimate metadata (verify-deps mapping,
    # doc-strings explaining what wrapix does on top) is not a false
    # positive. Test files under */tests/* are excluded: they may legally
    # describe podman in shim documentation.
    local violations=0
    while IFS= read -r -d '' file; do
        case "$file" in
            */tests/*) continue ;;
        esac
        local hits
        hits=$(grep -nE 'Command::new\("podman"|spawn\("podman"|exec\("podman"|process::Command.*podman' "$file" || true)
        if [ -n "$hits" ]; then
            echo "podman invocation found in $file:" >&2
            echo "$hits" >&2
            violations=$((violations + 1))
        fi
    done < <(find "$LOOM_DIR/crates" -name '*.rs' -print0)
    if [ "$violations" -ne 0 ]; then
        return 1
    fi

    # Positive contract: each backend must spawn through `wrapix run-bead`.
    # The literal "run-bead" appears as the first arg in both backends'
    # Command construction; missing it would mean the backend bypassed the
    # wrapper, which is the failure mode the negative grep alone could miss.
    local backend
    for backend in \
        "$LOOM_DIR/crates/loom-agent/src/pi/backend.rs" \
        "$LOOM_DIR/crates/loom-agent/src/claude/backend.rs"
    do
        if [ ! -f "$backend" ]; then
            continue
        fi
        if ! grep -qE '"run-bead"' "$backend"; then
            echo "$backend: missing \"run-bead\" arg — backend must spawn via wrapix wrapper" >&2
            return 1
        fi
    done
}

#-----------------------------------------------------------------------------
# test_no_panics_in_production — non-test Rust code under loom/crates/ contains
# no `unwrap()`, `expect(...)`, `todo!()`, `unimplemented!()`, or `panic!()`
# calls. Test code (`#[cfg(test)]` modules and files under `tests/`) is
# excluded; the workspace clippy block already denies these in production.
#-----------------------------------------------------------------------------
test_no_panics_in_production() {
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    local hits violations=0
    while IFS= read -r -d '' file; do
        # Skip integration test directories entirely.
        case "$file" in
            */tests/*) continue ;;
        esac
        # Strip `#[cfg(test)] mod tests { ... }` blocks before scanning so
        # unit tests under the same source file are excluded.
        local body
        body=$(awk '
            BEGIN { skip = 0; depth = 0 }
            /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/ { skip = 1; next }
            skip && /\{/ { depth += gsub(/\{/, "{") }
            skip && /\}/ {
                depth -= gsub(/\}/, "}")
                if (depth <= 0) { skip = 0; depth = 0; next }
            }
            !skip { print }
        ' "$file")
        hits=$(grep -nE '\b(unwrap|expect|todo|unimplemented|panic)\s*\(' <<<"$body" || true)
        # Filter out comment lines and the macro pattern in identifier/mod.rs
        # where `unwrap`/`panic` could appear in doc strings.
        hits=$(grep -vE '^[[:space:]]*[0-9]+:[[:space:]]*(//|/\*|\*)' <<<"$hits" || true)
        if [ -n "$hits" ]; then
            echo "panic-in-production candidate(s) in $file:" >&2
            echo "$hits" >&2
            violations=$((violations + 1))
        fi
    done < <(find "$LOOM_DIR/crates" -name '*.rs' -print0)
    return "$violations"
}

#-----------------------------------------------------------------------------
# test_no_allow_dead_code — non-test Rust code uses `#[expect(dead_code)]`,
# never `#[allow(dead_code)]` (NF-9). `expect` fails the build if the warning
# stops firing; `allow` silently rots.
#-----------------------------------------------------------------------------
test_no_allow_dead_code() {
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    local hits
    hits=$(grep -rEn '#\[allow\(dead_code\)\]' "$LOOM_DIR/crates" --include='*.rs' || true)
    if [ -n "$hits" ]; then
        echo "forbidden #[allow(dead_code)] (use #[expect(dead_code)] instead):" >&2
        echo "$hits" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_no_derive_from_on_newtypes — the newtype_id! macro and any newtype
# struct must not derive `From` or `Into` (NF-8: bypasses the newtype
# boundary). `#[from]` is permitted only on error enum variants — guarded by
# scoping the search to identifier/ submodules where the newtypes live.
#-----------------------------------------------------------------------------
test_no_derive_from_on_newtypes() {
    local id_dir="$LOOM_DIR/crates/loom-core/src/identifier"
    if [ ! -d "$id_dir" ]; then
        echo "loom-core identifier module not yet scaffolded" >&2
        return 77
    fi
    local hits
    # Look at every #[derive(...)] line under identifier/ for a From or Into
    # bare ident. Match on word boundaries so `TryFrom`, `IntoIterator`, etc.
    # don't false-positive (none of those are valid in derive lists anyway).
    hits=$(grep -rEn '#\[derive\([^)]*\b(From|Into)\b[^)]*\)\]' "$id_dir" --include='*.rs' || true)
    if [ -n "$hits" ]; then
        echo "forbidden derive(From) or derive(Into) on newtype:" >&2
        echo "$hits" >&2
        return 1
    fi
    # Same for the macro definition itself: the body must not splice From/Into
    # into the derive list.
    hits=$(awk '
        /macro_rules![[:space:]]*newtype_id/ { in_macro = 1 }
        in_macro && /#\[derive\(/ { print NR ": " $0 }
        in_macro && /^[[:space:]]*\}[[:space:]]*$/ { in_macro = 0 }
    ' "$id_dir/mod.rs" | grep -E '\b(From|Into)\b' || true)
    if [ -n "$hits" ]; then
        echo "newtype_id! macro derives From/Into:" >&2
        echo "$hits" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_askama_templates_compile — `cargo build -p loom-templates` succeeds.
# Askama runs its template parser at compile time, so a successful build is
# proof every template parsed and every typed context covers its variables.
#-----------------------------------------------------------------------------
test_askama_templates_compile() {
    cargo_run build -p loom-templates --quiet
}

#-----------------------------------------------------------------------------
# test_template_compile_time_check — every template file has a typed context
# struct under loom-templates/src/, and the crate's render integration tests
# (which exercise every Template::render impl) pass. A missing context field
# fails askama's derive at compile time, so a green run here proves the
# compile-time check is enforced.
#-----------------------------------------------------------------------------
test_template_compile_time_check() {
    local templates_dir="$LOOM_DIR/crates/loom-templates/templates"
    local missing=0 t
    if [ ! -d "$templates_dir" ]; then
        echo "loom-templates/templates not yet scaffolded" >&2
        return 77
    fi
    for t in plan_new plan_update todo_new todo_update run check msg; do
        if [ ! -f "$templates_dir/$t.md" ]; then
            echo "missing template: $templates_dir/$t.md" >&2
            missing=$((missing + 1))
        fi
    done
    cargo_run test -p loom-templates --test render --quiet
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_template_partials — every loom partial lives under templates/partial/
# and is referenced via askama's `{% include %}` (not the legacy `{{> name}}`
# syntax).
#-----------------------------------------------------------------------------
test_template_partials() {
    local partials_dir="$LOOM_DIR/crates/loom-templates/templates/partial"
    local templates_dir="$LOOM_DIR/crates/loom-templates/templates"
    local missing=0 p
    if [ ! -d "$partials_dir" ]; then
        echo "loom partials directory missing: $partials_dir" >&2
        return 1
    fi
    for p in context_pinning spec_header exit_signals companions_context; do
        if [ ! -f "$partials_dir/$p.md" ]; then
            echo "missing partial: $partials_dir/$p.md" >&2
            missing=$((missing + 1))
        fi
    done
    if grep -rE '\{\{>[[:space:]]+[a-z_-]+[[:space:]]*\}\}' "$templates_dir" >/dev/null; then
        echo "legacy {{> partial}} syntax found in loom templates (expected {% include %}):" >&2
        grep -rnE '\{\{>[[:space:]]+[a-z_-]+[[:space:]]*\}\}' "$templates_dir" >&2
        missing=$((missing + 1))
    fi
    if ! grep -rE '\{%[[:space:]]+include[[:space:]]+"partial/' "$templates_dir" >/dev/null; then
        echo "no askama include directives found in loom templates" >&2
        missing=$((missing + 1))
    fi
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_template_output_parity — the loom-templates render integration test
# `run_output_parity_with_ralph_for_shared_inputs` produces output that
# preserves every section and substituted value Ralph's bash renderer emits
# for the same inputs (modulo intentional drift: dropped legacy variables and
# the `ralph` → `loom` driver rename).
#-----------------------------------------------------------------------------
test_template_output_parity() {
    cargo_run test -p loom-templates --test render \
        run_output_parity_with_ralph_for_shared_inputs --quiet
}

#-----------------------------------------------------------------------------
# Workflow commands — each function dispatches into the matching cargo unit
# test under `loom-workflow`. Sharing the cargo binary keeps verify and
# `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
todo_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_todo_tier_detection — `compute_spec_diff` correctly classifies inputs
# into the four tiers from `lib/ralph/cmd/util.sh::compute_spec_diff`:
#   - Tier 1 (diff): molecule + valid base_commit → per-spec fan-out, with
#     sibling overrides and orphan fallback to the anchor's base.
#   - Tier 2 (tasks): molecule present but base_commit absent / orphaned /
#     missing → fall back to existing-task comparison.
#   - Tier 4 (new): no molecule recorded → fresh decomposition.
#   - --since override: replaces the anchor's base for the anchor only;
#     errors when the override commit is missing or orphaned.
# (Tier 3 — README discovery — is the driver's responsibility before
# `compute_spec_diff` runs; once it reconstructs a molecule, the call
# proceeds as tier 2.)
#-----------------------------------------------------------------------------
test_todo_tier_detection() {
    todo_cargo_test todo::tier::tests::tier_4_when_no_molecule_no_since
    todo_cargo_test todo::tier::tests::tier_2_when_molecule_present_without_base_commit
    todo_cargo_test todo::tier::tests::tier_2_when_base_commit_orphaned
    todo_cargo_test todo::tier::tests::tier_2_when_base_commit_no_longer_exists
    todo_cargo_test todo::tier::tests::tier_1_diff_with_empty_candidate_set
    todo_cargo_test todo::tier::tests::tier_1_anchor_only_diff
    todo_cargo_test todo::tier::tests::tier_1_fanout_uses_sibling_base_when_set
    todo_cargo_test todo::tier::tests::tier_1_fanout_seeds_orphaned_sibling_from_anchor
    todo_cargo_test todo::tier::tests::tier_1_skips_candidates_with_empty_diff
    todo_cargo_test todo::tier::tests::since_override_replaces_anchor_base_for_anchor_only
    todo_cargo_test todo::tier::tests::since_override_errors_when_commit_missing
    todo_cargo_test todo::tier::tests::since_override_errors_when_commit_orphaned
}

#-----------------------------------------------------------------------------
# State database — each function dispatches into the matching cargo
# integration test under `loom-core/tests/state_db.rs`. Sharing the cargo
# binary keeps verify and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
state_db_cargo_test() {
    cargo_run test -p loom-core --test state_db "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_state_db_init — `StateDb::open` creates `specs`, `molecules`,
# `companions`, and `meta` tables and seeds `meta.schema_version`.
#-----------------------------------------------------------------------------
test_state_db_init() {
    state_db_cargo_test state_db_init_creates_tables
}

#-----------------------------------------------------------------------------
# test_state_db_rebuild — `StateDb::rebuild` writes one specs row per
# spec markdown file and one molecules row per active molecule.
#-----------------------------------------------------------------------------
test_state_db_rebuild() {
    state_db_cargo_test state_db_rebuild_populates_specs_and_molecules
}

#-----------------------------------------------------------------------------
# test_state_db_rebuild_companions — `## Companions` parser extracts paths,
# specs without the section contribute zero rows, malformed lines skip.
#-----------------------------------------------------------------------------
test_state_db_rebuild_companions() {
    state_db_cargo_test state_db_rebuild_companions
}

#-----------------------------------------------------------------------------
# test_state_db_rebuild_resets_counters — `iteration_count` returns to 0
# after rebuild, even when previously incremented.
#-----------------------------------------------------------------------------
test_state_db_rebuild_resets_counters() {
    state_db_cargo_test state_db_rebuild_resets_counters
}

#-----------------------------------------------------------------------------
# test_state_current_spec — `set_current_spec` followed by `current_spec`
# round-trips the same `SpecLabel`.
#-----------------------------------------------------------------------------
test_state_current_spec() {
    state_db_cargo_test state_current_spec_round_trips
}

#-----------------------------------------------------------------------------
# test_state_increment_iteration — `increment_iteration` returns the post-
# increment value (1, 2, 3, ...).
#-----------------------------------------------------------------------------
test_state_increment_iteration() {
    state_db_cargo_test state_increment_iteration_returns_updated_count
}

#-----------------------------------------------------------------------------
# test_state_corruption_recovery — opening a non-SQLite blob fails; the
# `recreate()` recovery path replaces the file and rebuild succeeds.
#-----------------------------------------------------------------------------
test_state_corruption_recovery() {
    state_db_cargo_test state_corruption_recovery
}

#-----------------------------------------------------------------------------
# Beads CLI wrapper — each function dispatches into a unit test under
# loom-core/src/bd/client.rs::tests, so verify and `cargo test` exercise the
# same code paths. Tests substitute a `CapturingRunner` to keep the verify
# path independent of a real `bd` binary.
#-----------------------------------------------------------------------------
bd_client_cargo_test() {
    cargo_run test -p loom-core --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_bd_show_parsing — `bd show <id> --json` output deserializes into a
# typed `Bead` value with the expected fields.
#-----------------------------------------------------------------------------
test_bd_show_parsing() {
    bd_client_cargo_test bd::client::tests::show_parses_first_row_into_bead
}

#-----------------------------------------------------------------------------
# test_bd_list_parsing — `bd list --json` output deserializes into a Vec<Bead>
# and the `--status=` / `--label=` filters are forwarded to the CLI argv.
#-----------------------------------------------------------------------------
test_bd_list_parsing() {
    bd_client_cargo_test bd::client::tests::list_parses_array_of_beads
    bd_client_cargo_test bd::client::tests::list_filters_status_and_label
    bd_client_cargo_test bd::client::tests::list_handles_null_response_as_empty_vec
}

#-----------------------------------------------------------------------------
# test_bd_create_returns_id — `bd create --silent` stdout (a single id) is
# parsed back into a `BeadId`; blank stdout maps to `BdError::CreateMissingId`.
#-----------------------------------------------------------------------------
test_bd_create_returns_id() {
    bd_client_cargo_test bd::client::tests::create_returns_id_from_silent_output
    bd_client_cargo_test bd::client::tests::create_errors_on_blank_silent_output
}

#-----------------------------------------------------------------------------
# test_bd_error_handling — non-zero exits map to `BdError::Cli` with the
# argv + stderr captured; malformed JSON maps to `BdError::Decode`; missing
# rows map to `BdError::ShowEmpty`.
#-----------------------------------------------------------------------------
test_bd_error_handling() {
    bd_client_cargo_test bd::client::tests::cli_failure_maps_to_typed_error
    bd_client_cargo_test bd::client::tests::decode_failure_carries_args_context
    bd_client_cargo_test bd::client::tests::show_returns_show_empty_for_zero_rows
}

#-----------------------------------------------------------------------------
# Run-time logging — each function dispatches into a cargo integration test
# under `loom-core/tests/logging.rs`. Sharing the cargo binary keeps verify
# and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
logging_cargo_test() {
    cargo_run test -p loom-core --test logging "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_default_output_shape — default render mode prints one header line
# per bead and one short line per tool call (assistant text deltas
# suppressed); the closing line carries tool count + duration.
#-----------------------------------------------------------------------------
test_run_default_output_shape() {
    logging_cargo_test run_default_output_shape
}

#-----------------------------------------------------------------------------
# test_run_verbose_streams_text — verbose mode streams MessageDelta verbatim.
#-----------------------------------------------------------------------------
test_run_verbose_streams_text() {
    logging_cargo_test run_verbose_streams_text
}

#-----------------------------------------------------------------------------
# test_run_writes_per_bead_ndjson_log — every bead spawn writes the full
# AgentEvent stream as NDJSON to
# `<workspace>/.wrapix/loom/logs/<spec>/<bead>-<utc>.ndjson`, regardless of
# terminal verbosity.
#-----------------------------------------------------------------------------
test_run_writes_per_bead_ndjson_log() {
    logging_cargo_test run_writes_per_bead_ndjson_log
}

#-----------------------------------------------------------------------------
# test_run_logs_log_path — opening a sink emits an info-level tracing event
# whose `log_path` field carries the resolved file path.
#-----------------------------------------------------------------------------
test_run_logs_log_path() {
    logging_cargo_test run_logs_log_path
}

#-----------------------------------------------------------------------------
# test_parallel_logs_are_per_bead — running two beads against the same logs
# root writes two distinct files (per-bead, not per-session), and the
# contents never cross-contaminate even when `emit` is interleaved.
#-----------------------------------------------------------------------------
test_parallel_logs_are_per_bead() {
    logging_cargo_test parallel_logs_are_per_bead
}

#-----------------------------------------------------------------------------
# test_log_retention_sweep — `sweep_retention_at` deletes files older than
# `[logs] retention_days` and preserves recent files.
#-----------------------------------------------------------------------------
test_log_retention_sweep() {
    logging_cargo_test log_retention_sweep
}

#-----------------------------------------------------------------------------
# test_log_retention_disabled — `retention_days = 0` disables sweeping.
#-----------------------------------------------------------------------------
test_log_retention_disabled() {
    logging_cargo_test log_retention_disabled
}

#-----------------------------------------------------------------------------
# test_log_retention_failure_tolerance — per-file delete failures (here: a
# read-only directory) do not abort the sweep; survivors and failures are
# both surfaced in the report.
#-----------------------------------------------------------------------------
test_log_retention_failure_tolerance() {
    logging_cargo_test log_retention_failure_tolerance
}

#-----------------------------------------------------------------------------
# Concurrency & locking — each function dispatches into a cargo integration
# test under `loom-core/tests/lock_manager.rs` so verify and `cargo test`
# exercise the same paths. The acceptance behaviour (per-spec serialization,
# 5s timeout, cross-spec independence, read-only commands unblocked,
# init/workspace exclusion, crash recovery) is asserted in those tests.
#-----------------------------------------------------------------------------
lock_cargo_test() {
    cargo_run test -p loom-core --test lock_manager "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_per_spec_lock_acquired — `LockManager::acquire_spec` creates the per-
# spec lock file and the guard releases on drop.
#-----------------------------------------------------------------------------
test_per_spec_lock_acquired() {
    lock_cargo_test acquire_spec_creates_lock_file
}

#-----------------------------------------------------------------------------
# test_per_spec_lock_serializes — a second mutating command on the same spec
# waits up to 5s and then errors with `another loom command is operating on
# <label>`. The fast contention path (250ms timeout) and the default 5s wait
# are both asserted.
#-----------------------------------------------------------------------------
test_per_spec_lock_serializes() {
    lock_cargo_test second_acquire_times_out_with_spec_busy
    lock_cargo_test times_out_with_default_timeout
}

#-----------------------------------------------------------------------------
# test_cross_spec_no_blocking — locks for distinct spec labels do not block
# each other; both acquire effectively immediately.
#-----------------------------------------------------------------------------
test_cross_spec_no_blocking() {
    lock_cargo_test cross_spec_locks_do_not_block
}

#-----------------------------------------------------------------------------
# test_readonly_commands_unblocked — read-only commands acquire no lock; an
# active spec lock does not block read-only inspection of the workspace.
#-----------------------------------------------------------------------------
test_readonly_commands_unblocked() {
    lock_cargo_test readonly_paths_unaffected_by_spec_lock
}

#-----------------------------------------------------------------------------
# test_init_workspace_lock — `acquire_workspace` errors immediately with
# `WorkspaceBusy` if any per-spec lock is held; succeeds when none are; and
# is exclusive against itself.
#-----------------------------------------------------------------------------
test_init_workspace_lock() {
    lock_cargo_test acquire_workspace_errors_when_spec_lock_held
    lock_cargo_test acquire_workspace_serializes_workspace_holders
}

#-----------------------------------------------------------------------------
# test_crash_releases_lock — a crashed (process-exit) holder leaves no stale
# lock; a fresh invocation acquires immediately. The integration test spawns
# the cargo test binary as a child process, takes the lock, then exits via
# `std::process::exit` so the kernel — not Rust's Drop — releases the flock.
#-----------------------------------------------------------------------------
test_crash_releases_lock() {
    lock_cargo_test crash_releases_spec_lock
}

#-----------------------------------------------------------------------------
# loom run — each function dispatches into a cargo unit test under
# `loom-workflow/src/run/`. Sharing the cargo binary keeps verify and
# `cargo test` exercising the same code paths. The driver is exercised via
# the `AgentLoopController` trait so the tests never need a real container,
# bd binary, or `loom check` exec.
#-----------------------------------------------------------------------------
run_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_continuous — continuous mode pulls beads until `next_ready_bead`
# returns `None`, closes each on success, and execs `loom check` exactly once
# at molecule completion.
#-----------------------------------------------------------------------------
test_run_continuous() {
    run_cargo_test run::runner::tests::continuous_loops_until_molecule_complete
}

#-----------------------------------------------------------------------------
# test_run_once — `--once` processes a single bead then returns; subsequent
# ready beads remain in the queue and `loom check` is never invoked.
#-----------------------------------------------------------------------------
test_run_once() {
    run_cargo_test run::runner::tests::once_mode_processes_single_bead
    run_cargo_test run::runner::tests::once_mode_does_not_exec_check_on_empty_queue
}

#-----------------------------------------------------------------------------
# test_run_profile_selection — `resolve_profile` reads the bead's `profile:X`
# label, falls back to `base` without a label, and honours the CLI override.
#-----------------------------------------------------------------------------
test_run_profile_selection() {
    run_cargo_test run::profile::tests::resolve_profile_reads_label
    run_cargo_test run::profile::tests::resolve_profile_falls_back_to_base_without_label
    run_cargo_test run::profile::tests::resolve_profile_uses_override
    run_cargo_test run::profile::tests::resolve_profile_first_matching_label_wins
}

#-----------------------------------------------------------------------------
# test_run_retry_with_context — a failing bead retries with `previous_failure`
# threaded into the next attempt, gives up after `max_retries`, and the
# RetryPolicy decision math is asserted directly.
#-----------------------------------------------------------------------------
test_run_retry_with_context() {
    run_cargo_test run::retry::tests::default_policy_is_two_retries
    run_cargo_test run::retry::tests::retries_when_attempts_remain
    run_cargo_test run::retry::tests::gives_up_after_max_retries
    run_cargo_test run::retry::tests::zero_retries_gives_up_immediately
    run_cargo_test run::runner::tests::failed_bead_retries_with_previous_failure_then_clarifies
    run_cargo_test run::runner::tests::retry_succeeds_within_budget_and_closes
    run_cargo_test run::context::tests::retry_input_wraps_previous_failure
    run_cargo_test run::context::tests::rendered_retry_prompt_includes_previous_failure_body
}

#-----------------------------------------------------------------------------
# test_run_execs_check — molecule completion in continuous mode triggers
# exactly one `loom check` exec; once mode never does.
#-----------------------------------------------------------------------------
test_run_execs_check() {
    run_cargo_test run::runner::tests::continuous_execs_check_on_molecule_complete
    run_cargo_test run::runner::tests::once_mode_does_not_exec_check_on_empty_queue
}

#-----------------------------------------------------------------------------
# Worktree parallelism — `--parallel N`. Pure-logic tests live in the
# loom-workflow lib; tests that touch a real git repo live in the
# `parallel` integration test.
#-----------------------------------------------------------------------------
parallel_lib_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}
parallel_int_test() {
    cargo_run test -p loom-workflow --test parallel "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_parallel_flag_validation — `Parallelism::from_str` accepts positive
# integers and rejects 0, negatives, and non-integers with a clear error
# (before any work begins).
#-----------------------------------------------------------------------------
test_run_parallel_flag_validation() {
    parallel_lib_test run::parallelism::tests::default_is_one
    parallel_lib_test run::parallelism::tests::parse_accepts_positive_integers
    parallel_lib_test run::parallelism::tests::parse_rejects_zero_and_negatives_and_non_integers
    parallel_lib_test run::parallelism::tests::is_one_false_for_n_greater_than_one
    parallel_int_test run_parallel_flag_validation
}

#-----------------------------------------------------------------------------
# test_parallel_one_no_worktree — `--parallel 1` (default) does not create
# a worktree and works on the driver branch directly. The dispatch predicate
# is `Parallelism::is_one()`; the integration test pins it.
#-----------------------------------------------------------------------------
test_parallel_one_no_worktree() {
    parallel_int_test parallel_one_no_worktree
}

#-----------------------------------------------------------------------------
# test_parallel_creates_worktrees — `--parallel N > 1` creates one worktree
# per dispatched bead under `.wrapix/worktree/<label>/<bead-id>/` on a fresh
# branch `loom/<label>/<bead-id>` based on HEAD.
#-----------------------------------------------------------------------------
test_parallel_creates_worktrees() {
    parallel_int_test parallel_creates_worktrees
}

#-----------------------------------------------------------------------------
# test_parallel_concurrent_spawns — `run_concurrent_spawns` joins futures via
# `tokio::JoinSet` so wall-clock time for N concurrent dispatch slots is
# dominated by a single slot's work, not the sum.
#-----------------------------------------------------------------------------
test_parallel_concurrent_spawns() {
    parallel_lib_test run::parallel::tests::concurrent_spawns_overlap_in_wall_clock
    parallel_lib_test run::parallel::tests::concurrent_spawns_collect_outcomes_for_every_slot
}

#-----------------------------------------------------------------------------
# test_parallel_merge_back — successful bead branches are merged back to the
# driver branch sequentially after the batch completes; the per-bead worktree
# directory and branch are reclaimed on a clean merge.
#-----------------------------------------------------------------------------
test_parallel_merge_back() {
    parallel_int_test parallel_merge_back
}

#-----------------------------------------------------------------------------
# test_parallel_failure_cleanup — on agent failure the worktree branch is
# deleted and the bead is queued for retry per the retry policy
# (`BatchResult::AgentFailed` carries the error body the driver threads
# back into the next attempt as `previous_failure`).
#-----------------------------------------------------------------------------
test_parallel_failure_cleanup() {
    parallel_int_test parallel_failure_cleanup
}

#-----------------------------------------------------------------------------
# test_parallel_conflict_preserves_worktree — on merge conflict the worktree
# is preserved (not silently overwritten) and the bead is marked failed via
# `BatchResult::Conflict`. The branch is not deleted; the path on disk
# remains for human inspection.
#-----------------------------------------------------------------------------
test_parallel_conflict_preserves_worktree() {
    parallel_int_test parallel_conflict_preserves_worktree
}

#-----------------------------------------------------------------------------
# test_loom_run_smoke — end-to-end binary invocation. The spec acceptance
# criterion for wx-3hhwq.20 demands `loom run --once` against a fake bd return
# a meaningful exit code (not "unrecognized subcommand"). The integration test
# stubs bd to print `[]` for any --json query, seeds the state DB with a
# `current_spec`, then spawns the compiled `loom` binary and asserts exit 0
# plus the empty-queue summary line. A second test pins `loom run --help` so
# the regression where `run` is missing from the clap surface fails loudly.
#-----------------------------------------------------------------------------
test_loom_run_smoke() {
    cargo_run test -p loom --test run_smoke -- --nocapture --quiet
}

#-----------------------------------------------------------------------------
# loom check / loom msg — same dispatch pattern as run: each function pins one
# or more pure unit tests under `loom-workflow::{check,msg}`. The push-gate /
# auto-iterate decision logic is exercised through the `CheckController` trait
# under a `FakeController`, so no real container, bd binary, or `git push` is
# spawned by these tests.
#-----------------------------------------------------------------------------
check_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

msg_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_check_push_gate — clean review (no new beads, no clarify) pushes once
# and resets the iteration counter; a clarify present (new or pre-existing)
# stops the gate without pushing.
#-----------------------------------------------------------------------------
test_check_push_gate() {
    check_cargo_test check::runner::tests::clean_review_pushes_and_resets_counter
    check_cargo_test check::runner::tests::clarify_present_stops_without_pushing
    check_cargo_test check::runner::tests::pre_existing_clarify_blocks_push_even_when_no_new_beads
    check_cargo_test check::production::tests::beads_push_argv_invokes_beads_push_not_bd_dolt_push
}

#-----------------------------------------------------------------------------
# test_check_auto_iterate — fix-up beads under the iteration cap trigger an
# `exec loom run` with the counter incremented; reaching the cap escalates the
# newest fix-up bead to `ralph:clarify` instead of looping forever.
#-----------------------------------------------------------------------------
test_check_auto_iterate() {
    check_cargo_test check::iteration::tests::default_cap_matches_spec
    check_cargo_test check::iteration::tests::is_exhausted_true_at_or_above_cap
    check_cargo_test check::runner::tests::fix_up_beads_under_cap_auto_iterate
    check_cargo_test check::runner::tests::iteration_cap_escalates_newest_fix_up_to_clarify
}

#-----------------------------------------------------------------------------
# test_msg_list — clarify list filters to `ralph:clarify`-labelled beads,
# drops the SPEC column under a spec filter, and falls back to bead title when
# the `## Options — <summary>` header is missing.
#-----------------------------------------------------------------------------
test_msg_list() {
    msg_cargo_test msg::list::tests::filter_keeps_only_clarify_labelled_beads
    msg_cargo_test msg::list::tests::filter_with_spec_label_keeps_only_matching
    msg_cargo_test msg::list::tests::rows_drop_spec_column_under_filter
    msg_cargo_test msg::list::tests::rows_carry_spec_column_when_unfiltered
    msg_cargo_test msg::list::tests::summary_prefers_options_header_over_title
    msg_cargo_test msg::list::tests::summary_falls_back_to_title_when_header_absent
    msg_cargo_test msg::context::tests::rendered_msg_template_lists_each_clarify
}

#-----------------------------------------------------------------------------
# test_msg_fast_reply — `-a <choice>` resolves a pure-integer to the matching
# `### Option <N>` per the Options Format Contract; a missing index errors
# with the available indices; non-integer choice is stored verbatim.
#-----------------------------------------------------------------------------
test_msg_fast_reply() {
    msg_cargo_test msg::options::tests::options_em_dash_summary_and_three_options
    msg_cargo_test msg::options::tests::separator_variants_all_strip_cleanly
    msg_cargo_test msg::reply::tests::integer_choice_resolves_to_option_note
    msg_cargo_test msg::reply::tests::missing_option_index_errors_with_available_list
    msg_cargo_test msg::reply::tests::verbatim_string_passes_through_unchanged
    msg_cargo_test msg::reply::tests::integer_with_no_options_section_errors
    msg_cargo_test msg::reply::tests::empty_title_or_body_renders_partial_note
}

#-----------------------------------------------------------------------------
# Auxiliary commands (init, status, use, logs, spec) — each function dispatches
# into a unit test under loom-workflow/src/. Sharing the cargo binary keeps the
# verify path and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
aux_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_init_creates_state — `loom init` creates `.wrapix/loom/config.toml`
# (round-trips through `LoomConfig::default()`) and `.wrapix/loom/state.db`,
# preserving an existing config file on subsequent invocations.
#-----------------------------------------------------------------------------
test_init_creates_state() {
    aux_cargo_test init::tests::run_creates_config_and_state_db
    aux_cargo_test init::tests::run_preserves_existing_config_file
}

#-----------------------------------------------------------------------------
# test_init_rebuild — `loom init --rebuild` drops and repopulates the state
# DB from `specs/*.md` plus the supplied molecule slice, and resets every
# `iteration_count` to 0.
#-----------------------------------------------------------------------------
test_init_rebuild() {
    aux_cargo_test init::tests::rebuild_drops_and_repopulates_state_db
}

#-----------------------------------------------------------------------------
# test_status_command — `loom status` (read-only) renders `<unset>` when no
# spec has been chosen, otherwise prints the active spec, molecule id, and
# iteration counter. Sanity check confirms the call needs no lock.
#-----------------------------------------------------------------------------
test_status_command() {
    aux_cargo_test status::tests::empty_state_reports_unset_spec
    aux_cargo_test status::tests::populated_state_reports_label_and_iteration
    aux_cargo_test status::tests::no_lock_required_to_call_load
}

#-----------------------------------------------------------------------------
# test_use_command — `loom use <label>` acquires the per-spec lock, writes
# `current_spec` to the state DB, and round-trips with `status::load`. A
# spec lock held elsewhere causes `SpecBusy` after the configured timeout.
#-----------------------------------------------------------------------------
test_use_command() {
    aux_cargo_test use_spec::tests::use_round_trips_with_status_load
    aux_cargo_test use_spec::tests::use_acquires_per_spec_lock
}

#-----------------------------------------------------------------------------
# test_logs_command — `loom logs` (read-only) walks `.wrapix/loom/logs/` two
# levels deep, returns the most recent `*.ndjson`, applies an exact bead-id
# prefix filter so `wx-1` does not collapse into `wx-10`, and rejects
# non-ndjson files.
#-----------------------------------------------------------------------------
test_logs_command() {
    aux_cargo_test logs_cmd::tests::empty_root_returns_no_logs
    aux_cargo_test logs_cmd::tests::returns_most_recent_log_across_specs
    aux_cargo_test logs_cmd::tests::bead_filter_matches_prefix_exactly
    aux_cargo_test logs_cmd::tests::missing_bead_filter_returns_typed_error
    aux_cargo_test logs_cmd::tests::ignores_non_ndjson_files
}

#-----------------------------------------------------------------------------
# test_spec_query — `loom spec` parses `## Success Criteria` checkboxes and
# pairs each with the following `[verify](path#fn)` / `[judge](path#fn)`
# annotation. Fenced code blocks, the next `##` heading, and orphan
# checkboxes (no annotation) are all handled per `parse_spec_annotations` in
# `lib/ralph/cmd/util.sh`.
#-----------------------------------------------------------------------------
test_spec_query() {
    aux_cargo_test spec::annotations::tests::returns_no_criteria_error_when_section_missing
    aux_cargo_test spec::annotations::tests::pairs_checkbox_with_following_verify_link
    aux_cargo_test spec::annotations::tests::checked_box_propagates
    aux_cargo_test spec::annotations::tests::criterion_without_annotation_yields_none_kind
    aux_cargo_test spec::annotations::tests::fenced_code_blocks_are_skipped
    aux_cargo_test spec::annotations::tests::next_h2_terminates_section
    aux_cargo_test spec::annotations::tests::relative_paths_normalize_against_spec_dir
    aux_cargo_test spec::annotations::tests::legacy_double_colon_separator_supported
    aux_cargo_test spec::tests::list_for_label_reads_spec_under_workspace
}

#-----------------------------------------------------------------------------
# test_spec_deps — `loom spec --deps` mirrors `ralph sync --deps`: it scans
# every `[verify]`/`[judge]` test file for known tool invocations (curl, jq,
# rg, tmux, ssh, etc.), collapses aliases (`rg`/`ripgrep`, `ssh`/`scp`) to a
# single nixpkgs name, and ignores substring matches such as "curling".
#-----------------------------------------------------------------------------
test_spec_deps() {
    aux_cargo_test spec::deps::tests::maps_known_tools_to_nix_packages
    aux_cargo_test spec::deps::tests::aliases_collapse_to_canonical_package
    aux_cargo_test spec::deps::tests::ignores_substring_matches
    aux_cargo_test spec::deps::tests::matches_after_pipes_and_command_subst
    aux_cargo_test spec::deps::tests::ssh_and_scp_both_map_to_openssh
    aux_cargo_test spec::deps::tests::collect_deps_ignores_missing_files
    aux_cargo_test spec::deps::tests::collect_deps_skips_non_test_annotations
    aux_cargo_test spec::tests::deps_for_label_aggregates_across_test_files
}

#-----------------------------------------------------------------------------
# test_no_sync_or_tune_command — the loom binary must NOT expose `sync` or
# `tune` subcommands. Askama compiled templates make per-project sync
# unnecessary (see `specs/loom-harness.md`). The check greps the binary's
# clap surface; if either name shows up as a subcommand identifier, the
# binary has regressed.
#-----------------------------------------------------------------------------
test_no_sync_or_tune_command() {
    local main="$LOOM_DIR/crates/loom/src/main.rs"
    if [ ! -f "$main" ]; then
        echo "loom binary not yet scaffolded" >&2
        return 77
    fi
    local hits
    hits=$(grep -nE '#\[command\(name[[:space:]]*=[[:space:]]*"(sync|tune)"\)\]|^\s*Sync\b|^\s*Tune\b' "$main" || true)
    if [ -n "$hits" ]; then
        echo "forbidden sync/tune subcommand surfaced in $main:" >&2
        echo "$hits" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_plan_new — `loom plan -n <label>` renders the new-spec template, shells
# out to interactive `wrapix run` (NOT `run-bead --stdio`), waits for the
# session to exit, then re-parses `## Companions` from the spec markdown the
# interview wrote and replaces the companion rows for `<label>` in state.db.
#-----------------------------------------------------------------------------
test_plan_new() {
    aux_cargo_test plan::runner::tests::plan_new_invokes_wrapix_run_and_records_companions
    aux_cargo_test plan::runner::tests::plan_new_errors_when_interview_writes_no_spec
    aux_cargo_test plan::runner::tests::plan_new_flags_missing_companions_section
    aux_cargo_test plan::args::tests::parse_mode_accepts_new_only
    aux_cargo_test plan::args::tests::parse_mode_rejects_no_flags
    aux_cargo_test plan::args::tests::parse_mode_rejects_both_flags
}

#-----------------------------------------------------------------------------
# test_plan_update — `loom plan -u <label>` requires the spec to already
# exist, threads the existing companion rows into the update template, and
# reconciles companions from the spec markdown after the interactive session
# exits.
#-----------------------------------------------------------------------------
test_plan_update() {
    aux_cargo_test plan::runner::tests::plan_update_threads_existing_companions_into_prompt
    aux_cargo_test plan::runner::tests::plan_update_errors_when_spec_missing
    aux_cargo_test plan::args::tests::parse_mode_accepts_update_only
    aux_cargo_test plan::companions::tests::rerun_replaces_previous_rows
}

#-----------------------------------------------------------------------------
# test_plan_uses_interactive_wrapix_run — `loom plan` must shell out to the
# interactive `wrapix run` subcommand with the user's TTY attached. It must
# NEVER use `wrapix run-bead`, NEVER pass `--stdio`, and NEVER pass
# `--spawn-config` — those are reserved for the NDJSON-driven phases.
#-----------------------------------------------------------------------------
test_plan_uses_interactive_wrapix_run() {
    aux_cargo_test plan::command::tests::argv_starts_with_run_subcommand
    aux_cargo_test plan::command::tests::argv_passes_prompt_to_claude_with_skip_permissions
    aux_cargo_test plan::command::tests::argv_never_contains_run_bead_or_stdio_or_spawn_config
    aux_cargo_test plan::runner::tests::plan_acquires_per_spec_lock
}

#-----------------------------------------------------------------------------
# Agent backend trait surface — pin the loom-core types and modules that
# loom-agent depends on. Each grep test lives next to the file under test so
# the failure message points directly at the source.
#-----------------------------------------------------------------------------

#-----------------------------------------------------------------------------
# test_agent_trait_exists — `pub trait AgentBackend` declared in
# loom-core/src/agent/backend.rs with a single `spawn` associated function
# returning `impl Future<Output = Result<AgentSession<Idle>, ProtocolError>>
# + Send`. There must be no `SUPPORTS_STEERING` constant: both backends
# support steering (pi via `steer`, claude via stream-json user messages).
#-----------------------------------------------------------------------------
test_agent_trait_exists() {
    local f="$LOOM_DIR/crates/loom-core/src/agent/backend.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    grep -E '^pub trait AgentBackend' "$f" >/dev/null \
        || { echo "AgentBackend trait declaration missing in $f" >&2; return 1; }
    grep -E 'fn spawn[[:space:]]*\(' "$f" >/dev/null \
        || { echo "AgentBackend::spawn signature missing in $f" >&2; return 1; }
    grep -E 'AgentSession<Idle>[[:space:]]*,[[:space:]]*ProtocolError' "$f" >/dev/null \
        || { echo "spawn return type AgentSession<Idle>, ProtocolError missing in $f" >&2; return 1; }
    grep -E 'impl[[:space:]]+(std::)?future::Future' "$f" >/dev/null \
        || { echo "spawn does not return impl Future in $f" >&2; return 1; }
    grep -E '\+[[:space:]]*Send' "$f" >/dev/null \
        || { echo "spawn future is not + Send in $f" >&2; return 1; }
    if grep -E '\bSUPPORTS_STEERING\b' "$f" >/dev/null; then
        echo "AgentBackend declares forbidden SUPPORTS_STEERING constant in $f" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_agent_trait_static_dispatch — the loom-agent compile-only dispatch
# test (`tests/static_dispatch.rs`) instantiates `run_agent::<PiBackend>` and
# `run_agent::<ClaudeBackend>`; `cargo build --workspace --tests` succeeding
# is the assertion that the trait surface accepts both ZST backends.
#-----------------------------------------------------------------------------
test_agent_trait_static_dispatch() {
    local f="$LOOM_DIR/crates/loom-agent/tests/static_dispatch.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    grep -E 'run_agent::<PiBackend>' "$f" >/dev/null \
        || { echo "static_dispatch.rs does not reference run_agent::<PiBackend>" >&2; return 1; }
    grep -E 'run_agent::<ClaudeBackend>' "$f" >/dev/null \
        || { echo "static_dispatch.rs does not reference run_agent::<ClaudeBackend>" >&2; return 1; }
    cargo_run build --workspace --tests --quiet
}

#-----------------------------------------------------------------------------
# test_agent_event_variants — loom-core/src/agent/event.rs declares every
# spec-listed `AgentEvent` variant.
#-----------------------------------------------------------------------------
test_agent_event_variants() {
    local f="$LOOM_DIR/crates/loom-core/src/agent/event.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    if ! grep -E '^pub enum AgentEvent' "$f" >/dev/null; then
        echo "AgentEvent enum declaration missing in $f" >&2
        return 1
    fi
    local missing=0 v
    for v in MessageDelta ToolCall ToolResult TurnEnd SessionComplete \
             CompactionStart CompactionEnd Error; do
        if ! grep -E "^[[:space:]]+${v}([[:space:]]|\{|,)" "$f" >/dev/null; then
            echo "AgentEvent::${v} missing in $f" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_spawn_config_fields — loom-core/src/agent/backend.rs declares every
# spec-listed `SpawnConfig` field. The struct is the stable JSON contract
# between loom and `wrapix run-bead --spawn-config`.
#-----------------------------------------------------------------------------
test_spawn_config_fields() {
    local f="$LOOM_DIR/crates/loom-core/src/agent/backend.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    if ! grep -E '^pub struct SpawnConfig' "$f" >/dev/null; then
        echo "SpawnConfig struct missing in $f" >&2
        return 1
    fi
    local missing=0 field
    for field in image workspace env initial_prompt agent_args repin; do
        if ! grep -E "^[[:space:]]+pub ${field}[[:space:]]*:" "$f" >/dev/null; then
            echo "SpawnConfig.${field} missing in $f" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_typestate_transitions — loom-core/src/agent/session.rs splits the
# session API by typestate: `impl AgentSession<Idle>` exposes `prompt`;
# `impl AgentSession<Active>` exposes `next_event`, `steer`, and `abort`.
# Together they enforce the Idle → Active → Idle protocol order at compile
# time.
#-----------------------------------------------------------------------------
test_typestate_transitions() {
    local f="$LOOM_DIR/crates/loom-core/src/agent/session.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    local missing=0
    if ! awk '
        /^impl AgentSession<Idle>/ { idle = 1; next }
        idle && /^\}/ { idle = 0 }
        idle && /pub async fn prompt[[:space:]]*\(/ { found = 1 }
        END { exit (found ? 0 : 1) }
    ' "$f"; then
        echo "AgentSession<Idle>::prompt missing in $f" >&2
        missing=$((missing + 1))
    fi
    local m
    for m in next_event steer abort; do
        if ! awk -v needle="pub async fn ${m}" '
            /^impl AgentSession<Active>/ { active = 1; next }
            active && /^\}/ { active = 0 }
            active && index($0, needle) > 0 { found = 1 }
            END { exit (found ? 0 : 1) }
        ' "$f"; then
            echo "AgentSession<Active>::${m} missing in $f" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_protocol_error_variants — loom-core/src/agent/error.rs declares every
# spec-listed `ProtocolError` variant. The error covers the layers where
# loom-core is the only code that knows about the wire (line framing, JSON
# parse, subprocess IO) plus the small set of semantic outcomes a backend
# `LineParse` reports back upward.
#-----------------------------------------------------------------------------
test_protocol_error_variants() {
    local f="$LOOM_DIR/crates/loom-core/src/agent/error.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    if ! grep -E '^pub enum ProtocolError' "$f" >/dev/null; then
        echo "ProtocolError enum declaration missing in $f" >&2
        return 1
    fi
    local missing=0 v
    for v in InvalidJson UnknownMessageType Io ProcessExit UnexpectedEof \
             LineTooLong Unsupported; do
        if ! grep -E "^[[:space:]]+${v}([[:space:]]|\(|\{|,)" "$f" >/dev/null; then
            echo "ProtocolError::${v} missing in $f" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_claude_stream_json_parsing — parser deserializes the four primary
# claude stream-json line types (system/init, assistant text+tool_use, user
# tool_result, result/success) into their typed shapes. Covered by unit
# tests in claude/parser.rs that drive `ClaudeMessage` through fixtures.
#-----------------------------------------------------------------------------
test_claude_stream_json_parsing() {
    cargo_run test -p loom-agent --quiet -- \
        claude::parser::tests::parses_system_init \
        claude::parser::tests::parses_assistant_text_and_tool_use \
        claude::parser::tests::parses_user_tool_result_string_content \
        claude::parser::tests::parses_result_success
}

#-----------------------------------------------------------------------------
# test_claude_tagged_enum — `ClaudeMessage` uses serde's internally-tagged
# enum form (`#[serde(tag = "type")]`) so dispatch is driven by the wire
# `type` field rather than a hand-rolled match.
#-----------------------------------------------------------------------------
test_claude_tagged_enum() {
    local f="$LOOM_DIR/crates/loom-agent/src/claude/messages.rs"
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        return 1
    fi
    if ! awk '
        /#\[serde\(tag[[:space:]]*=[[:space:]]*"type"\)\]/ { tagged = 1; next }
        tagged && /^pub enum ClaudeMessage/ { found = 1; exit }
        /^pub enum ClaudeMessage/ { tagged = 0 }
        END { exit (found ? 0 : 1) }
    ' "$f"; then
        echo "ClaudeMessage is not annotated with #[serde(tag = \"type\")] in $f" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_claude_event_mapping — `result/success` yields `TurnEnd` then
# `SessionComplete`; `result/error` yields `Error` then `SessionComplete`.
# Covered by fixture-driven unit tests in claude/parser.rs.
#-----------------------------------------------------------------------------
test_claude_event_mapping() {
    cargo_run test -p loom-agent --quiet -- \
        claude::parser::tests::result_success_yields_turn_end_then_session_complete \
        claude::parser::tests::result_error_yields_error_then_session_complete
}

#-----------------------------------------------------------------------------
# test_claude_cost_capture — a `result` event with `total_cost_usd: 0.42`
# produces a `SessionComplete` whose `cost_usd` is `Some(0.42)`. Covered by
# unit tests in claude/parser.rs.
#-----------------------------------------------------------------------------
test_claude_cost_capture() {
    cargo_run test -p loom-agent --quiet -- \
        claude::parser::tests::result_event_captures_cost_usd \
        claude::parser::tests::result_event_without_cost_yields_none
}

#-----------------------------------------------------------------------------
# test_claude_unknown_events — a line with an unrecognised `type` value
# returns a `ParsedLine` with empty events (forward compatibility via
# `#[serde(other)]`), not an error. Covered by a unit test in
# claude/parser.rs.
#-----------------------------------------------------------------------------
test_claude_unknown_events() {
    cargo_run test -p loom-agent --quiet -- \
        claude::parser::tests::unknown_message_type_returns_empty_events
}

#-----------------------------------------------------------------------------
# test_claude_permission_autoapprove — a `control_request` line yields a
# `ParsedLine::response` carrying `control_response { id, approved: true }`
# when the deny-list is empty; with `denied_tools = ["WebFetch"]`, a
# WebFetch request yields `approved: false` while other tools remain
# auto-approved. Covered by unit tests in claude/parser.rs.
#-----------------------------------------------------------------------------
test_claude_permission_autoapprove() {
    cargo_run test -p loom-agent --quiet -- \
        claude::parser::tests::control_request_autoapproves_when_denylist_empty \
        claude::parser::tests::control_request_denied_when_tool_in_denylist \
        claude::parser::tests::control_request_denylist_does_not_affect_other_tools
}

#-----------------------------------------------------------------------------
# test_claude_repin_files — `ClaudeBackend` writes `repin.sh` and
# `claude-settings.json` under `<workspace>/.wrapix/loom/runtime/` plus the
# serialized `SpawnConfig` JSON before the launcher exec. Covered by a unit
# test in claude/backend.rs that exercises `prepare_runtime` against a
# tempdir workspace.
#-----------------------------------------------------------------------------
test_claude_repin_files() {
    cargo_run test -p loom-agent --quiet -- \
        claude::backend::tests::prepare_runtime_writes_repin_files_and_spawn_config
}

#-----------------------------------------------------------------------------
# test_claude_supports_steering — the driver writes a stream-json user
# message on stdin during the session via `AgentSession::steer`; the mock
# claude observes it and emits an additional assistant turn that echoes
# the steered payload, proving the wire round-trip. Covered by a unit
# test in claude/backend.rs that drives a bash mock under
# tests/loom/mock-claude/.
#-----------------------------------------------------------------------------
test_claude_supports_steering() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-claude/claude.sh" ]; then
        echo "missing mock claude binary: tests/loom/mock-claude/claude.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        claude::backend::tests::steering_message_reaches_mock_and_emits_followup_turn
}

#-----------------------------------------------------------------------------
# test_claude_shutdown_watchdog — after a `result` event the driver closes
# its end of stdin; if the agent ignores the close and SIGTERM, the
# watchdog escalates to SIGKILL. The mock under tests/loom/mock-claude/
# traps SIGTERM so the test exercises the full SIGTERM → SIGKILL path
# with an overridden small grace window.
#-----------------------------------------------------------------------------
test_claude_shutdown_watchdog() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-claude/claude.sh" ]; then
        echo "missing mock claude binary: tests/loom/mock-claude/claude.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        claude::backend::tests::shutdown_watchdog_escalates_to_sigkill_when_child_ignores_stdin_close
}

#-----------------------------------------------------------------------------
# test_pi_two_phase_deser — `PiParser::parse_line` peeks the envelope and
# re-deserializes the line into the matched concrete type. Exercised by
# fixtures covering an envelope-only line with unknown extras, a full
# response, a full event (id-absent path), and a full extension UI request.
#-----------------------------------------------------------------------------
test_pi_two_phase_deser() {
    cargo_run test -p loom-agent --quiet -- \
        pi::parser::tests::envelope_only_with_unknown_extras_classifies_as_event \
        pi::parser::tests::full_response_classifies_and_re_deserializes \
        pi::parser::tests::full_event_classifies_via_id_absent_path \
        pi::parser::tests::full_ui_request_classifies_as_extension_ui_request \
        pi::parser::tests::unknown_envelope_type_with_id_is_unknown_message_type
}

#-----------------------------------------------------------------------------
# test_pi_event_mapping — each pi event variant maps to the AgentEvent the
# spec table prescribes: message_update text/error deltas, tool_execution_*,
# turn_end, agent_end (synthesized exit_code 0), compaction_start reason
# coercion, compaction_end aborted flag, and trace-only events that yield no
# AgentEvents.
#-----------------------------------------------------------------------------
test_pi_event_mapping() {
    cargo_run test -p loom-agent --quiet -- \
        pi::parser::tests::message_update_text_delta_yields_message_delta \
        pi::parser::tests::message_update_error_delta_yields_error_event \
        pi::parser::tests::message_update_unmapped_delta_is_silent \
        pi::parser::tests::tool_execution_end_yields_tool_result \
        pi::parser::tests::tool_execution_end_stringifies_non_string_result \
        pi::parser::tests::turn_end_yields_turn_end_event \
        pi::parser::tests::agent_end_yields_session_complete_with_synthesized_zero \
        pi::parser::tests::compaction_start_threshold_maps_to_context_limit \
        pi::parser::tests::compaction_start_overflow_maps_to_context_limit \
        pi::parser::tests::compaction_start_manual_maps_to_user_requested \
        pi::parser::tests::compaction_start_unknown_reason_maps_to_unknown \
        pi::parser::tests::compaction_end_carries_aborted_flag \
        pi::parser::tests::observability_only_events_yield_no_agent_events \
        pi::parser::tests::unknown_event_type_via_serde_other_yields_no_events
}

#-----------------------------------------------------------------------------
# test_pi_malformed_ndjson — a garbage line returns
# `ProtocolError::InvalidJson`; the runner catches the error and continues
# (defensive — pi v0.72+ has stdout discipline via takeOverStdout).
#-----------------------------------------------------------------------------
test_pi_malformed_ndjson() {
    cargo_run test -p loom-agent --quiet -- \
        pi::parser::tests::malformed_json_returns_invalid_json_error
}

#-----------------------------------------------------------------------------
# test_pi_extension_ui_passthrough — an `extension_ui_request` whose method
# requires a host response (`select`/`confirm`/`input`/`editor`) yields a
# `ParsedLine::response` carrying `extension_ui_response { id, cancelled:
# true }`. Methods that do not block the agent (`notify`, `setStatus`)
# leave `ParsedLine::response` at None.
#-----------------------------------------------------------------------------
test_pi_extension_ui_passthrough() {
    cargo_run test -p loom-agent --quiet -- \
        pi::parser::tests::extension_ui_select_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_confirm_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_input_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_editor_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_notify_leaves_response_none \
        pi::parser::tests::extension_ui_set_status_leaves_response_none
}

#-----------------------------------------------------------------------------
# test_pi_startup_probe — `PiBackend::spawn` issues a `get_commands` request
# immediately after launching the subprocess and verifies the response
# carries every command Loom depends on (`prompt`, `steer`, `abort`,
# `set_model`). Missing commands surface as `ProtocolError::Unsupported`
# before any session work begins. The mock pi binary under
# tests/loom/mock-pi/ exposes happy-path and missing-command modes.
#-----------------------------------------------------------------------------
test_pi_startup_probe() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-pi/pi.sh" ]; then
        echo "missing mock pi binary: tests/loom/mock-pi/pi.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        pi::backend::tests::startup_probe_succeeds_when_required_commands_present \
        pi::backend::tests::startup_probe_fails_fast_when_required_command_missing
}

#-----------------------------------------------------------------------------
# test_pi_rpc_command_sending — the driver writes the initial prompt as an
# NDJSON line on stdin via `AgentSession::prompt`; the mock pi observes the
# line and echoes its `message` field back in a `message_update`/`text_delta`
# event, proving the wire shape.
#-----------------------------------------------------------------------------
test_pi_rpc_command_sending() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-pi/pi.sh" ]; then
        echo "missing mock pi binary: tests/loom/mock-pi/pi.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        pi::backend::tests::driver_sends_prompt_as_ndjson_line
}

#-----------------------------------------------------------------------------
# test_pi_supports_steering — `AgentSession::steer` writes a `steer` NDJSON
# line on stdin during an active session; the mock pi receives it,
# acknowledges it as a follow-up assistant turn, and the driver observes
# the corresponding `MessageDelta` + `TurnEnd` events.
#-----------------------------------------------------------------------------
test_pi_supports_steering() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-pi/pi.sh" ]; then
        echo "missing mock pi binary: tests/loom/mock-pi/pi.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        pi::backend::tests::driver_steers_mid_session_and_mock_observes_payload
}

#-----------------------------------------------------------------------------
# test_pi_compaction_repin — when the parser surfaces
# `AgentEvent::CompactionStart`, the driver (workflow-layer responsibility,
# exercised here by the test) sends `RePinContent::to_prompt` via
# `AgentSession::steer`. The mock pi observes the steer line and echoes the
# payload back as a `MessageDelta` so the test can verify the wire round-trip.
#-----------------------------------------------------------------------------
test_pi_compaction_repin() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-pi/pi.sh" ]; then
        echo "missing mock pi binary: tests/loom/mock-pi/pi.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        pi::backend::tests::driver_repins_on_compaction_start_via_steer
}

#-----------------------------------------------------------------------------
# test_pi_set_model_from_phase_config — `SpawnConfig::model` carries a
# provider/model_id pair sourced from per-phase config (`[agent.todo]`).
# `PiBackend::spawn` sends the matching `set_model { provider, modelId }`
# RPC after the startup probe; the mock pi acknowledges and echoes the
# fields back so the test can confirm both reached the agent.
#-----------------------------------------------------------------------------
test_pi_set_model_from_phase_config() {
    if [ ! -x "$REPO_ROOT/tests/loom/mock-pi/pi.sh" ]; then
        echo "missing mock pi binary: tests/loom/mock-pi/pi.sh" >&2
        return 1
    fi
    cargo_run test -p loom-agent --quiet -- \
        pi::backend::tests::set_model_from_phase_config_reaches_mock_pi
}

#-----------------------------------------------------------------------------
# test_per_phase_backend_config — config with [agent] default='claude' and
# [agent.todo] backend='pi' must resolve agent_for(Todo) to Pi and
# agent_for(Run) to Claude. Exercised in loom-core's config tests.
#-----------------------------------------------------------------------------
test_per_phase_backend_config() {
    cargo_run test -p loom-core --quiet -- \
        config::tests::agent_for_per_phase_resolves_override_and_default
}

#-----------------------------------------------------------------------------
# test_backend_selection_flag — `loom run --agent pi` overrides phase config.
# Verified at the binary surface: `loom --help` lists --agent, the value-enum
# accepts pi, and the global flag flows into `run`'s help.
#-----------------------------------------------------------------------------
test_backend_selection_flag() {
    cargo_run test -p loom --test agent_flag --quiet -- \
        loom_help_lists_agent_global_flag \
        loom_run_help_includes_agent_flag \
        loom_accepts_agent_pi \
        loom_accepts_agent_claude
}

#-----------------------------------------------------------------------------
# test_backend_default_claude — empty config + no flag yields claude for
# every phase (Plan/Todo/Run/Check/Msg).
#-----------------------------------------------------------------------------
test_backend_default_claude() {
    cargo_run test -p loom-core --quiet -- \
        config::tests::agent_for_default_is_claude_when_config_empty
}

#-----------------------------------------------------------------------------
# test_backend_invalid_name — `loom --agent unknown status` fails with a
# clap error that names the offending value and lists the valid choices.
# Also verifies the config-side `UnknownBackend` error path.
#-----------------------------------------------------------------------------
test_backend_invalid_name() {
    cargo_run test -p loom --test agent_flag --quiet -- \
        loom_rejects_unknown_agent_value
    cargo_run test -p loom-core --quiet -- \
        config::tests::agent_for_unknown_backend_in_default_returns_error \
        config::tests::agent_for_unknown_backend_in_phase_override_isolated_to_that_phase
}

#-----------------------------------------------------------------------------
# Agent runtime layer — verifies the two-axis composition in
# lib/sandbox/image.nix. The pi runtime layer adds Node.js + pi binary on top
# of any workspace profile; the claude runtime layer is a no-op (claude is
# already in the base image).
#
# Tests inspect the realized image closure rather than booting a container,
# so they run on any host that can build a Linux image (Linux directly,
# Darwin via the remote linux-builder).
#-----------------------------------------------------------------------------

# Build an image and emit its full closure (one store path per line).
image_closure() {
    local attr="$1" out
    out=$(nix build "$REPO_ROOT#$attr" --no-link --print-out-paths) || return 1
    nix-store --query --requisites "$out"
}

#-----------------------------------------------------------------------------
# test_pi_runtime_layer — pi runtime adds Node.js + pi binary to whatever
# workspace profile is selected. Verified by inspecting the image closure
# for both the pi-mono package and a nodejs derivation.
#-----------------------------------------------------------------------------
test_pi_runtime_layer() {
    local closure
    closure=$(image_closure sandbox-pi) || return 1
    if ! grep -q -- '-pi-mono-' <<<"$closure"; then
        echo "pi-mono not in sandbox-pi closure (pi runtime layer missing)" >&2
        return 1
    fi
    if ! grep -qE -- '-nodejs(-[0-9]|_)' <<<"$closure"; then
        echo "nodejs not in sandbox-pi closure (pi runtime layer missing)" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_pi_rust_composition — composing profile:rust with agent=pi yields a
# buildable image. Verifies the two axes compose without name collisions or
# missing dependencies.
#-----------------------------------------------------------------------------
test_pi_rust_composition() {
    local closure
    closure=$(image_closure sandbox-rust-pi) || return 1
    grep -q -- '-pi-mono-' <<<"$closure" \
        || { echo "pi-mono missing from sandbox-rust-pi closure" >&2; return 1; }
    # Rust profile signature: cargo (or rustc) must remain present.
    grep -qE -- '-(cargo|rustc)-' <<<"$closure" \
        || { echo "rust toolchain missing from sandbox-rust-pi closure" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_pi_base_composition — composing profile:base with agent=pi builds.
#-----------------------------------------------------------------------------
test_pi_base_composition() {
    local closure
    closure=$(image_closure sandbox-pi) || return 1
    grep -q -- '-pi-mono-' <<<"$closure" \
        || { echo "pi-mono missing from sandbox-pi closure" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_pi_binary_in_container — the pi binary the runtime layer bundles is
# functional: `pi --version` succeeds. Exercised against the pi-mono store
# path that the layer pulls in.
#-----------------------------------------------------------------------------
test_pi_binary_in_container() {
    local pi_pkg
    pi_pkg=$(nix build "$REPO_ROOT#pi-mono" --no-link --print-out-paths) || return 1
    [ -x "$pi_pkg/bin/pi" ] \
        || { echo "pi binary missing at $pi_pkg/bin/pi" >&2; return 1; }
    "$pi_pkg/bin/pi" --version >/dev/null \
        || { echo "'pi --version' failed against $pi_pkg/bin/pi" >&2; return 1; }
    # The same pi-mono store path must appear in a pi-runtime image, otherwise
    # the layer is referencing a different binary than the one we just probed.
    local closure
    closure=$(image_closure sandbox-pi) || return 1
    grep -qF "$pi_pkg" <<<"$closure" \
        || { echo "pi binary $pi_pkg not in sandbox-pi closure" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_claude_runtime_noop — the default (claude) runtime adds nothing on top
# of the base image: pi-mono is absent, and claude itself is still present
# (sanity check that the base image's claude binary survived the refactor).
#-----------------------------------------------------------------------------
test_claude_runtime_noop() {
    local closure
    closure=$(image_closure sandbox) || return 1
    if grep -q -- '-pi-mono-' <<<"$closure"; then
        echo "pi-mono unexpectedly present in claude-runtime sandbox closure" >&2
        return 1
    fi
    grep -q -- '-claude-code-' <<<"$closure" \
        || { echo "claude-code missing from default sandbox closure (regression)" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_fuzz_runner_exists — `nix run .#fuzz-loom` is wired up on Linux.
# Per spec NFR §Property-Based Testing: cargo-fuzz is on-demand only and
# not gated by `nix flake check`. The acceptance is that the app
# attribute resolves; the fuzz targets themselves can land later.
#-----------------------------------------------------------------------------
test_fuzz_runner_exists() {
    case "$(uname -s)" in
        Linux) ;;
        *)
            # Linux-only: cargo-fuzz needs nightly LLVM sanitizers.
            return 77
            ;;
    esac

    local arch
    arch=$(uname -m)
    local nix_system
    case "$arch" in
        x86_64)  nix_system="x86_64-linux" ;;
        aarch64) nix_system="aarch64-linux" ;;
        *)
            echo "unsupported arch for fuzz-loom: $arch" >&2
            return 1
            ;;
    esac

    nix eval "$REPO_ROOT#apps.$nix_system.fuzz-loom.program" --raw >/dev/null \
        || { echo "nix eval .#apps.$nix_system.fuzz-loom failed" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_protocol_versions_pinned — both pi-mono and claude-code are pinned
# in modules/flake/overlays.nix with documentation. Per spec NFR #9:
# version bumps go through a checklist; the pin file is the single
# discoverable point reviewers grep when bumping either dependency.
#-----------------------------------------------------------------------------
test_protocol_versions_pinned() {
    local file="$REPO_ROOT/modules/flake/overlays.nix"
    [ -f "$file" ] || { echo "missing overlays.nix: $file" >&2; return 1; }

    grep -q "pi-mono" "$file" \
        || { echo "no pi-mono pin reference in $file" >&2; return 1; }
    grep -q "claude-code" "$file" \
        || { echo "no claude-code pin reference in $file" >&2; return 1; }
    grep -qiE "protocol[- ]bump checklist" "$file" \
        || { echo "no protocol-bump checklist comment in $file" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_flake_check_includes_loom — `nix flake check` runs the loom-tests
# derivation. Asserts `checks.<current-system>.loom-tests` evaluates to a
# store path produced by the loom-tests derivation. This is the gate that
# binds the unit + integration tier to CI.
#-----------------------------------------------------------------------------
test_flake_check_includes_loom() {
    local arch kernel nix_system store
    arch=$(uname -m)
    case "$(uname -s)" in
        Linux)  kernel="linux" ;;
        Darwin) kernel="darwin" ;;
        *)
            echo "unsupported kernel: $(uname -s)" >&2
            return 1
            ;;
    esac
    case "$arch" in
        x86_64)        nix_system="x86_64-$kernel" ;;
        aarch64|arm64) nix_system="aarch64-$kernel" ;;
        *)
            echo "unsupported arch: $arch" >&2
            return 1
            ;;
    esac

    store=$(nix eval --raw "$REPO_ROOT#checks.$nix_system.loom-tests.outPath") \
        || { echo "checks.$nix_system.loom-tests does not evaluate" >&2; return 1; }

    case "$store" in
        /nix/store/*-loom-tests-*) ;;
        *)
            echo "unexpected store path: $store" >&2
            return 1
            ;;
    esac
}

#-----------------------------------------------------------------------------
# test_flake_declares_loom_for_all_systems — per spec NFR #7 the unit +
# integration tier is cross-platform. Asserts checks.<system>.loom-tests
# evaluates for all four supported systems.
#-----------------------------------------------------------------------------
test_flake_declares_loom_for_all_systems() {
    local sys missing=0 errlog
    errlog=$(mktemp)
    trap 'rm -f "$errlog"' RETURN
    for sys in x86_64-linux aarch64-linux x86_64-darwin aarch64-darwin; do
        if ! nix eval --raw "$REPO_ROOT#checks.$sys.loom-tests.outPath" \
                >/dev/null 2>"$errlog"; then
            echo "checks.$sys.loom-tests not declared:" >&2
            sed 's/^/  /' "$errlog" >&2
            missing=$((missing + 1))
        fi
    done
    [ "$missing" -eq 0 ]
}

#-----------------------------------------------------------------------------
# test_cargo_nextest_timing — soft <5s warm-cache target for
# `cargo nextest run --workspace` per spec NFR #2. Guides PR review;
# returns 0 with a WARN on stderr when exceeded (not a hard CI fail).
# Skips when cargo or cargo-nextest is unavailable.
#-----------------------------------------------------------------------------
test_cargo_nextest_timing() {
    if ! command -v cargo >/dev/null; then
        echo "cargo not on PATH; skipping" >&2
        return 77
    fi
    if ! command -v cargo-nextest >/dev/null; then
        echo "cargo-nextest not on PATH; skipping" >&2
        return 77
    fi

    local log
    log=$(mktemp)
    trap 'rm -f "$log"' RETURN

    # Warm the cache so we measure run-time, not compile-time. Capture
    # output so failures surface their actual cause rather than getting
    # silenced.
    if ! ( cd "$LOOM_DIR" && cargo nextest run --workspace --no-run ) >"$log" 2>&1; then
        echo "warm-up build failed:" >&2
        sed 's/^/  /' "$log" >&2
        return 1
    fi

    local start_ns end_ns elapsed_ms
    start_ns=$(date +%s%N)
    if ! ( cd "$LOOM_DIR" && cargo nextest run --workspace ) >"$log" 2>&1; then
        echo "cargo nextest run --workspace failed:" >&2
        sed 's/^/  /' "$log" >&2
        return 1
    fi
    end_ns=$(date +%s%N)
    elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))

    echo "cargo nextest run --workspace: ${elapsed_ms}ms"
    if [ "$elapsed_ms" -gt 5000 ]; then
        echo "WARN: exceeds soft 5s target (${elapsed_ms}ms)" >&2
    fi
    return 0
}

#-----------------------------------------------------------------------------
# test_newtype_serde_roundtrip — every domain identifier newtype
# (`BeadId`, `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`,
# `ToolCallId`, `RequestId`) round-trips through serde JSON without a
# wrapper (`#[serde(transparent)]`) and rejects malformed input. Exercises
# the inline `#[cfg(test)] mod tests` blocks in
# `loom-core/src/identifier/*.rs`.
#-----------------------------------------------------------------------------
test_newtype_serde_roundtrip() {
    cargo_run test -p loom-core --lib --quiet -- \
        identifier::bead::tests::serde_round_trips_as_plain_string \
        identifier::bead::tests::deserialize_rejects_malformed_string \
        identifier::bead::tests::display_round_trips_with_as_str \
        identifier::bead::tests::parse_accepts_canonical_shapes \
        identifier::bead::tests::parse_rejects_malformed_inputs \
        identifier::spec::tests::serde_round_trips_as_plain_string \
        identifier::spec::tests::display_round_trips_with_as_str \
        identifier::molecule::tests::serde_round_trips_as_plain_string \
        identifier::molecule::tests::display_round_trips_with_as_str \
        identifier::profile::tests::serde_round_trips_as_plain_string \
        identifier::profile::tests::display_round_trips_with_as_str \
        identifier::session::tests::serde_round_trips_as_plain_string \
        identifier::session::tests::display_round_trips_with_as_str \
        identifier::tool_call::tests::serde_round_trips_as_plain_string \
        identifier::tool_call::tests::display_round_trips_with_as_str \
        identifier::request::tests::serde_round_trips_as_plain_string \
        identifier::request::tests::display_round_trips_with_as_str
}

#-----------------------------------------------------------------------------
# test_state_db_roundtrip — `StateDb` covers spec, molecule, companions, and
# meta-table operations end-to-end: open creates schema, rebuild populates
# from `specs/*.md` + mock molecule list, `current_spec` round-trips,
# `increment_iteration` returns the post-increment value (starting at 0
# after rebuild), and `recreate` recovers from a corrupted file. Each cargo
# integration test exercises one operation against a `tempfile::tempdir`.
#-----------------------------------------------------------------------------
test_state_db_roundtrip() {
    state_db_cargo_test state_db_init_creates_tables
    state_db_cargo_test state_db_rebuild_populates_specs_and_molecules
    state_db_cargo_test state_db_rebuild_companions
    state_db_cargo_test state_db_rebuild_resets_counters
    state_db_cargo_test state_current_spec_round_trips
    state_db_cargo_test state_increment_iteration_returns_updated_count
    state_db_cargo_test state_corruption_recovery
}

#-----------------------------------------------------------------------------
# test_pi_protocol_coverage — Pi RPC parser covers every documented field of
# every documented message type for the pinned protocol version (per spec
# NFR #9): two-phase deserialization, response success/failure with
# `data` / `error` field mapping, `tool_execution_start` / `_end` field
# mapping (`toolCallId` / `toolName` / `args` / `result` / `isError`),
# `message_update` nested delta dispatch (text, error, unmapped),
# compaction lifecycle reasons, observability-only events, malformed JSON,
# extension UI auto-cancel, and command (prompt/steer/abort) serialization.
#-----------------------------------------------------------------------------
test_pi_protocol_coverage() {
    cargo_run test -p loom-agent --lib --quiet -- \
        pi::messages::tests::pi_response_success_populates_data_field \
        pi::messages::tests::pi_response_failure_populates_error_field \
        pi::messages::tests::pi_response_minimal_shape_omits_data_and_error \
        pi::messages::tests::pi_event_tool_execution_start_maps_all_fields \
        pi::messages::tests::pi_event_tool_execution_end_maps_all_fields \
        pi::messages::tests::pi_ui_request_maps_id_and_method \
        pi::messages::tests::command_structs_serialize_to_expected_type_field \
        pi::parser::tests::envelope_only_with_unknown_extras_classifies_as_event \
        pi::parser::tests::full_response_classifies_and_re_deserializes \
        pi::parser::tests::full_event_classifies_via_id_absent_path \
        pi::parser::tests::full_ui_request_classifies_as_extension_ui_request \
        pi::parser::tests::unknown_envelope_type_with_id_is_unknown_message_type \
        pi::parser::tests::message_update_text_delta_yields_message_delta \
        pi::parser::tests::message_update_error_delta_yields_error_event \
        pi::parser::tests::message_update_unmapped_delta_is_silent \
        pi::parser::tests::tool_execution_end_yields_tool_result \
        pi::parser::tests::tool_execution_end_stringifies_non_string_result \
        pi::parser::tests::turn_end_yields_turn_end_event \
        pi::parser::tests::agent_end_yields_session_complete_with_synthesized_zero \
        pi::parser::tests::compaction_start_threshold_maps_to_context_limit \
        pi::parser::tests::compaction_start_overflow_maps_to_context_limit \
        pi::parser::tests::compaction_start_manual_maps_to_user_requested \
        pi::parser::tests::compaction_start_unknown_reason_maps_to_unknown \
        pi::parser::tests::compaction_end_carries_aborted_flag \
        pi::parser::tests::observability_only_events_yield_no_agent_events \
        pi::parser::tests::unknown_event_type_via_serde_other_yields_no_events \
        pi::parser::tests::malformed_json_returns_invalid_json_error \
        pi::parser::tests::extension_ui_select_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_confirm_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_input_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_editor_yields_auto_cancel_response \
        pi::parser::tests::extension_ui_notify_leaves_response_none \
        pi::parser::tests::extension_ui_set_status_leaves_response_none \
        pi::parser::tests::encode_prompt_emits_prompt_command \
        pi::parser::tests::encode_steer_emits_steer_command \
        pi::parser::tests::encode_abort_emits_abort_command_some
}

#-----------------------------------------------------------------------------
# test_claude_protocol_coverage — Claude stream-json parser covers every
# documented field of every documented message type:
# `#[serde(tag = "type")]` dispatch, `#[serde(other)]` → `Unknown` for
# forward compatibility, `Result` with all six fields (`subtype`, `result`,
# `total_cost_usd`, `duration_ms`, `num_turns`, `is_error`), `System` /
# `ControlRequest` / assistant / user block field mappings, control_request
# auto-approval keyed on the deny-list, and malformed-JSON handling.
#-----------------------------------------------------------------------------
test_claude_protocol_coverage() {
    cargo_run test -p loom-agent --lib --quiet -- \
        claude::parser::tests::parses_system_init \
        claude::parser::tests::parses_assistant_text_and_tool_use \
        claude::parser::tests::parses_user_tool_result_string_content \
        claude::parser::tests::parses_result_success \
        claude::parser::tests::result_success_yields_turn_end_then_session_complete \
        claude::parser::tests::result_error_yields_error_then_session_complete \
        claude::parser::tests::result_event_captures_cost_usd \
        claude::parser::tests::result_event_without_cost_yields_none \
        claude::parser::tests::unknown_message_type_returns_empty_events \
        claude::parser::tests::control_request_autoapproves_when_denylist_empty \
        claude::parser::tests::control_request_denied_when_tool_in_denylist \
        claude::parser::tests::control_request_denylist_does_not_affect_other_tools \
        claude::parser::tests::encode_prompt_emits_stream_json_user_message \
        claude::parser::tests::encode_steer_emits_same_shape_as_prompt \
        claude::parser::tests::encode_abort_returns_none \
        claude::parser::tests::result_message_round_trips_every_documented_field \
        claude::parser::tests::system_message_maps_subtype_and_session_id \
        claude::parser::tests::control_request_message_round_trips_all_fields \
        claude::parser::tests::assistant_block_text_and_tool_use_field_mapping \
        claude::parser::tests::user_block_tool_result_field_mapping \
        claude::parser::tests::malformed_json_returns_invalid_json_error
}

#-----------------------------------------------------------------------------
# test_template_rendering — every Askama template renders cleanly with
# representative inputs. Exercises the integration tests under
# `loom-templates/tests/render.rs`, which assert on shared sections,
# partials, agent-output wrapping, and `previous_failure` truncation.
#-----------------------------------------------------------------------------
test_template_rendering() {
    cargo_run test -p loom-templates --test render --quiet -- \
        plan_new_renders_partials_and_inputs \
        plan_update_renders_partials_and_companions \
        todo_new_renders_implementation_notes_when_present \
        todo_new_omits_implementation_notes_section_when_empty \
        todo_update_wraps_existing_tasks_in_agent_output \
        run_wraps_agent_supplied_fields_in_agent_output \
        run_renders_expected_sections_for_shared_inputs \
        previous_failure_truncates_at_max_len \
        previous_failure_preserves_short_input \
        check_renders_review_context_fields \
        msg_renders_clarify_beads_with_options \
        msg_renders_with_no_clarify_beads
}

#-----------------------------------------------------------------------------
# Property-based tests — `proptest` invariants under
# `loom/crates/{loom-core,loom-agent}/tests/properties.rs`. Each shell
# function dispatches into the matching cargo integration test. CI runs
# at `PROPTEST_CASES=32` (set in tests/loom/default.nix); local exhaustive
# runs override via env var (`PROPTEST_CASES=2048+`).
#-----------------------------------------------------------------------------

# JSONL line parser invariants: never panics on arbitrary bytes, never emits
# an `AgentEvent` from a malformed line, and `MAX_LINE_BYTES` matches the
# spec's 10 MB cap. Exercises both PiParser and ClaudeParser since the
# JSONL framing contract is shared between backends.
test_jsonl_parser_invariants() {
    cargo_run test -p loom-agent --test properties --quiet -- \
        max_line_bytes_is_ten_megabytes \
        jsonl_arbitrary_bytes_never_panic \
        jsonl_malformed_line_emits_no_events
}

# Pi protocol parser invariants: round-trip identity for known shapes
# (prompt/steer encoders), unknown message types surface as
# `ProtocolError::UnknownMessageType`, never panics on arbitrary bytes.
test_pi_parser_invariants() {
    cargo_run test -p loom-agent --test properties --quiet -- \
        pi_encode_prompt_round_trips \
        pi_encode_steer_round_trips \
        pi_unknown_message_type_surfaces_typed_error \
        pi_arbitrary_bytes_never_panic
}

# Claude stream-json parser invariants: round-trip identity for known
# shapes (System, Result, encoded user message), unknown shapes hit the
# `Unknown` variant via `#[serde(other)]`, never panics on arbitrary bytes.
test_claude_parser_invariants() {
    cargo_run test -p loom-agent --test properties --quiet -- \
        claude_system_round_trips \
        claude_result_round_trips \
        claude_unknown_type_falls_through_serde_other \
        claude_encode_prompt_round_trips \
        claude_arbitrary_bytes_never_panic
}

# State DB rebuild invariants: arbitrary spec content never corrupts the
# schema, `recreate` always recovers from a corrupted DB file, and
# round-trips of known molecule shapes are stable.
test_state_db_rebuild_invariants() {
    cargo_run test -p loom-core --test properties --quiet -- \
        rebuild_never_corrupts_schema \
        recreate_recovers_from_arbitrary_bytes \
        rebuild_round_trips_known_shapes
}

# `PROPTEST_CASES=32` is configured for CI in tests/loom/default.nix and
# is overridable via env var for local exhaustive runs (NFR §Property-
# Based Testing). The tests/loom/default.nix derivation is the single
# discoverable point; assert it sets the variable.
test_proptest_case_count() {
    local file="$REPO_ROOT/tests/loom/default.nix"
    [ -f "$file" ] || { echo "missing $file" >&2; return 1; }
    grep -qE 'PROPTEST_CASES *= *"32"' "$file" \
        || { echo "PROPTEST_CASES=32 not set in $file" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# Snapshot tests — `insta` snapshots for contract surfaces.
#
# Templates and CLI help are user-visible contracts where layout drift slips
# silently past substring assertions. Snapshots fail loudly on any text
# change; PR reviewers see the rendered diff. The run-time renderer is a
# flexibility surface and is excluded by `renderer_no_insta_dependency`.
#-----------------------------------------------------------------------------

# Every typed Askama context has at least one `.snap` under
# loom/crates/loom-templates/tests/snapshots/. Names are tied to the test
# function via insta's default `<test-binary>__<test-name>.snap` shape.
test_template_snapshots_exist() {
    local snap_dir="$LOOM_DIR/crates/loom-templates/tests/snapshots"
    [ -d "$snap_dir" ] || {
        echo "missing snapshot dir: $snap_dir" >&2
        return 1
    }
    local missing=0
    # Each typed context gets one snapshot exercising representative inputs.
    for ctx in plan_new plan_update todo_new todo_update run check msg; do
        local snap="$snap_dir/snapshots__${ctx}_snapshot.snap"
        if [ ! -f "$snap" ]; then
            echo "missing snapshot for ${ctx}: $snap" >&2
            missing=$((missing + 1))
        fi
    done
    [ "$missing" -eq 0 ] || return 1
    cargo_run test -p loom-templates --test snapshots --quiet
}

# Every `loom <subcommand> --help` surface has an insta snapshot. The list
# below mirrors the v1 command surface in `crates/loom/src/main.rs::Command`.
test_cli_help_snapshots_exist() {
    local snap_dir="$LOOM_DIR/crates/loom/tests/snapshots"
    [ -d "$snap_dir" ] || {
        echo "missing snapshot dir: $snap_dir" >&2
        return 1
    }
    local missing=0
    local commands=(
        loom_help
        loom_init_help
        loom_status_help
        loom_use_help
        loom_logs_help
        loom_spec_help
        loom_plan_help
        loom_run_help
        loom_check_help
        loom_msg_help
        loom_todo_help
    )
    for cmd in "${commands[@]}"; do
        local snap="$snap_dir/cli_help__${cmd}_snapshot.snap"
        if [ ! -f "$snap" ]; then
            echo "missing CLI help snapshot: $snap" >&2
            missing=$((missing + 1))
        fi
    done
    [ "$missing" -eq 0 ] || return 1
    cargo_run test -p loom --test cli_help --quiet
}

# Run-time renderer must NOT use `insta`. Per spec §Snapshot Testing, the
# renderer is a flexibility surface — substring + structural assertions keep
# layout decisions free to evolve. The Rust check lives at
# `loom/crates/loom/tests/style.rs::renderer_no_insta_dependency`.
test_renderer_no_insta_dependency() {
    cargo_run test -p loom --test style --quiet -- renderer_no_insta_dependency
}

#-----------------------------------------------------------------------------
# Container smoke tests (specs/loom-tests.md Functional #5, NFR #7).
#
# The smoke harness lives at tests/loom/run-tests.sh and is exposed as
# `nix run .#test-loom` via tests/loom/default.nix and modules/flake/apps.nix.
# The verifications below check (a) the harness file exists, (b) the Nix
# wiring exposes the runner only on Linux, (c) Darwin gets a clear skip
# message, and (d) Linux execution stays under the 30s wall-time budget.
#-----------------------------------------------------------------------------

# Linux-only: spawns the real podman container via `nix run .#test-loom`,
# asserts exit 0 and bead closure. Skips on Darwin and when podman is
# unavailable (exit 77 — both are environmental, not test failures).
test_loom_smoke_real_container() {
    if [ "$(uname -s)" = "Darwin" ]; then
        echo "skip: container smoke is Linux-only" >&2
        exit 77
    fi
    if ! command -v podman >/dev/null 2>&1; then
        echo "skip: podman not on PATH" >&2
        exit 77
    fi
    ( cd "$REPO_ROOT" && nix run --impure .#test-loom )
}

# `tests/loom/default.nix` must expose `loomSmoke` only when the host system
# is Linux — the smoke depends on podman, which is not part of Darwin.
test_smoke_linux_only() {
    local file="$REPO_ROOT/tests/loom/default.nix"
    [ -f "$file" ] || { echo "missing $file" >&2; return 1; }
    grep -q 'isLinux' "$file" || {
        echo "$file: missing isLinux gate around loomSmoke" >&2
        return 1
    }
    grep -q 'loomSmoke =' "$file" || {
        echo "$file: missing loomSmoke binding" >&2
        return 1
    }
    grep -q 'loomSmokeDarwinSkip' "$file" || {
        echo "$file: missing Darwin skip stub" >&2
        return 1
    }
}

# Darwin's `nix run .#test-loom` must print a clear "not available on
# Darwin" message to stderr and exit 0. The message is asserted by string
# match against the run-tests.sh skip branch and the Darwin stub in
# tests/loom/default.nix.
test_smoke_darwin_skip_message() {
    local script="$REPO_ROOT/tests/loom/run-tests.sh"
    [ -f "$script" ] || { echo "missing $script" >&2; return 1; }

    grep -q 'uname -s.*Darwin' "$script" || {
        echo "$script: missing Darwin uname check" >&2
        return 1
    }
    grep -q 'container smoke not available on Darwin' "$script" || {
        echo "$script: missing canonical Darwin skip message" >&2
        return 1
    }

    local nix_file="$REPO_ROOT/tests/loom/default.nix"
    [ -f "$nix_file" ] || { echo "missing $nix_file" >&2; return 1; }
    grep -q 'container smoke not available on Darwin' "$nix_file" || {
        echo "$nix_file: Darwin stub missing canonical skip message" >&2
        return 1
    }
}

# `nix run .#test-loom` must be exposed on Linux as a writeShellApplication
# named `test-loom`. The Nix file binds the runner; modules/flake/apps.nix
# registers it under apps.test-loom.
test_system_runner_exists() {
    local apps="$REPO_ROOT/modules/flake/apps.nix"
    [ -f "$apps" ] || { echo "missing $apps" >&2; return 1; }
    grep -q 'test-loom = test.apps.loom' "$apps" || {
        echo "$apps: missing test-loom app registration" >&2
        return 1
    }

    local nix_file="$REPO_ROOT/tests/loom/default.nix"
    grep -q 'writeShellApplication' "$nix_file" || {
        echo "$nix_file: loomSmoke is not a writeShellApplication" >&2
        return 1
    }
    grep -q 'name = "test-loom"' "$nix_file" || {
        echo "$nix_file: writeShellApplication name is not test-loom" >&2
        return 1
    }
}

# The smoke harness enforces a <30s wall-time budget per Functional #5.
# Verify the script actually checks elapsed time against 30 seconds — if
# this guard slips, regressions in startup cost go silent.
test_smoke_timing() {
    local script="$REPO_ROOT/tests/loom/run-tests.sh"
    [ -f "$script" ] || { echo "missing $script" >&2; return 1; }
    grep -qE 'ELAPSED.*-gt[[:space:]]+30' "$script" || {
        echo "$script: missing 30s wall-time guard" >&2
        return 1
    }
}

#-----------------------------------------------------------------------------
# Dispatch
#-----------------------------------------------------------------------------
if [ $# -eq 0 ]; then
    echo "usage: $0 <test_function>" >&2
    echo "available functions:" >&2
    declare -F | awk '{print "  " $3}' | grep '^  test_' >&2
    exit 2
fi

fn="$1"
shift
if ! declare -F "$fn" >/dev/null; then
    echo "unknown function: $fn" >&2
    exit 2
fi
"$fn" "$@"
