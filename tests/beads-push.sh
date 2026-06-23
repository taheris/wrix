#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-beads-push.XXXXXX)"
WRIX_TEST_BIN=""

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'SKIP: %s is required\n' "$command_name" >&2
    exit 77
  fi
}

build_wrix() {
  if [[ -n "${WRIX_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_BIN"
    return 0
  fi
  if [[ -z "$WRIX_TEST_BIN" ]]; then
    require_command cargo
    cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p wrix-cli
    WRIX_TEST_BIN="$REPO_ROOT/target/debug/wrix"
  fi
  printf '%s\n' "$WRIX_TEST_BIN"
}

write_shims() {
  local bin_dir="$1"
  mkdir -p "$bin_dir"

  cat >"$bin_dir/bd" <<'BD'
#!/usr/bin/env bash
set -euo pipefail

log_file="${WRIX_BEADS_PUSH_LOG:?}"
state_dir="${WRIX_BEADS_PUSH_STATE:?}"
printf 'bd|cwd=%s|prek=%s|%s\n' "$PWD" "${PREK_ALLOW_NO_CONFIG:-}" "$*" >>"$log_file"

if [[ "${1:-}" == "config" && "${2:-}" == "set" && "${3:-}" == "export.auto" && "${4:-}" == "false" ]]; then
  config_path="${FAKE_REPO_ROOT:?}/.beads/config.yaml"
  mkdir -p "$(dirname "$config_path")"
  if [[ -f "$config_path" ]] && grep -q '^export[.]auto:' "$config_path"; then
    tmp_path="$state_dir/config.yaml.tmp"
    awk '{ if ($0 ~ /^export[.]auto:/) print "export.auto: false"; else print }' "$config_path" >"$tmp_path"
    mv "$tmp_path" "$config_path"
  elif [[ -f "$config_path" ]]; then
    printf '%s\n' 'export.auto: false' >>"$config_path"
  else
    printf '%s\n' 'export.auto: false' >"$config_path"
  fi
  exit 0
fi

if [[ "${1:-}" == "dolt" && "${2:-}" == "remote" && "${3:-}" == "list" ]]; then
  origin_state="$state_dir/origin-remote"
  if [[ -f "$origin_state" ]]; then
    printf 'origin %s\n' "$(cat "$origin_state")"
    exit 0
  fi
  case "${FAKE_BD_REMOTE_LIST:-empty}" in
    correct)
      printf 'origin file://%s/.git/beads-worktrees/beads/.beads/dolt-remote\n' "${FAKE_REPO_ROOT:?}"
      ;;
    missing_origin)
      printf 'backup file:///backup/beads\n'
      ;;
    stale)
      printf 'backup file://%s/.git/beads-worktrees/beads/.beads/dolt-remote\n' "${FAKE_REPO_ROOT:?}"
      printf 'origin file:///stale/beads\n'
      ;;
    empty)
      ;;
    *)
      printf 'fake bd: unknown remote list mode: %s\n' "${FAKE_BD_REMOTE_LIST:-}" >&2
      exit 64
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "dolt" && "${2:-}" == "commit" ]]; then
  exit 0
fi

if [[ "${1:-}" == "dolt" && "${2:-}" == "push" ]]; then
  count_file="$state_dir/bd-push-count"
  count=0
  if [[ -f "$count_file" ]]; then
    count="$(cat "$count_file")"
  fi
  count=$((count + 1))
  printf '%s\n' "$count" >"$count_file"
  case "${FAKE_BD_PUSH_MODE:-success}" in
    auth_fail)
      printf 'authentication failed\n' >&2
      exit 1
      ;;
    permission_denied)
      printf 'permission denied (publickey)\n' >&2
      exit 1
      ;;
    access_denied)
      printf 'access denied\n' >&2
      exit 1
      ;;
    reject_intent)
      if [[ "$count" -eq 1 ]]; then
        printf 'non-fast-forward update rejected\n' >&2
        exit 1
      fi
      ;;
    success)
      ;;
    *)
      printf 'fake bd: unknown push mode: %s\n' "${FAKE_BD_PUSH_MODE:-}" >&2
      exit 64
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "dolt" && "${2:-}" == "pull" ]]; then
  exit 0
