#!/usr/bin/env bash
set -euo pipefail

wrix_prepare_mcp_manifest() {
  local available_manifest="/etc/wrix/mcp-available.json"
  local selected_manifest="/tmp/wrix-mcp-manifest.json"
  local selection="${WRIX_MCP-all}"

  if [[ ! -f "$available_manifest" ]]; then
    unset WRIX_MCP_MANIFEST
    return 0
  fi

  if ! jq \
    --arg selection "$selection" \
    '
      def selected_names:
        $selection
        | split(",")
        | map(gsub("^\\s+|\\s+$"; ""))
        | map(select(length > 0))
        | unique;
      .servers as $available
      | if .runtime_selection then
          (if $selection == "all" then [$available[].name] else selected_names end) as $selected
          | ([$available[].name]) as $availableNames
          | ($selected - $availableNames) as $unknown
          | if $unknown != [] then
              error("WRIX_MCP selects unknown servers: " + ($unknown | join(", ")))
            else
              {
                schema: 1,
                servers: ($available | map(select(.name as $name | $selected | index($name))))
              }
            end
        else
          { schema: 1, servers: $available }
        end
    ' \
    "$available_manifest" >"$selected_manifest"; then
    printf 'Error: failed to select MCP servers from %s\n' "$available_manifest" >&2
    rm -f "$selected_manifest"
    return 1
  fi

  chmod 0600 "$selected_manifest"
  export WRIX_MCP_MANIFEST="$selected_manifest"
}

wrix_configure_pi_mcp() {
  local config_dir="$HOME/.pi/agent"
  local config_tmp
  mkdir -p "$config_dir"
  config_tmp=$(mktemp "$config_dir/mcp.json.XXXXXX")
  if [[ -n "${WRIX_MCP_MANIFEST:-}" ]]; then
    if ! jq '{mcpServers: (.servers | map({key: .name, value: {
      command: .command, args: .args, env: .env, exposure: "codemode"
    }}) | from_entries)}' "$WRIX_MCP_MANIFEST" >"$config_tmp"; then
      rm -f "$config_tmp"
      return 1
    fi
  else
    printf '{"mcpServers":{}}\n' >"$config_tmp"
  fi
  chmod 0600 "$config_tmp"
  mv -f "$config_tmp" "$config_dir/mcp.json"
}
