#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"
  printf 'SKIP: %s\n' "$reason" >&2
  exit 77
}

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

command -v nix >/dev/null 2>&1 || skip "nix not on PATH"
command -v jq >/dev/null 2>&1 || skip "jq not on PATH"

result=$(nix eval --impure --no-warn-dirty --json --expr "
  let
    flake = builtins.getFlake \"git+file://$REPO_ROOT\";
    system = builtins.currentSystem;
    lib = flake.legacyPackages.\${system}.lib;
    directAttempt = builtins.tryEval ((lib.mkSandbox {
      profile = lib.profiles.base;
      agent = \"direct\";
      agentSettings = { env.WRIX_AGENT_SETTINGS_PROBE = \"direct\"; };
    }).package.drvPath);
    claude = lib.mkSandbox {
      profile = lib.profiles.base;
      agent = \"claude\";
      agentSettings = {
        env = {
          ANTHROPIC_MODEL = \"wrix-agent-settings-probe\";
          WRIX_AGENT_SETTINGS_PROBE = \"1\";
        };
      };
    };
    pi = lib.mkSandbox {
      profile = lib.profiles.base;
      agent = \"pi\";
      agentSettings = {
        defaultModel = \"wrix-pi-settings-probe\";
        customProbe = \"1\";
      };
    };
    claudeSettings = builtins.fromJSON (builtins.readFile claude.image.claudeSettingsJson);
    piSettings = builtins.fromJSON (builtins.readFile pi.image.piSettingsJson);
  in
  {
    directRejected = directAttempt.success == false;
    claudeModel = claudeSettings.env.ANTHROPIC_MODEL or \"\";
    claudeProbe = claudeSettings.env.WRIX_AGENT_SETTINGS_PROBE or \"\";
    piModel = piSettings.defaultModel or \"\";
    piProbe = piSettings.customProbe or \"\";
  }
")

if ! jq -e '
  .directRejected == true and
  .claudeModel == "wrix-agent-settings-probe" and
  .claudeProbe == "1" and
  .piModel == "wrix-pi-settings-probe" and
  .piProbe == "1"
' <<<"$result" >/dev/null; then
  fail "agentSettings contract failed: $result"
fi

printf 'PASS: agentSettings merges into selected agent settings and direct rejects non-empty settings\n' >&2
