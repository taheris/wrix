#!/usr/bin/env bash
set -euo pipefail

wrix_prek_fail() {
  printf 'wrix: %s; reload the Wrix devshell or run the Nix-packaged wrix init in this repository to repair hooks\n' "$*" >&2
  exit 1
}

wrix_prek_binding_key() {
  local os arch context
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) wrix_prek_fail "unsupported hook platform $os" ;;
  esac
  case "$arch" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64) ;;
    *) wrix_prek_fail "unsupported hook architecture $arch" ;;
  esac
  context="${WRIX_PREK_CONTEXT:-}"
  if [[ -z "$context" ]]; then
    if [[ -f /etc/wrix/image-agent ]]; then
      context=container
    else
      context=host
    fi
  fi
  case "$context" in
    host | container) ;;
    *) wrix_prek_fail "invalid WRIX_PREK_CONTEXT $context (expected host or container)" ;;
  esac
  printf 'wrix.prek-%s-%s-%s.runner\n' "$context" "$arch" "$os"
}

wrix_prek_config_get() {
  git config --local --get "$1"
}

wrix_prek_resolve() {
  local key runner bin_dir status tool
  for tool in git uname; do
    command -v "$tool" >/dev/null || wrix_prek_fail "hook bootstrap needs $tool on PATH"
  done
  key=$(wrix_prek_binding_key)
  if runner=$(wrix_prek_config_get "$key"); then
    [[ "$runner" == /* && -x "$runner" ]] || wrix_prek_fail "missing or invalid hook runner $runner for $key"
  else
    status="$?"
    [[ "$status" == 1 ]] || wrix_prek_fail "cannot read repository-local Git config $key (exit $status)"
    # Compatibility for older installers that expose a packaged runner but no binding.
    runner=$(command -v wrix-prek) || wrix_prek_fail "no hook runner binding for $key and wrix-prek is not on PATH"
  fi
  if ! bin_dir=$("$runner" --print-bin-dir); then
    wrix_prek_fail "hook runner $runner could not resolve its packaged runtime"
  fi
  [[ -n "$bin_dir" ]] || wrix_prek_fail "hook runner $runner returned an empty runtime PATH"
  export PATH="$bin_dir:${PATH:-}"
  command -v prek >/dev/null || wrix_prek_fail "hook runner $runner has no prek executable"
}
