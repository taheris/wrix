{
  pkgs,
  system,
  linuxPkgs,
}:

let
  inherit (builtins)
    attrNames
    concatStringsSep
    mapAttrs
    toJSON
    ;
  inherit (pkgs.lib)
    escapeShellArg
    makeBinPath
    optionals
    sort
    ;
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

  wrixPrek = import ../../lib/prek/runner.nix { inherit pkgs; };

  domainRegistries = [
    (import ./beads.nix { inherit pkgs system; })
    (import ./cli.nix { inherit pkgs system; })
    (import ./images.nix { inherit pkgs system; })
    (import ./linux-builder.nix { inherit pkgs system linuxPkgs; })
    (import ./notifications.nix { inherit pkgs system; })
    (import ./playwright-mcp.nix { inherit pkgs system; })
    (import ./prek.nix { inherit pkgs system; })
    (import ./profiles.nix { inherit pkgs system; })
    (import ./sandbox.nix { inherit pkgs system; })
    (import ./security.nix { inherit pkgs system; })
    (import ./services.nix { inherit pkgs system; })
    (import ./tmux-mcp.nix { inherit pkgs system; })
  ];
  registry = mapAttrs (_: entry: if builtins.isString entry then { script = entry; } else entry) (
    builtins.foldl' (acc: next: acc // next) { } domainRegistries
  );
  requirements = pkgs.writeText "verify-requirements.json" (
    toJSON (
      mapAttrs (_: entry: {
        platforms = entry.platforms or [ ];
        capabilities = entry.capabilities or [ ];
      }) registry
    )
  );
  inputDefinition = import ../lib/inputs.nix { };
  inputDescriptions = pkgs.writeText "verify-inputs.json" (toJSON (inputDefinition.project registry));
  preflightPath = makeBinPath (
    optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.podman
      pkgs.util-linux
    ]
  );
  targetNames = sort builtins.lessThan (attrNames registry);
  listArguments = concatStringsSep " \\\n        " (
    map (target: escapeShellArg "verify:${target}") targetNames
  );
  knownPatterns = concatStringsSep "|" (map escapeShellArg targetNames);
  caseArms = concatStringsSep "\n" (
    map (target: ''
      ${escapeShellArg target})
        ${registry.${target}.script}
        ;;
    '') targetNames
  );

  verify = writeShellScriptBin "verify" ''
    set -euo pipefail

    export PATH="${bash}/bin:${coreutils}/bin:${findutils}/bin:${gawk}/bin:${git}/bin:${gnugrep}/bin:${gnused}/bin:${jq}/bin:${nix}/bin:${openssh}/bin:${prek}/bin:${python3}/bin:${wrixPrek}/bin:$PATH"
    SELF="$0"
    export PATH="${preflightPath}:$PATH"
    source ${../lib/verifier.sh}
    source ${../lib/print-inputs.sh}

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
      printf 'Usage: nix run .#verify -- [--list | --print-inputs] <id>...\n'
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
      if ! printf '%s\n' "$json_lines" | jq -e --arg target "$target" 'select(.target == $target and .outcome == "passed")' >/dev/null; then
        fail "$label: missing passing JSON verdict for $target"
      fi
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
      local requirements
      requirements="$(jq -c --arg target "$target" '.[$target]' ${requirements})"
      verifier_run "$target" "$requirements" bash "$SELF" --execute-target "$target"
    }

    main() {
      if [[ "$#" -eq 0 ]]; then
        usage >&2
        return 64
      fi

      case "''${1:-}" in
        --execute-target)
          [[ "$#" -eq 2 ]] || fail "internal target execution requires one ID"
          is_known_target "$2" || fail "unknown internal target: $2"
          run_target "$2"
          return
          ;;
        --help|-h)
          usage
          return 0
          ;;
        --print-inputs)
          shift
          validate_targets "$@"
          local raw
          local targets=()
          for raw in "$@"; do
            targets+=("$(normalize_target "$raw")")
          done
          verifier_print_inputs ${inputDescriptions} "''${targets[@]}"
          return
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
      local failed=0 skipped=0 status
      for raw in "$@"; do
        target="$(normalize_target "$raw")"
        if run_one "$target"; then
          continue
        else
          status="$?"
        fi
        case "$status" in
          77) skipped=$((skipped + 1)) ;;
          *) failed=$((failed + 1)) ;;
        esac
      done
      verifier_batch_exit "$failed" "$skipped"
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
