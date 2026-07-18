{
  pkgs,
  system,
}:

let
  inherit (builtins) attrNames concatStringsSep;
  inherit (pkgs.lib) escapeShellArg sort;
  inherit (pkgs)
    bash
    coreutils
    findutils
    gawk
    git
    gnugrep
    gnused
    jq
    nix
    openssh
    prek
    python3
    writeShellScriptBin
    ;

  domainRegistries = [
    (import ./beads.nix { inherit pkgs system; })
    (import ./cli.nix { inherit pkgs system; })
    (import ./images.nix { inherit pkgs system; })
    (import ./linux-builder.nix { inherit pkgs system; })
    (import ./notifications.nix { inherit pkgs system; })
    (import ./playwright-mcp.nix { inherit pkgs system; })
    (import ./prek.nix { inherit pkgs system; })
    (import ./profiles.nix { inherit pkgs system; })
    (import ./sandbox.nix { inherit pkgs system; })
    (import ./security.nix { inherit pkgs system; })
    (import ./services.nix { inherit pkgs system; })
    (import ./tmux-mcp.nix { inherit pkgs system; })
  ];
  registry = builtins.foldl' (acc: next: acc // next) { } domainRegistries;
  targetNames = sort builtins.lessThan (attrNames registry);
  listArguments = concatStringsSep " \\\n        " (
    map (target: escapeShellArg "verify:${target}") targetNames
  );
  knownPatterns = concatStringsSep "|" (map escapeShellArg targetNames);
  caseArms = concatStringsSep "\n" (
    map (target: ''
      ${escapeShellArg target})
        ${registry.${target}}
        ;;
    '') targetNames
  );

  verify = writeShellScriptBin "verify" ''
    set -euo pipefail

    export PATH="${bash}/bin:${coreutils}/bin:${findutils}/bin:${gawk}/bin:${git}/bin:${gnugrep}/bin:${gnused}/bin:${jq}/bin:${nix}/bin:${openssh}/bin:${prek}/bin:${python3}/bin:$PATH"
    SELF="$0"

    fail() {
      local message="$1"
      printf 'FAIL: %s\n' "$message" >&2
      return 1
    }

    list_targets() {
      printf '%s\n' \
        ${listArguments}
    }

    usage() {
      printf 'Usage: nix run .#verify -- [--list] <id>...\n'
      printf 'IDs may be passed as verify:<domain>.<check-id> or <domain>.<check-id>.\n'
    }

    normalize_target() {
      local target="$1"
      case "$target" in
        verify:*) printf '%s\n' "''${target#verify:}" ;;
        *) printf '%s\n' "$target" ;;
      esac
    }

    is_known_target() {
      local target="$1"
      case "$target" in
        ${knownPatterns}) return 0 ;;
        *) return 1 ;;
      esac
    }

    validate_targets() {
      local raw
      local target
      local unknown=0
      for raw in "$@"; do
        target="$(normalize_target "$raw")"
        if ! is_known_target "$target"; then
          printf 'Unknown verify target: %s\n' "$raw" >&2
          unknown=1
        fi
      done
      if [[ "$unknown" -ne 0 ]]; then
        printf 'Run `nix run .#verify -- --list` to see supported targets.\n' >&2
        printf 'Supported verify targets:\n' >&2
        list_targets >&2
        return 64
      fi
    }

    repo_root() {
      if [[ -n "''${REPO_ROOT:-}" ]]; then
        printf '%s\n' "$REPO_ROOT"
      else
        git rev-parse --show-toplevel
      fi
    }

    run_repo_script() {
      local relative_path="$1"
      shift
      local root
      root="$(repo_root)"
      REPO_ROOT="$root" bash "$root/$relative_path" "$@"
    }

    run_repo_script_with_wrix() {
      local relative_path="$1"
      shift
      local root
      local package
      root="$(repo_root)"
      package="$(build_flake_package wrix)"
      WRIX_TEST_WRIX_BIN="$package/bin/wrix" REPO_ROOT="$root" bash "$root/$relative_path" "$@"
    }

    build_flake_package() {
      local attr="$1"
      local root
      root="$(repo_root)"
      nix build --no-link --print-out-paths --no-warn-dirty "$root#$attr"
    }

    assert_contains() {
      local label="$1"
      local haystack="$2"
      local needle="$3"
      if [[ "$haystack" != *"$needle"* ]]; then
        fail "$label: missing '$needle'"
      fi
    }

    assert_executable() {
      local path="$1"
      if [[ ! -x "$path" ]]; then
        fail "expected executable at $path"
      fi
    }

    assert_package_attr_absent() {
      local attr="$1"
      local root
      local out_file
      local err_file
      root="$(repo_root)"
      out_file="$(mktemp -t wrix-verify-attr.XXXXXX)"
      err_file="$(mktemp -t wrix-verify-attr.XXXXXX)"
      if nix build --no-link --no-warn-dirty "$root#$attr" >"$out_file" 2>"$err_file"; then
        rm -f "$out_file" "$err_file"
        fail "legacy package attr is still exposed: $attr"
      fi
      rm -f "$out_file" "$err_file"
    }

    assert_json_verdict() {
      local label="$1"
      local json_lines="$2"
      local target="$3"
      if ! printf '%s\n' "$json_lines" | jq -e --arg target "$target" 'select(.target == $target and .pass == true)' >/dev/null; then
        fail "$label: missing passing JSON verdict for $target"
      fi
    }

    emit_verdict() {
      local target="$1"
      local pass="$2"
      local evidence="$3"
      jq -cn --arg target "$target" --argjson pass "$pass" --arg evidence "$evidence" '{target:$target,pass:$pass,evidence:$evidence}'
    }

    summarize_evidence() {
      local path="$1"
      local fallback="$2"
      local evidence
      evidence="$(head -c 4000 "$path")"
      if [[ -z "$evidence" ]]; then
        evidence="$fallback"
      fi
      printf '%s\n' "$evidence"
    }

    run_target() {
      local target="$1"
      case "$target" in
    ${caseArms}
        *) fail "internal dispatcher received unknown target: $target" ;;
      esac
    }

    run_one() {
      local target="$1"
      local out_file
      local status
      local evidence
      out_file="$(mktemp -t wrix-verify.XXXXXX)"
      set +e
      (
        set -euo pipefail
        run_target "$target"
      ) >"$out_file" 2>&1
      status="$?"
      set -e
      if [[ "$status" -eq 0 ]]; then
        rm -f "$out_file"
        emit_verdict "$target" true passed
        return 0
      fi
      evidence="$(summarize_evidence "$out_file" "failed with exit $status")"
      if [[ "$status" -eq 77 ]]; then
        cat "$out_file" >&2
        rm -f "$out_file"
        emit_verdict "$target" false "skipped: $evidence"
        return 1
      fi
      cat "$out_file" >&2
      rm -f "$out_file"
      emit_verdict "$target" false "$evidence"
      return 1
    }

    main() {
      if [[ "$#" -eq 0 ]]; then
        usage >&2
        return 64
      fi

      case "''${1:-}" in
        --help|-h)
          usage
          return 0
          ;;
        --list)
          if [[ "$#" -ne 1 ]]; then
            fail "--list cannot be combined with target IDs"
          fi
          list_targets
          return 0
          ;;
      esac

      validate_targets "$@"

      local raw
      local target
      local failed=0
      for raw in "$@"; do
        target="$(normalize_target "$raw")"
        if ! run_one "$target"; then
          failed=$((failed + 1))
        fi
      done
      if [[ "$failed" -ne 0 ]]; then
        printf '%s verify target(s) failed\n' "$failed" >&2
        return 1
      fi
    }

    main "$@"
  '';
in
{
  app = {
    meta.description = "Run repository verify targets.";
    type = "app";
    program = "${verify}/bin/verify";
  };
  package = verify;
  targets = map (target: "verify:${target}") targetNames;
}
