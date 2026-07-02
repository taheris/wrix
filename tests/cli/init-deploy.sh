#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-init-deploy.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

skip() {
  local message="$1"
  printf 'SKIP: %s\n' "$message" >&2
  exit 77
}

require_tools() {
  command -v git >/dev/null 2>&1 || skip "git not on PATH"
  command -v ssh >/dev/null 2>&1 || skip "ssh not on PATH"
  command -v ssh-keygen >/dev/null 2>&1 || skip "ssh-keygen not on PATH"
}

build_wrix() {
  if [[ -n "${WRIX_BIN:-}" ]]; then
    printf '%s\n' "$WRIX_BIN"
    return 0
  fi

  command -v nix >/dev/null 2>&1 || skip "nix not on PATH and WRIX_BIN is unset"
  local out_link="$TEST_TMP/wrix"
  nix build --no-warn-dirty --out-link "$out_link" "$REPO_ROOT#wrix"
  printf '%s\n' "$out_link/bin/wrix"
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label: missing '$needle' in output: $haystack"
  fi
}

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail "$label: unexpected '$needle' in output: $haystack"
  fi
}

assert_equals() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  if [[ "$actual" != "$expected" ]]; then
    fail "$label: got '$actual', expected '$expected'"
  fi
}

assert_file_absent() {
  local path="$1"
  if [[ -e "$path" ]]; then
    fail "unexpected file exists: $path"
  fi
}

mode_of() {
  local path="$1"
  case "$(uname -s)" in
    Darwin | FreeBSD) stat -f '%Lp' "$path" ;;
    *) stat -c '%a' "$path" ;;
  esac
}

assert_mode() {
  local path="$1"
  local expected="$2"
  local actual
  actual="$(mode_of "$path")"
  assert_equals "mode for $path" "$actual" "$expected"
}

setup_repo() {
  local name="$1"
  local remote_url="$2"
  local repo="$TEST_TMP/$name"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.name "Wrix Deploy Test"
  git -C "$repo" config user.email "wrix-deploy@example.invalid"
  printf 'initial\n' >"$repo/README.md"
  git -C "$repo" add README.md
  git -C "$repo" commit -qm "initial"
  git -C "$repo" remote add origin "$remote_url"
  printf '%s\n' "$repo"
}

write_fake_git() {
  local real_git="$1"
  local bin_dir="$TEST_TMP/fake-git"
  mkdir -p "$bin_dir"
  cat >"$bin_dir/git" <<SH
#!/usr/bin/env bash
set -euo pipefail

for arg in "\$@"; do
  if [[ "\$arg" == "ls-remote" ]]; then
    printf '%s\tHEAD\n' "0123456789012345678901234567890123456789"
    exit 0
  fi
done
exec "$real_git" "\$@"
SH
  chmod 700 "$bin_dir/git"
  printf '%s\n' "$bin_dir"
}

write_fake_gh() {
  local state_dir="$1"
  local log_file="$2"
  local bin_dir="$TEST_TMP/fake-gh"
  mkdir -p "$bin_dir" "$state_dir"
  cat >"$bin_dir/gh" <<SH
#!/usr/bin/env bash
set -euo pipefail

state_dir="$state_dir"
log_file="$log_file"

get_field() {
  local prefix="\$1"
  shift
  local field
  for field in "\$@"; do
    if [[ "\$field" == "\$prefix"* ]]; then
      printf '%s\n' "\${field#"\$prefix"}"
      return 0
    fi
  done
  printf 'missing field: %s\n' "\$prefix" >&2
  return 1
}

write_deploy_list() {
  if [[ ! -f "\$state_dir/deploy_title" ]]; then
    printf '[]\n'
    return 0
  fi
  local id title key read_only
  id="\$(<"\$state_dir/deploy_id")"
  title="\$(<"\$state_dir/deploy_title")"
  key="\$(<"\$state_dir/deploy_key")"
  read_only="\$(<"\$state_dir/deploy_read_only")"
  printf '[{"id":%s,"title":"%s","key":"%s","read_only":%s}]\n' "\$id" "\$title" "\$key" "\$read_only"
}

write_signing_list() {
  if [[ ! -f "\$state_dir/signing_title" ]]; then
    printf '[]\n'
    return 0
  fi
  local id title key
  id="\$(<"\$state_dir/signing_id")"
  title="\$(<"\$state_dir/signing_title")"
  key="\$(<"\$state_dir/signing_key")"
  printf '[{"id":%s,"title":"%s","key":"%s"}]\n' "\$id" "\$title" "\$key"
}

if [[ "\${1:-}" != "api" ]]; then
  printf 'unsupported gh command\n' >&2
  exit 2
fi
shift
method="GET"
endpoint=""
fields=()
while [[ "\$#" -gt 0 ]]; do
  case "\$1" in
    --method)
      method="\$2"
      shift 2
      ;;
    --raw-field | --field)
      fields+=("\$2")
      shift 2
      ;;
    --*)
      printf 'unsupported gh api option: %s\n' "\$1" >&2
      exit 2
      ;;
    *)
      if [[ -z "\$endpoint" ]]; then
        endpoint="\$1"
      fi
      shift
      ;;
  esac
