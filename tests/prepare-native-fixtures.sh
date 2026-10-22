#!/usr/bin/env bash
set -euo pipefail

main() {
  : "${NEXTEST_ENV:?nextest must provide the setup environment file}"
  local names=(
    WRIX_TEST_PREK_HOOKS
    WRIX_TEST_PREK_RUNNER
    WRIX_TEST_BROKEN_PREK_RUNNER
    WRIX_TEST_PREK_BUILD_SPEC
    WRIX_TEST_PACKAGED_WRIX
    WRIX_TEST_DEVSHELL_HOOK
    WRIX_TEST_CELLERD
    WRIX_TEST_CONTROLLER_BINDING
  )
  local members=(hooks runner broken-runner builder-spec packaged-wrix devshell-hook cellerd controller-binding)
  local name index root fixtures="" missing=0 value
  local values=()

  for name in "${names[@]}"; do
    if [[ -z "${!name:-}" ]]; then
      missing=1
    fi
  done
  if [[ "$missing" -eq 1 ]]; then
    root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    fixtures="$(WRIX_TEST_REPO="$root" nix build --impure --print-out-paths \
      --out-link "$NEXTEST_ENV.fixtures" --expr '
        let
          flake = builtins.getFlake ("git+file://" + builtins.getEnv "WRIX_TEST_REPO");
        in (builtins.getAttr builtins.currentSystem flake.legacyPackages).testFixtures.native
      ')"
    if [[ "$fixtures" != /* || ! -d "$fixtures" ]]; then
      printf 'Invalid native fixture bundle: %s\n' "$fixtures" >&2
      return 1
    fi
  fi

  for index in "${!names[@]}"; do
    name="${names[$index]}"
    if [[ -n "${!name:-}" ]]; then
      value="${!name}"
    else
      value="$(readlink "$fixtures/${members[$index]}")"
    fi
    if [[ "$value" == *$'\n'* || "$value" == *$'\r'* || ! -e "$value" ]]; then
      printf 'Invalid packaging fixture %s: %s\n' "$name" "$value" >&2
      return 1
    fi
    case "$name" in
      WRIX_TEST_PREK_HOOKS) [[ -d "$value" ]] ;;
      WRIX_TEST_PREK_RUNNER|WRIX_TEST_BROKEN_PREK_RUNNER|WRIX_TEST_PACKAGED_WRIX|WRIX_TEST_CELLERD) [[ -f "$value" && -x "$value" ]] ;;
      *) [[ -f "$value" ]] ;;
    esac || {
      printf 'Wrong file type or permissions for packaging fixture %s: %s\n' "$name" "$value" >&2
      return 1
    }
    values+=("$value")
  done
  for index in "${!names[@]}"; do
    printf '%s=%s\n' "${names[$index]}" "${values[$index]}"
  done >>"$NEXTEST_ENV"
}

main "$@"