fi

if [[ "${1:-}" == "sql" ]]; then
  query=""
  argument=""
  for argument in "$@"; do
    query="$argument"
  done
  if [[ "$query" == *"dolt_commit_diff_issues"* ]]; then
    case "${FAKE_BD_AFFECTED_IDS:-wx-1}" in
      none)
        printf 'id\n'
        ;;
      *)
        printf 'id\n%s\n' "${FAKE_BD_AFFECTED_IDS:-wx-1}"
        ;;
    esac
    exit 0
  fi
  if [[ "$query" == *"SELECT i.id, i.status"* ]]; then
    count_file="$state_dir/bd-snapshot-count"
    count=0
    if [[ -f "$count_file" ]]; then
      count="$(cat "$count_file")"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$count_file"
    if [[ "${FAKE_BD_INTENT_MODE:-same}" == "diverge" && "$count" -gt 1 ]]; then
      printf '%s\n' 'id,status,labels' 'wx-1,open,ready'
    else
      printf '%s\n' 'id,status,labels' 'wx-1,closed,ready'
    fi
    exit 0
  fi
  if [[ "$query" == "CALL DOLT_REMOTE('remove', 'origin')" ]]; then
    rm -f "$state_dir/origin-remote"
    exit 0
  fi
  if [[ "$query" == "CALL DOLT_REMOTE('add', 'origin', '"*"')" ]]; then
    prefix="CALL DOLT_REMOTE('add', 'origin', '"
    suffix="')"
    remote_url="${query#"$prefix"}"
    remote_url="${remote_url%"$suffix"}"
    printf '%s\n' "$remote_url" >"$state_dir/origin-remote"
    exit 0
  fi
  if [[ "$query" == *"DOLT_REMOTE"* ]]; then
    exit 0
  fi
fi

printf 'fake bd: unexpected args: %s\n' "$*" >&2
exit 64
BD
  chmod +x "$bin_dir/bd"

  cat >"$bin_dir/git" <<'GIT'
#!/usr/bin/env bash
set -euo pipefail

log_file="${WRIX_BEADS_PUSH_LOG:?}"
state_dir="${WRIX_BEADS_PUSH_STATE:?}"
printf 'git|cwd=%s|prek=%s|%s\n' "$PWD" "${PREK_ALLOW_NO_CONFIG:-}" "$*" >>"$log_file"

require_prek_for_sync() {
  if [[ "${FAKE_GIT_REQUIRE_PREK:-0}" != "1" ]]; then
    return 0
  fi
  if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--show-toplevel" ]]; then
    return 0
  fi
  if [[ "${PREK_ALLOW_NO_CONFIG:-}" == "1" ]]; then
    return 0
  fi
  printf 'No prek.toml found\n' >&2
  exit 42
}

