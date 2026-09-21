# Repair default-network routes captured by a VPN before contacting the builder.
{ pkgs }:
''
  _vpn_conflict=false
  _fix_vmnet_route() {
    local _subnet _net _default_if _prefix _vmnet_if _route_table _interfaces
    # Best-effort discovery: an unavailable Apple network or host probe leaves routes unchanged.
    _subnet=$(container network inspect default 2>/dev/null \
      | ${pkgs.jq}/bin/jq -r '
          (if type == "array" then .[0] else . end)
          | .status.ipv4Subnet // empty
        ') || return 0
    [[ -z "$_subnet" ]] && return 0
    _net="''${_subnet%%/*}"
    _default_if=$(route -n get default 2>/dev/null \
      | awk '/interface:/{print $2}') || return 0
    [[ "$_default_if" == utun* ]] || return 0
    _vpn_conflict=true
    _prefix="''${_net%.*}"
    _route_table=$(netstat -rn 2>/dev/null) || return 0
    if grep -q "^''${_prefix}\.128.*bridge" <<<"$_route_table"; then
      return 0
    fi
    _interfaces=$(ifconfig 2>/dev/null) || return 0
    _vmnet_if=$(awk '
      /^[a-z][a-z0-9]*:/ {
        interface = $1
        sub(/:$/, "", interface)
      }
      /inet 192\.168\.64\./ {
        print interface
        exit
      }
    ' <<<"$_interfaces")
    if [[ -n "$_vmnet_if" ]]; then
      echo "Adding vmnet route (VPN detected on $_default_if)" >&2
      sudo route add -net "$_net/25" \
        -interface "$_vmnet_if"
      sudo route add -net "''${_net%.*}.128/25" \
        -interface "$_vmnet_if"
    fi
  }
  _fix_vmnet_route
''
