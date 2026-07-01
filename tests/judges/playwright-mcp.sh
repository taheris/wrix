#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for playwright-mcp.md success criteria

test_chromium_path_derivation() {
  judge_files "lib/mcp/playwright/default.nix"
  judge_criterion "Chromium executable path is derived from pkgs.playwright-driver.browsers using the revision from browsersJSON.chromium.revision, constructing the path as browsers/chromium-REVISION/chrome-linux64/chrome rather than hardcoding any revision number"
}

test_container_flags() {
  judge_files "lib/mcp/playwright/default.nix"
  judge_criterion "The mandatory Chromium flags --no-sandbox, --disable-dev-shm-usage, and --disable-gpu are always present in browser.launchOptions.args, placed before any user-provided args, and cannot be overridden or removed by user config"
}

test_config_passthrough() {
  judge_files "lib/mcp/playwright/default.nix"
  judge_criterion "The mkServerConfig function accepts headless, viewport, and config options; headless controls browser.launchOptions.headless, viewport maps to contextOptions.viewport, and config.launchOptions.args are appended after mandatory flags while other config keys are merged into the top-level JSON config"
}

test_registry_pattern() {
  judge_files "lib/mcp/playwright/default.nix" "lib/mcp/tmux/default.nix"
  judge_criterion "The playwright server definition follows the same MCP registry pattern as tmux: exports name (string), packages (list of derivations), and mkServerConfig (function returning attrset with command, and optionally args and env)"
}
