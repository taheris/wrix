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
# test_loom_does_not_invoke_podman — loom Rust sources never invoke podman
# directly; only documentation/comments may reference it.
#-----------------------------------------------------------------------------
test_loom_does_not_invoke_podman() {
    local hits
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    # Match podman in any .rs source; allow only doc-comment lines (`///`,
    # `//!`) and ordinary comments (`//`). Anything outside a comment is a
    # rule violation.
    hits=$(grep -rEn 'podman' "$LOOM_DIR/crates" --include='*.rs' || true)
    if [ -z "$hits" ]; then
        return 0
    fi
    if grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*)' <<<"$hits" >/dev/null; then
        echo "non-comment podman reference found in loom/crates:" >&2
        grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*)' <<<"$hits" >&2
        return 1
    fi
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