status_for_count() {
  local requested="$1"
  local sequence="${FAKE_GIT_STATUS_SEQUENCE:-clean}"
  local index=1
  local item=""
  local selected=""
  local -a items
  IFS=',' read -r -a items <<<"$sequence"
  for item in "${items[@]}"; do
    if [[ "$index" -eq "$requested" ]]; then
      selected="$item"
    fi
    index=$((index + 1))
  done
  if [[ -z "$selected" ]]; then
    local last_index
    last_index=$((${#items[@]} - 1))
    selected="${items[$last_index]}"
  fi
  printf '%s\n' "$selected"
}

require_prek_for_sync "$@"

if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--show-toplevel" ]]; then
  if [[ "${FAKE_GIT_ROOT_FAIL:-0}" == "1" ]]; then
    printf 'fake git: not a repository\n' >&2
    exit 128
  fi
  printf '%s\n' "${FAKE_REPO_ROOT:?}"
  exit 0
fi

if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--is-inside-work-tree" ]]; then
  if [[ "${FAKE_GIT_WORKTREE_VALID:-1}" == "1" ]]; then
    printf 'true\n'
    exit 0
  fi
  exit 128
fi

if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--verify" ]]; then
  target="${3:-}"
  branch="${FAKE_GIT_BRANCH_NAME:-beads}"
  if [[ "$target" == "$branch" && "${FAKE_GIT_BRANCH_EXISTS:-1}" == "1" ]]; then
    exit 0
  fi
  if [[ "$target" == "origin/$branch" && "${FAKE_GIT_ORIGIN_BRANCH_EXISTS:-0}" == "1" ]]; then
    exit 0
  fi
  exit 128
fi

if [[ "${1:-}" == "worktree" && "${2:-}" == "prune" ]]; then
  exit 0
fi

if [[ "${1:-}" == "worktree" && "${2:-}" == "add" ]]; then
  worktree_path="${3:?}"
  mkdir -p "$worktree_path/.beads/dolt-remote"
  if [[ "${FAKE_GIT_WRITE_DOTGIT:-0}" == "1" ]]; then
    printf 'gitdir: %s\n' "${FAKE_GITDIR_TARGET:-/stale/wrix-beads}" >"$worktree_path/.git"
  fi
  if [[ "${FAKE_GIT_CREATE_ADMIN:-0}" == "1" ]]; then
    mkdir -p "${FAKE_REPO_ROOT:?}/.git/worktrees/${FAKE_GIT_BRANCH_NAME:-beads}"
  fi
  exit 0
fi

if [[ "${1:-}" == "update-index" && "${2:-}" == "--refresh" ]]; then
  exit 0
fi

if [[ "${1:-}" == "status" && "${2:-}" == "--porcelain" ]]; then
  count_file="$state_dir/git-status-count"
  count=0
  if [[ -f "$count_file" ]]; then
    count="$(cat "$count_file")"
  fi
  count=$((count + 1))
  printf '%s\n' "$count" >"$count_file"
  status="$(status_for_count "$count")"
  if [[ "$status" == "dirty" ]]; then
    printf '%s\n' ' M .beads/dolt-remote/tracked' '?? .beads/dolt-remote/untracked'
  fi
  exit 0
fi

if [[ "${1:-}" == "add" && "${2:-}" == "-A" ]]; then
  exit 0
fi

if [[ "${1:-}" == "commit" && "${2:-}" == "-m" ]]; then
  exit 0
fi

if [[ "${1:-}" == "pull" && "${2:-}" == "--rebase" ]]; then
  exit 0
fi

if [[ "${1:-}" == "push" && "${2:-}" == "-u" ]]; then
  exit 0
fi

printf 'fake git: unexpected args: %s\n' "$*" >&2
exit 64
GIT
  chmod +x "$bin_dir/git"

  cat >"$bin_dir/rsync" <<'RSYNC'
#!/usr/bin/env bash
set -euo pipefail

log_file="${WRIX_BEADS_PUSH_LOG:?}"
printf 'rsync|cwd=%s|prek=%s|%s\n' "$PWD" "${PREK_ALLOW_NO_CONFIG:-}" "$*" >>"$log_file"
destination=""
argument=""
for argument in "$@"; do
  destination="$argument"
done
if [[ -n "$destination" ]]; then
  mkdir -p "$destination"
fi
exit 0
RSYNC
  chmod +x "$bin_dir/rsync"
}

write_base_config() {
  local root="$1"
  mkdir -p "$root/.beads"
  cat >"$root/.beads/config.yaml" <<'YAML'
issue-prefix: wx
sync-branch: beads
sync:
  mode: dolt-native
YAML
}

verify_shims() {
  local case_dir="$1"
  local root="$case_dir/root"
  local git_out
  local rsync_dest="$case_dir/shim-rsync-dest"
  git_out="$(env "WRIX_BEADS_PUSH_LOG=$case_dir/log" "WRIX_BEADS_PUSH_STATE=$case_dir/state" "FAKE_REPO_ROOT=$root" "$case_dir/bin/git" rev-parse --show-toplevel)"
  if [[ "$git_out" != "$root" ]]; then
    fail "fake git returned root '$git_out', want '$root'"
  fi
  env "WRIX_BEADS_PUSH_LOG=$case_dir/log" "WRIX_BEADS_PUSH_STATE=$case_dir/state" "FAKE_REPO_ROOT=$root" "$case_dir/bin/bd" config set export.auto false
  assert_file_contains "$root/.beads/config.yaml" 'export.auto: false'
  env "WRIX_BEADS_PUSH_LOG=$case_dir/log" "$case_dir/bin/rsync" -a "$root/" "$rsync_dest/"
  if [[ ! -d "$rsync_dest" ]]; then
    fail "fake rsync did not create destination"
  fi
  rm -rf "$case_dir/state" "$rsync_dest"
  mkdir -p "$case_dir/state"
  : >"$case_dir/log"
  write_base_config "$root"
}

make_case() {
  local name="$1"
  local case_dir="$TEST_TMP/$name"
  local root="$case_dir/root"
  mkdir -p "$case_dir/bin" "$case_dir/state" "$root/.beads"
  : >"$case_dir/log"
  write_base_config "$root"
  write_shims "$case_dir/bin"
  verify_shims "$case_dir"
  printf '%s\n' "$case_dir"
}

run_push() {
  local case_dir="$1"
  shift
  local root="$case_dir/root"
  local wrix_bin
  local rc
  local -a command
  wrix_bin="$(build_wrix)"
  command=(
    env
    -u LOOM_INSIDE
    -u IS_SANDBOX
    -u PREK_ALLOW_NO_CONFIG
    -u GIT_DIR
    -u GIT_WORK_TREE
    "PATH=$case_dir/bin:$PATH"
    "WRIX_BEADS_PUSH_LOG=$case_dir/log"
    "WRIX_BEADS_PUSH_STATE=$case_dir/state"
    "FAKE_REPO_ROOT=$root"
  )
  local assignment=""
  for assignment in "$@"; do
    command+=("$assignment")
  done
  command+=("$wrix_bin" beads push)
  set +e
  ( cd "$root" && "${command[@]}" >"$case_dir/stdout" 2>"$case_dir/stderr" )
  rc=$?
  set -e
  printf '%s\n' "$rc" >"$case_dir/rc"
}

run_push_from() {
  local case_dir="$1"
  local cwd="$2"
  shift 2
  local root="$case_dir/root"
  local wrix_bin
  local rc
  local -a command
  mkdir -p "$cwd"
  wrix_bin="$(build_wrix)"
  command=(
    env
    -u LOOM_INSIDE
    -u IS_SANDBOX
    -u PREK_ALLOW_NO_CONFIG
    -u GIT_DIR
    -u GIT_WORK_TREE
    "PATH=$case_dir/bin:$PATH"
    "WRIX_BEADS_PUSH_LOG=$case_dir/log"
    "WRIX_BEADS_PUSH_STATE=$case_dir/state"
    "FAKE_REPO_ROOT=$root"
  )
  local assignment=""
  for assignment in "$@"; do
    command+=("$assignment")
  done
  command+=("$wrix_bin" beads push)
  set +e
  ( cd "$cwd" && "${command[@]}" >"$case_dir/stdout" 2>"$case_dir/stderr" )
  rc=$?
  set -e
  printf '%s\n' "$rc" >"$case_dir/rc"
}

assert_rc() {
  local case_dir="$1"
  local want="$2"
  local got
  got="$(cat "$case_dir/rc")"
  if [[ "$got" != "$want" ]]; then
    printf 'stdout:\n' >&2
    cat "$case_dir/stdout" >&2
    printf 'stderr:\n' >&2
    cat "$case_dir/stderr" >&2
    fail "expected exit $want, got $got"
  fi
}

assert_file_contains() {
  local file="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" "$file"; then
    printf 'file %s:\n' "$file" >&2
    cat "$file" >&2
    fail "missing expected text: $needle"
  fi
}

assert_file_not_contains() {
  local file="$1"
  local needle="$2"
  if grep -Fq -- "$needle" "$file"; then
    printf 'file %s:\n' "$file" >&2
    cat "$file" >&2
    fail "unexpected text present: $needle"
  fi
}

line_number_after() {
  local file="$1"
  local needle="$2"
  local after="$3"
  awk -v needle="$needle" -v after="$after" 'NR > after && index($0, needle) { print NR; found = 1; exit } END { if (found != 1) exit 1 }' "$file"
}

log_count() {
  local file="$1"
  local needle="$2"
  awk -v needle="$needle" 'index($0, needle) { count += 1 } END { print count + 0 }' "$file"
}

assert_log_count() {
  local case_dir="$1"
  local needle="$2"
  local want="$3"
  local got
  got="$(log_count "$case_dir/log" "$needle")"
  if [[ "$got" != "$want" ]]; then
    printf 'log:\n' >&2
    cat "$case_dir/log" >&2
    fail "expected $want log entries for '$needle', got $got"
  fi
}

assert_log_absent() {
  local case_dir="$1"
  local needle="$2"
  assert_log_count "$case_dir" "$needle" 0
}

assert_log_order() {
  local case_dir="$1"
  shift
  local previous=0
  local needle=""
  local current
  for needle in "$@"; do
    if ! current="$(line_number_after "$case_dir/log" "$needle" "$previous")"; then
      printf 'log:\n' >&2
      cat "$case_dir/log" >&2
      fail "missing log entry: $needle"
    fi
    if (( current <= previous )); then
      printf 'log:\n' >&2
      cat "$case_dir/log" >&2
      fail "log entry out of order: $needle"
    fi
    previous="$current"
  done
}

assert_sync_git_has_prek() {
  local case_dir="$1"
  if ! awk -F'|' '$1 == "git" && index($4, "rev-parse --show-toplevel") == 0 && $3 != "prek=1" { print; bad = 1 } END { exit bad ? 1 : 0 }' "$case_dir/log"; then
    printf 'log:\n' >&2
    cat "$case_dir/log" >&2
    fail "a beads-branch git command did not set PREK_ALLOW_NO_CONFIG=1"
  fi
}

assert_bd_lacks_prek() {
  local case_dir="$1"
  if awk -F'|' '$1 == "bd" && $3 == "prek=1" { found = 1 } END { exit found ? 0 : 1 }' "$case_dir/log"; then
    printf 'log:\n' >&2
    cat "$case_dir/log" >&2
    fail "bd command unexpectedly inherited PREK_ALLOW_NO_CONFIG=1"
  fi
}

test_beadspush_pushes_before_pulls() {
  local case_dir
  case_dir="$(make_case pushes-before-pulls)"
  run_push "$case_dir" "FAKE_GIT_BRANCH_EXISTS=0"
  assert_rc "$case_dir" 0
  assert_log_order "$case_dir" \
    'config set export.auto false' \
    'dolt commit' \
    'dolt push'
  assert_log_absent "$case_dir" 'dolt pull'

  local auth_case
  local auth_mode
  local auth_message
  local -a auth_failures=(
    'auth_fail:authentication failed'
    'permission_denied:permission denied'
    'access_denied:access denied'
  )
  for auth_case in "${auth_failures[@]}"; do
    auth_mode="${auth_case%%:*}"
    auth_message="${auth_case#*:}"
    auth_case="$(make_case "push-$auth_mode-no-pull")"
    run_push "$auth_case" "FAKE_BD_PUSH_MODE=$auth_mode" "FAKE_GIT_BRANCH_EXISTS=0"
    assert_rc "$auth_case" 1
    assert_file_contains "$auth_case/stderr" "$auth_message"
    assert_log_count "$auth_case" 'dolt push' 1
    assert_log_absent "$auth_case" 'dolt pull'
  done
}

test_beadspush_failloud_on_intent_overwrite() {
  local case_dir
  case_dir="$(make_case intent-overwrite)"
  run_push "$case_dir" \
    "FAKE_BD_PUSH_MODE=reject_intent" \
    "FAKE_BD_INTENT_MODE=diverge" \
    "FAKE_GIT_BRANCH_EXISTS=0"
  assert_rc "$case_dir" 1
  assert_file_contains "$case_dir/stderr" 'pull-fallback diverged from local status/label intent'
  assert_file_contains "$case_dir/stderr" 'affected issue IDs: wx-1'
  assert_log_order "$case_dir" \
    'dolt push' \
    'dolt_commit_diff_issues' \
    'SELECT i.id, i.status' \
    'dolt pull'
  assert_log_count "$case_dir" 'dolt push' 1
  assert_log_count "$case_dir" 'SELECT i.id, i.status' 2
}

test_beadspush_disables_autoexport() {
  local case_dir
  case_dir="$(make_case autoexport)"
  run_push "$case_dir" "FAKE_GIT_BRANCH_EXISTS=0"
  assert_rc "$case_dir" 0
  assert_file_contains "$case_dir/root/.beads/config.yaml" 'export.auto: false'
  assert_file_not_contains "$case_dir/stderr" 'Warning: auto-export: git add failed'
  assert_log_order "$case_dir" \
    'config set export.auto false' \
    'dolt commit' \
    'dolt push'
  assert_file_not_contains "$case_dir/log" '.beads/issues.jsonl'
}

test_beadspush_repairs_host_dolt_remote() {
  local case_dir
  case_dir="$(make_case host-remote-repair)"
  mkdir -p "$case_dir/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  run_push "$case_dir" "FAKE_BD_REMOTE_LIST=stale"
  assert_rc "$case_dir" 0
  assert_file_contains "$case_dir/stderr" "repairing Dolt origin remote -> file://$case_dir/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  assert_log_order "$case_dir" \
    'config set export.auto false' \
    'dolt remote list' \
    "CALL DOLT_REMOTE('remove', 'origin')" \
    "CALL DOLT_REMOTE('add', 'origin'" \
    'dolt commit' \
    'dolt push'
  assert_file_not_contains "$case_dir/log" 'dolt remote add'

  local sandbox_case
  sandbox_case="$(make_case sandbox-remote-temporary)"
  mkdir -p "$sandbox_case/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  run_push "$sandbox_case" "IS_SANDBOX=1" "FAKE_BD_REMOTE_LIST=stale"
  assert_rc "$sandbox_case" 0
  assert_file_contains "$sandbox_case/stderr" "temporarily using sandbox Dolt origin remote -> file://$sandbox_case/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  assert_log_order "$sandbox_case" \
    'config set export.auto false' \
    'dolt remote list' \
    "CALL DOLT_REMOTE('remove', 'origin')" \
    "CALL DOLT_REMOTE('add', 'origin', 'file://$sandbox_case/root/.git/beads-worktrees/beads/.beads/dolt-remote')" \
    'dolt commit' \
    'dolt push' \
    "CALL DOLT_REMOTE('remove', 'origin')" \
    "CALL DOLT_REMOTE('add', 'origin', 'file:///stale/beads')"
  assert_file_contains "$sandbox_case/state/origin-remote" 'file:///stale/beads'
}

test_beadspush_loom_inside_noop() {
  local case_dir
  case_dir="$(make_case loom-inside)"
  run_push_from "$case_dir" "$case_dir/not-a-repo" "LOOM_INSIDE=1" "FAKE_GIT_ROOT_FAIL=1"
  assert_rc "$case_dir" 0
  assert_file_contains "$case_dir/stderr" 'wrix beads push: LOOM_INSIDE set; loom driver owns publish, skipping'
  assert_log_count "$case_dir" '' 0
}

test_beadspush_failloud_missing_repo() {
  local case_dir
  case_dir="$(make_case missing-repo)"
  run_push_from "$case_dir" "$case_dir/not-a-repo" "FAKE_GIT_ROOT_FAIL=1"
  assert_rc "$case_dir" 1
  assert_file_contains "$case_dir/stderr" "cannot resolve a git repository from '$case_dir/not-a-repo'"
  assert_file_contains "$case_dir/stderr" 'run inside a workspace checkout'
  assert_file_not_contains "$case_dir/stderr" 'fatal: not a git repository: (null)'
  assert_log_absent "$case_dir" 'bd|'
}

test_beadspush_pre_pull_cleanup_canonical() {
  local case_dir
  case_dir="$(make_case pre-pull-cleanup)"
  mkdir -p "$case_dir/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  mkdir -p "$case_dir/root/.beads/dolt/dolt-remote"
  run_push "$case_dir" \
    "IS_SANDBOX=1" \
    "FAKE_GIT_STATUS_SEQUENCE=dirty,dirty"
  assert_rc "$case_dir" 0
  assert_log_order "$case_dir" \
    'update-index --refresh' \
    'status --porcelain --untracked-files=normal' \
    'add -A' \
    'commit -m bd sync --quiet' \
    'pull --rebase --quiet' \
    'rsync|'
  assert_log_order "$case_dir" \
    'rsync|' \
    'status --porcelain --untracked-files=normal' \
    'push -u origin beads --quiet'
  assert_log_count "$case_dir" 'update-index --refresh' 2
  assert_log_count "$case_dir" 'status --porcelain --untracked-files=normal' 2
  assert_log_count "$case_dir" 'commit -m bd sync --quiet' 2
}

test_beadspush_recovers_orphaned_worktree() {
  local case_dir
  case_dir="$(make_case orphaned-worktree)"
  mkdir -p "$case_dir/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  mkdir -p "$case_dir/root/.beads/dolt/dolt-remote"
  printf 'gitdir: /stale/wrix-beads\n' >"$case_dir/root/.git/beads-worktrees/beads/.git"
  run_push "$case_dir" \
    "IS_SANDBOX=1" \
    "FAKE_GIT_WORKTREE_VALID=0" \
    "FAKE_GIT_WRITE_DOTGIT=1"
  assert_rc "$case_dir" 0
  assert_file_contains "$case_dir/stdout" 'wrix beads push: synced to GitHub'
  assert_file_not_contains "$case_dir/stderr" 'fatal: not a git repository: (null)'
  assert_log_order "$case_dir" \
    'rev-parse --is-inside-work-tree' \
    'worktree prune' \
    'worktree add' \
    'pull --rebase --quiet' \
    'push -u origin beads --quiet'
  assert_file_contains "$case_dir/log" "worktree add $case_dir/root/.git/beads-worktrees/beads beads --quiet"
}

test_beadspush_worktree_recreate_skips_prek() {
  local case_dir
  case_dir="$(make_case prek-skip)"
  mkdir -p "$case_dir/root/.git/beads-worktrees/beads/.beads/dolt-remote"
  printf 'gitdir: /stale/wrix-beads\n' >"$case_dir/root/.git/beads-worktrees/beads/.git"
  run_push "$case_dir" \
    "IS_SANDBOX=1" \
    "FAKE_GIT_WORKTREE_VALID=0" \
    "FAKE_GIT_REQUIRE_PREK=1" \
    "FAKE_GIT_WRITE_DOTGIT=1"
  assert_rc "$case_dir" 0
  assert_file_not_contains "$case_dir/stderr" 'No prek.toml'
  assert_file_contains "$case_dir/stdout" 'wrix beads push: synced to GitHub'
  assert_sync_git_has_prek "$case_dir"
  assert_bd_lacks_prek "$case_dir"
}

ALL_TESTS=(
  test_beadspush_pushes_before_pulls
  test_beadspush_failloud_on_intent_overwrite
  test_beadspush_disables_autoexport
  test_beadspush_repairs_host_dolt_remote
  test_beadspush_loom_inside_noop
  test_beadspush_failloud_missing_repo
  test_beadspush_pre_pull_cleanup_canonical
  test_beadspush_recovers_orphaned_worktree
  test_beadspush_worktree_recreate_skips_prek
)

run_one() {
  local fn="$1"
  "$fn"
  printf 'PASS: %s\n' "$fn"
}

run_all() {
  local failed=0
  local fn=""
  local wrix_bin
  wrix_bin="$(build_wrix)"
  export WRIX_BIN="$wrix_bin"
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if bash "$0" "$fn"; then
      :
    else
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    fail "$failed test(s) failed"
  fi
}

if [[ "$#" -gt 0 ]]; then
  run_one "$1"
else
  run_all
fi