done
printf '%s %s %s\n' "\$method" "\$endpoint" "\${fields[*]}" >>"\$log_file"

if [[ "\$method" == "GET" && "\$endpoint" == repos/example/*/keys\?per_page=100 ]]; then
  write_deploy_list
elif [[ "\$method" == "GET" && "\$endpoint" == "user/ssh_signing_keys?per_page=100" ]]; then
  write_signing_list
elif [[ "\$method" == "POST" && "\$endpoint" == repos/example/*/keys ]]; then
  title="\$(get_field "title=" "\${fields[@]}")"
  key="\$(get_field "key=" "\${fields[@]}")"
  read_only="\$(get_field "read_only=" "\${fields[@]}")"
  if [[ "\$read_only" != "false" ]]; then
    printf 'deploy key was not requested with write access\n' >&2
    exit 3
  fi
  printf '1\n' >"\$state_dir/deploy_id"
  printf '%s\n' "\$title" >"\$state_dir/deploy_title"
  printf '%s\n' "\$key" >"\$state_dir/deploy_key"
  printf '%s\n' "\$read_only" >"\$state_dir/deploy_read_only"
  printf '{"id":1}\n'
elif [[ "\$method" == "POST" && "\$endpoint" == "user/ssh_signing_keys" ]]; then
  title="\$(get_field "title=" "\${fields[@]}")"
  key="\$(get_field "key=" "\${fields[@]}")"
  printf '2\n' >"\$state_dir/signing_id"
  printf '%s\n' "\$title" >"\$state_dir/signing_title"
  printf '%s\n' "\$key" >"\$state_dir/signing_key"
  printf '{"id":2}\n'
