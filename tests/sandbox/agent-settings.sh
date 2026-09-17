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
    direct = lib.mkSandbox {
      profile = lib.profiles.base;
      agent = \"direct\";
    };
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
    piDefault = lib.mkSandbox {
      profile = lib.profiles.base;
      agent = \"pi\";
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
    piDefaultSettings = builtins.fromJSON (builtins.readFile piDefault.image.piSettingsJson);
    piSettings = builtins.fromJSON (builtins.readFile pi.image.piSettingsJson);
  in
  {
    directRejected = directAttempt.success == false;
    directConfigAbsent = direct.image.claudeConfigJson == null
      && direct.image.claudeSettingsJson == null
      && direct.image.piSettingsJson == null;
    claudeConfigScoped = claude.image.claudeConfigJson != null
      && claude.image.claudeSettingsJson != null
      && claude.image.piSettingsJson == null;
    piConfigScoped = pi.image.claudeConfigJson == null
      && pi.image.claudeSettingsJson == null
      && pi.image.piSettingsJson != null;
    claudeModel = claudeSettings.env.ANTHROPIC_MODEL or \"\";
    claudeProbe = claudeSettings.env.WRIX_AGENT_SETTINGS_PROBE or \"\";
    piDefaultModel = piDefaultSettings.defaultModel or \"\";
    piEditorPadding = piSettings.editorPaddingX or null;
    piInstallTelemetry = piSettings.enableInstallTelemetry or null;
    piModel = piSettings.defaultModel or \"\";
    piProbe = piSettings.customProbe or \"\";
  }
")

if ! jq -e '
  .directRejected == true and
  .directConfigAbsent == true and
  .claudeConfigScoped == true and
  .piConfigScoped == true and
  .claudeModel == "wrix-agent-settings-probe" and
  .claudeProbe == "1" and
  .piDefaultModel == "gpt-6-astra" and
  .piEditorPadding == 1 and
  .piInstallTelemetry == false and
  .piModel == "wrix-pi-settings-probe" and
  .piProbe == "1"
' <<<"$result" >/dev/null; then
  fail "agentSettings contract failed: $result"
fi

printf 'PASS: agent settings and selected-agent config scoping are preserved\n' >&2
