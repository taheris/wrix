#!/usr/bin/env bash
set -euo pipefail

verifier_platform() {
  local architecture os
  architecture="$(uname -m)"
  os="$(uname -s)"
  case "$architecture" in arm64) architecture=aarch64 ;; esac
  case "$os" in Linux) os=linux ;; Darwin) os=darwin ;; esac
  printf '%s-%s\n' "$architecture" "$os"
}

verifier_missing_capability() {
  local capability="$1" reason="$2"
  jq -cn --arg capability "$capability" --arg reason "$reason" \
    '{kind:"missing-capability",capability:$capability,reason:$reason}'
  return 77
}

verifier_preflight() {
  local platform="$1" requirements="$2" capability runtime
  if ! jq -e --arg platform "$platform" \
    '.platforms | length == 0 or index($platform) != null' <<<"$requirements" >/dev/null; then
    jq -cn --arg reason "verifier is not applicable on $platform" \
      '{kind:"foreign-platform",reason:$reason}'
    return 77
  fi
  while IFS= read -r capability; do
    case "$capability" in
      container-runtime)
        case "$platform" in
          *-linux) runtime=podman ;;
          *-darwin) runtime=container ;;
          *) printf 'unsupported container platform: %s\n' "$platform" >&2; return 1 ;;
        esac
        if ! command -v "$runtime" >/dev/null 2>&1; then
          verifier_missing_capability "$capability" "$runtime is not on PATH"
          return 77
        fi
        if [[ "$platform" == *-linux && -e /run/.containerenv ]]; then
          verifier_missing_capability "$capability" "nested container: rootless Podman unavailable"
          return 77
        fi
        ;;
      kvm)
        if [[ ! -c /dev/kvm || ! -r /dev/kvm || ! -w /dev/kvm ]]; then
          verifier_missing_capability "$capability" "sandbox has no accessible /dev/kvm"
          return 77
        fi
        ;;
      user-network-namespace)
        if ! command -v unshare >/dev/null 2>&1; then
          printf 'required preflight tool is missing: unshare\n' >&2
          return 1
        fi
        if ! unshare --user --map-root-user --net true; then
          verifier_missing_capability "$capability" "user/network namespaces are unavailable"
          return 77
        fi
        ;;
      *) printf 'unknown verifier capability: %s\n' "$capability" >&2; return 1 ;;
    esac
  done < <(jq -r '.capabilities[]' <<<"$requirements")
}

verifier_emit() {
  local target="$1" outcome="$2" evidence="$3" platform="$4" requirements="$5" reason="${6:-}"
  jq -cn --arg target "$target" --arg outcome "$outcome" --arg evidence "$evidence" \
    --arg platform "$platform" --argjson requirements "$requirements" --arg reason "$reason" '
      {target:$target,outcome:$outcome,evidence:$evidence,
       execution:($requirements + {platform:$platform})}
      + if $reason == "" then {} else {skip_reason:($reason | fromjson)} end
    '
}

verifier_run() {
  local target="$1" requirements="$2"
  shift 2
  local platform directory status evidence reason
  platform="$(verifier_platform)"
  directory="$(mktemp -d -t wrix-verifier.XXXXXX)"
  if verifier_preflight "$platform" "$requirements" >"$directory/reason" 2>"$directory/log"; then
    if "$@" >"$directory/log" 2>&1; then
      verifier_emit "$target" passed passed "$platform" "$requirements"
      rm -rf "$directory"
      return 0
    else
      status="$?"
    fi
    evidence="$(head -c 4000 "$directory/log")"
    [[ -n "$evidence" ]] || evidence="verifier exited $status"
    cat "$directory/log" >&2
    if [[ "$status" -eq 77 ]]; then
      jq -cn --arg target "$target" --arg evidence "unexpected skip: $evidence" \
        --arg platform "$platform" --argjson requirements "$requirements" \
        '{target:$target,pass:false,skipped:true,evidence:$evidence,execution:($requirements + {platform:$platform})}'
      rm -rf "$directory"
      return 77
    fi
  else
    status="$?"
    cat "$directory/log" >&2
    if [[ "$status" -eq 77 ]]; then
      reason="$(<"$directory/reason")"
      evidence="$(jq -r '.reason' <<<"$reason")"
      verifier_emit "$target" skipped "$evidence" "$platform" "$requirements" "$reason"
      printf 'SKIP: %s: %s\n' "$target" "$evidence" >&2
      rm -rf "$directory"
      return 77
    fi
    evidence="$(head -c 4000 "$directory/log")"
    [[ -n "$evidence" ]] || evidence="preflight exited $status"
  fi
  verifier_emit "$target" failed "$evidence" "$platform" "$requirements"
  rm -rf "$directory"
  return 1
}

verifier_batch_exit() {
  local failed="$1" skipped="$2"
  if [[ "$failed" -ne 0 ]]; then
    printf '%s verifier(s) failed; %s skipped\n' "$failed" "$skipped" >&2
    return 1
  fi
  if [[ "$skipped" -ne 0 ]]; then
    printf '%s verifier(s) skipped; coverage remains unverified\n' "$skipped" >&2
    return 77
  fi
}