elif [[ "\$method" == "DELETE" && "\$endpoint" == repos/example/*/keys/* ]]; then
  rm -f "\$state_dir/deploy_id" "\$state_dir/deploy_title" "\$state_dir/deploy_key" "\$state_dir/deploy_read_only"
  printf '{}\n'
elif [[ "\$method" == "DELETE" && "\$endpoint" == user/ssh_signing_keys/* ]]; then
  rm -f "\$state_dir/signing_id" "\$state_dir/signing_title" "\$state_dir/signing_key"
  printf '{}\n'
else
  printf 'unsupported gh api call: %s %s\n' "\$method" "\$endpoint" >&2
  exit 2
fi
SH
  chmod 700 "$bin_dir/gh"
  printf '%s\n' "$bin_dir"
}

seed_remote_deploy() {
  local state_dir="$1"
  local title="$2"
  local key="$3"
  local read_only="$4"
  printf '1\n' >"$state_dir/deploy_id"
  printf '%s\n' "$title" >"$state_dir/deploy_title"
  printf '%s\n' "$key" >"$state_dir/deploy_key"
  printf '%s\n' "$read_only" >"$state_dir/deploy_read_only"
}

seed_remote_signing() {
  local state_dir="$1"
  local title="$2"
  local key="$3"
  printf '2\n' >"$state_dir/signing_id"
  printf '%s\n' "$title" >"$state_dir/signing_title"
  printf '%s\n' "$key" >"$state_dir/signing_key"
}

write_conflict_public_key() {
  local key_path="$TEST_TMP/conflict-key"
  ssh-keygen -q -t ed25519 -N '' -f "$key_path" >/dev/null
  ssh-keygen -y -f "$key_path"
}

run_init() {
  local repo="$1"
  local home="$2"
  local wrix_bin="$3"
  local fake_git="$4"
  local fake_gh="$5"
  local bin_dir path_value
  shift 5
  bin_dir="$(dirname "$wrix_bin")"
  path_value="$fake_gh:$fake_git:$bin_dir:$PATH"
  (cd "$repo" && env -u WRIX_DEPLOY_KEY -u WRIX_SIGNING_KEY PATH="$path_value" HOME="$home" "$wrix_bin" init "$@")
}

expect_failure() {
  local output_file="$1"
  shift
  if "$@" >"$output_file" 2>&1; then
    fail "expected command to fail: $*"
  fi
}

assert_no_remote_mutation() {
  local label="$1"
  local log_file="$2"
  local log
  log="$(<"$log_file")"
  assert_not_contains "$label" "$log" "POST"
  assert_not_contains "$label" "$log" "DELETE"
}

test_github_deploy_and_signing_keys() {
  require_tools
  local wrix_bin real_git fake_git gh_state gh_log fake_gh repo home output key_dir deploy_key signing_key deploy_public signing_public log output_file conflict_public before_public after_public unsupported_repo offline_repo offline_home config_repo config_home
  wrix_bin="$(build_wrix)"
  real_git="$(command -v git)"
  fake_git="$(write_fake_git "$real_git")"
  gh_state="$TEST_TMP/gh-state"
  gh_log="$TEST_TMP/gh.log"
  : >"$gh_log"
  fake_gh="$(write_fake_gh "$gh_state" "$gh_log")"

  repo="$(setup_repo deploy-target "git@github.com:example/deploy-target.git")"
  home="$TEST_TMP/home-deploy"
  output="$(run_init "$repo" "$home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key deploy-key --no-hooks)"
  assert_contains "deploy output" "$output" "deploy: true"
  assert_contains "deploy output" "$output" "sign_commits: true"

  key_dir="$home/.ssh/deploy_keys"
  deploy_key="$key_dir/deploy-key"
  signing_key="$key_dir/deploy-key-signing"
  if [[ ! -f "$deploy_key" || ! -f "$deploy_key.pub" ]]; then
    fail "deploy keypair was not generated"
  fi
  if [[ ! -f "$signing_key" || ! -f "$signing_key.pub" ]]; then
    fail "signing keypair was not generated"
  fi
  assert_mode "$home/.ssh" "700"
  assert_mode "$key_dir" "700"
  assert_mode "$deploy_key" "600"
  assert_mode "$signing_key" "600"
  deploy_public="$(ssh-keygen -y -f "$deploy_key")"
  signing_public="$(ssh-keygen -y -f "$signing_key")"
  assert_equals "remote deploy key" "$(<"$gh_state/deploy_key")" "$deploy_public"
  assert_equals "remote signing key" "$(<"$gh_state/signing_key")" "$signing_public"
  log="$(<"$gh_log")"
  assert_contains "deploy create" "$log" "POST repos/example/deploy-target/keys"
  assert_contains "deploy create" "$log" "read_only=false"
  assert_contains "signing create" "$log" "POST user/ssh_signing_keys"

  : >"$gh_log"
  before_public="$deploy_public/$signing_public"
  output="$(run_init "$repo" "$home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key deploy-key --no-hooks)"
  after_public="$(ssh-keygen -y -f "$deploy_key")/$(ssh-keygen -y -f "$signing_key")"
  assert_contains "reuse output" "$output" "deploy: true"
  assert_equals "reused public keys" "$after_public" "$before_public"
  assert_no_remote_mutation "reuse remote log" "$gh_log"

  chmod 644 "$deploy_key"
  : >"$gh_log"
  output_file="$TEST_TMP/local-conflict.out"
  expect_failure "$output_file" run_init "$repo" "$home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key deploy-key --no-hooks
  output="$(<"$output_file")"
  assert_contains "local conflict" "$output" "deploy key"
  assert_contains "local conflict" "$output" "conflicts with requested deploy provisioning"
  if [[ -s "$gh_log" ]]; then
    fail "local conflict reached GitHub API: $(<"$gh_log")"
  fi

  output="$(run_init "$repo" "$home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key deploy-key --no-hooks --force)"
  assert_contains "force output" "$output" "force: true"
  deploy_public="$(ssh-keygen -y -f "$deploy_key")"
  assert_equals "forced remote deploy key" "$(<"$gh_state/deploy_key")" "$deploy_public"
  log="$(<"$gh_log")"
  assert_contains "force deploy delete" "$log" "DELETE repos/example/deploy-target/keys/1"
  assert_contains "force deploy create" "$log" "POST repos/example/deploy-target/keys"

  conflict_public="$(write_conflict_public_key)"
  seed_remote_signing "$gh_state" "deploy-key-signing" "$conflict_public"
  : >"$gh_log"
  output_file="$TEST_TMP/remote-conflict.out"
  expect_failure "$output_file" run_init "$repo" "$home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key deploy-key --no-hooks
  output="$(<"$output_file")"
  assert_contains "remote conflict" "$output" "remote signing key registration"
  assert_contains "remote conflict" "$output" "conflicts with requested key"
  assert_no_remote_mutation "remote conflict log" "$gh_log"

  output="$(run_init "$repo" "$home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key deploy-key --no-hooks --force)"
  assert_contains "remote force output" "$output" "force: true"
  signing_public="$(ssh-keygen -y -f "$signing_key")"
  assert_equals "forced remote signing key" "$(<"$gh_state/signing_key")" "$signing_public"
  log="$(<"$gh_log")"
  assert_contains "force signing delete" "$log" "DELETE user/ssh_signing_keys/2"
  assert_contains "force signing create" "$log" "POST user/ssh_signing_keys"

  unsupported_repo="$(setup_repo unsupported-remote "git@example.com:example/unsupported-remote.git")"
  : >"$gh_log"
  output_file="$TEST_TMP/unsupported.out"
  expect_failure "$output_file" run_init "$unsupported_repo" "$TEST_TMP/home-unsupported" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key unsupported-key --no-sign --no-hooks
  output="$(<"$output_file")"
  assert_contains "unsupported remote" "$output" "supports only github.com remotes"
  if [[ -s "$gh_log" ]]; then
    fail "unsupported remote reached GitHub API: $(<"$gh_log")"
  fi

  offline_repo="$(setup_repo offline-flag "git@github.com:example/offline-flag.git")"
  offline_home="$TEST_TMP/home-offline"
  : >"$gh_log"
  output_file="$TEST_TMP/offline-flag.out"
  expect_failure "$output_file" run_init "$offline_repo" "$offline_home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --offline --key offline-key --no-hooks
  output="$(<"$output_file")"
  assert_contains "offline flag" "$output" "--deploy cannot be used with --offline"
  assert_file_absent "$offline_home/.ssh/deploy_keys/offline-key"
  if [[ -s "$gh_log" ]]; then
    fail "offline flag reached GitHub API: $(<"$gh_log")"
  fi

  config_repo="$(setup_repo offline-config "git@github.com:example/offline-config.git")"
  config_home="$TEST_TMP/home-config-offline"
  cat >"$config_repo/wrix.toml" <<'TOML'
[wrix.init]
online_verify = false
TOML
  : >"$gh_log"
  output_file="$TEST_TMP/offline-config.out"
  expect_failure "$output_file" run_init "$config_repo" "$config_home" "$wrix_bin" "$fake_git" "$fake_gh" --deploy --key offline-key --no-hooks
  output="$(<"$output_file")"
  assert_contains "offline config" "$output" "--deploy requires online verification"
  assert_file_absent "$config_home/.ssh/deploy_keys/offline-key"
  if [[ -s "$gh_log" ]]; then
    fail "offline config reached GitHub API: $(<"$gh_log")"
  fi
}

ALL_TESTS=(
  test_github_deploy_and_signing_keys
)

run_all() {
  local failed=0
  local fn status
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      status="$?"
      if [[ "$status" -eq 77 ]]; then
        printf 'SKIP: %s\n' "$fn" >&2
      else
        printf 'FAIL: %s\n' "$fn" >&2
        failed=$((failed + 1))
      fi
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    printf '%s test(s) failed\n' "$failed" >&2
    return 1
  fi
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
