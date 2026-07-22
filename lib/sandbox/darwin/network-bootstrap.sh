#!/bin/bash
set -euo pipefail

# This is the only Darwin stage that receives CAP_NET_ADMIN. It uses binaries
# pinned into the image at build time, installs and verifies the network policy,
# then replaces itself with the agent entrypoint after dropping NET_ADMIN from
# every capability set. No workspace path or workspace-controlled executable is
# consulted in this stage.
readonly WRIX_NETWORK_TOOL_DIR="/usr/local/libexec/wrix-network"
readonly WRIX_AGENT_ENTRYPOINT="/entrypoint.sh"
readonly WRIX_NETWORK_READY_FILE="/run/wrix-network-ready"
readonly WRIX_NETWORK_READY_DIR="${WRIX_NETWORK_READY_FILE%/*}"

wrix_die() {
  echo "Error: $*" >&2
  exit 1
}

[[ -x "$WRIX_AGENT_ENTRYPOINT" ]] || wrix_die "Darwin agent entrypoint is unavailable"
[[ -d "$WRIX_NETWORK_READY_DIR" && ! -L "$WRIX_NETWORK_READY_DIR" ]] \
  || wrix_die "trusted Darwin runtime directory is unavailable: $WRIX_NETWORK_READY_DIR"
[[ ! -e "$WRIX_NETWORK_READY_FILE" ]] || wrix_die "Darwin network bootstrap marker already exists"

WRIX_NFT_BIN="$WRIX_NETWORK_TOOL_DIR/nft"
WRIX_IPTABLES_BIN="$WRIX_NETWORK_TOOL_DIR/iptables"
WRIX_IP6TABLES_BIN="$WRIX_NETWORK_TOOL_DIR/ip6tables"
WRIX_CAPSH_BIN="$WRIX_NETWORK_TOOL_DIR/capsh"
WRIX_GETENT_BIN="$WRIX_NETWORK_TOOL_DIR/getent"
WRIX_AWK_BIN="$WRIX_NETWORK_TOOL_DIR/awk"
WRIX_SORT_BIN="$WRIX_NETWORK_TOOL_DIR/sort"
WRIX_GREP_BIN="$WRIX_NETWORK_TOOL_DIR/grep"
WRIX_NC_BIN="$WRIX_NETWORK_TOOL_DIR/nc"
WRIX_SLEEP_BIN="$WRIX_NETWORK_TOOL_DIR/sleep"

for tool in \
  "$WRIX_CAPSH_BIN" \
  "$WRIX_GETENT_BIN" \
  "$WRIX_AWK_BIN" \
  "$WRIX_SORT_BIN" \
  "$WRIX_GREP_BIN"; do
  [[ -x "$tool" ]] || wrix_die "trusted Darwin network tool is unavailable: $tool"
done

WRIX_FIREWALL_BACKEND="${WRIX_FIREWALL_BACKEND:-}"
case "$WRIX_FIREWALL_BACKEND" in
  "")
    if [[ -x "$WRIX_NFT_BIN" ]]; then
      WRIX_FIREWALL_BACKEND="nft"
    else
      WRIX_FIREWALL_BACKEND="iptables"
    fi
    ;;
  nft|iptables) ;;
  *) wrix_die "WRIX_FIREWALL_BACKEND must be 'nft' or 'iptables' (got: $WRIX_FIREWALL_BACKEND)" ;;
esac
if [[ "$WRIX_FIREWALL_BACKEND" = "nft" ]]; then
  [[ -x "$WRIX_NFT_BIN" ]] || wrix_die "nft is required for sandbox network policy"
else
  [[ -x "$WRIX_IPTABLES_BIN" ]] || wrix_die "iptables is required for sandbox network policy"
  [[ -x "$WRIX_IP6TABLES_BIN" ]] || wrix_die "ip6tables is required for sandbox IPv6 blocking"
fi

# BEGIN wrix network policy
wrix_nft() {
  "$WRIX_NFT_BIN" "$@" || wrix_die "nft $* failed"
}

wrix_nft_load_base_ruleset() {
  "$WRIX_NFT_BIN" -f - <<'NFT' || wrix_die "nft base ruleset load failed"
flush ruleset
table inet wrix {
  chain input {
    type filter hook input priority 0; policy drop;
  }

  chain forward {
    type filter hook forward priority 0; policy drop;
  }

  chain output {
    type filter hook output priority 0; policy drop;
  }
}
NFT
}

wrix_iptables() {
  "$WRIX_IPTABLES_BIN" -w "$@" || wrix_die "iptables $* failed"
}

wrix_ip6tables() {
  "$WRIX_IP6TABLES_BIN" -w "$@" || wrix_die "ip6tables $* failed"
}

wrix_firewall_allow_ipv4() {
  local ip="$1"
  local port="$2"
  local proto="$3"
  case "$WRIX_FIREWALL_BACKEND" in
    nft)
      if [[ -n "$port" ]]; then
        wrix_nft add rule inet wrix output ip daddr "$ip" "$proto" dport "$port" accept
      else
        wrix_nft add rule inet wrix output ip daddr "$ip" accept
      fi
      ;;
    iptables)
      if [[ -n "$port" ]]; then
        wrix_iptables -A OUTPUT -p "$proto" -d "$ip" --dport "$port" -j ACCEPT
      else
        wrix_iptables -A OUTPUT -d "$ip" -j ACCEPT
      fi
      ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_firewall_reject_ipv4_cidr() {
  local cidr="$1"
  case "$WRIX_FIREWALL_BACKEND" in
    nft) wrix_nft add rule inet wrix output ip daddr "$cidr" reject ;;
    iptables) wrix_iptables -A OUTPUT -d "$cidr" -j REJECT ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_firewall_allow_public_ipv4() {
  case "$WRIX_FIREWALL_BACKEND" in
    nft) wrix_nft add rule inet wrix output meta nfproto ipv4 accept ;;
    iptables) wrix_iptables -A OUTPUT -j ACCEPT ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_ipv4_is_special() {
  local ip="$1"
  local first second third _fourth
  [[ "$ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
  IFS=. read -r first second third _fourth <<< "$ip"
  case "$first" in
    0|10|127|224|225|226|227|228|229|230|231|232|233|234|235|236|237|238|239|240|241|242|243|244|245|246|247|248|249|250|251|252|253|254|255)
      return 0
      ;;
    100) [[ "$second" -ge 64 && "$second" -le 127 ]] ;;
    169) [[ "$second" -eq 254 ]] ;;
    172) [[ "$second" -ge 16 && "$second" -le 31 ]] ;;
    192)
      [[ ( "$second" -eq 0 && ( "$third" -eq 0 || "$third" -eq 2 ) ) || ( "$second" -eq 88 && "$third" -eq 99 ) || "$second" -eq 168 ]]
      ;;
    198) [[ "$second" -eq 18 || "$second" -eq 19 || ( "$second" -eq 51 && "$third" -eq 100 ) ]] ;;
    203) [[ "$second" -eq 0 && "$third" -eq 113 ]] ;;
    *) return 1 ;;
  esac
}

wrix_resolve_ipv4() {
  local host="$1"
  local records
  if [[ "$host" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf '%s\n' "$host"
    return 0
  fi
  if ! records=$("$WRIX_GETENT_BIN" ahostsv4 "$host" | "$WRIX_AWK_BIN" "{print \$1}" | "$WRIX_SORT_BIN" -u); then
    return 1
  fi
  [[ -n "$records" ]] || return 1
  printf '%s\n' "$records"
}

wrix_allow_ipv4_host() {
  local host="$1"
  local port="$2"
  local proto="$3"
  local reason="$4"
  local records ip
  records=$(wrix_resolve_ipv4 "$host") || wrix_die "$reason host is unresolvable: $host"
  while IFS= read -r ip; do
    [[ -n "$ip" ]] || continue
    wrix_firewall_allow_ipv4 "$ip" "$port" "$proto"
  done <<< "$records"
}

wrix_allow_dns_exceptions() {
  local servers="${WRIX_NETWORK_DNS_SERVERS:-}"
  local kind server _rest
  local -a dns_servers
  if [[ -f /etc/resolv.conf ]]; then
    while read -r kind server _rest; do
      [[ "$kind" = "nameserver" && -n "${server:-}" ]] || continue
      servers="$servers,$server"
    done < /etc/resolv.conf
  fi
  IFS=',' read -ra dns_servers <<< "$servers"
  for server in "${dns_servers[@]}"; do
    [[ "$server" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
    wrix_firewall_allow_ipv4 "$server" 53 udp
    wrix_firewall_allow_ipv4 "$server" 53 tcp
  done
}

wrix_allow_local_endpoints() {
  local endpoints="${WRIX_NETWORK_LOCAL_ENDPOINTS:-}"
  local endpoint target proto host port
  local -a endpoint_entries
  if [[ -n "${BEADS_DOLT_SERVER_HOST:-}" && -n "${BEADS_DOLT_SERVER_PORT:-}" ]]; then
    endpoints="$endpoints,${BEADS_DOLT_SERVER_HOST}:${BEADS_DOLT_SERVER_PORT}/tcp"
  fi
  if [[ -n "${WRIX_NIX_CACHE_HOST:-}" && -n "${WRIX_NIX_CACHE_PORT:-}" ]]; then
    endpoints="$endpoints,${WRIX_NIX_CACHE_HOST}:${WRIX_NIX_CACHE_PORT}/tcp"
  fi
  if [[ -n "${WRIX_PROJECT_CACHE_HOST:-}" && -n "${WRIX_PROJECT_CACHE_PORT:-}" ]]; then
    endpoints="$endpoints,${WRIX_PROJECT_CACHE_HOST}:${WRIX_PROJECT_CACHE_PORT}/tcp"
  fi
  if [[ -n "${WRIX_NOTIFY_TCP:-}" && "$WRIX_NOTIFY_TCP" == *:* ]]; then
    endpoints="$endpoints,${WRIX_NOTIFY_TCP}/tcp"
  fi
  IFS=',' read -ra endpoint_entries <<< "$endpoints"
  for endpoint in "${endpoint_entries[@]}"; do
    [[ -n "$endpoint" ]] || continue
    proto="tcp"
    target="$endpoint"
    if [[ "$endpoint" = */* ]]; then
      proto="${endpoint##*/}"
      target="${endpoint%/*}"
    fi
    [[ "$proto" = "tcp" || "$proto" = "udp" ]] || wrix_die "invalid local endpoint protocol: $endpoint"
    [[ "$target" = *:* ]] || wrix_die "invalid local endpoint, expected host:port: $endpoint"
    host="${target%:*}"
    port="${target##*:}"
    [[ "$port" =~ ^[0-9]+$ ]] || wrix_die "invalid local endpoint port: $endpoint"
    wrix_allow_ipv4_host "$host" "$port" "$proto" "local endpoint"
  done
}

wrix_block_special_ipv4_ranges() {
  local cidr
  local blocked_cidrs=(
    0.0.0.0/8
    10.0.0.0/8
    100.64.0.0/10
    127.0.0.0/8
    169.254.0.0/16
    172.16.0.0/12
    192.0.0.0/24
    192.0.2.0/24
    192.88.99.0/24
    192.168.0.0/16
    198.18.0.0/15
    198.51.100.0/24
    203.0.113.0/24
    224.0.0.0/4
    240.0.0.0/4
  )
  for cidr in "${blocked_cidrs[@]}"; do
    wrix_firewall_reject_ipv4_cidr "$cidr"
  done
}

wrix_allow_limit_domains() {
  local domain records ip
  local -a allowlist_domains
  IFS=',' read -ra allowlist_domains <<< "${WRIX_NETWORK_ALLOWLIST:-}"
  for domain in "${allowlist_domains[@]}"; do
    [[ -n "$domain" ]] || continue
    records=$(wrix_resolve_ipv4 "$domain") || wrix_die "allowlist domain is unresolvable: $domain"
    while IFS= read -r ip; do
      [[ -n "$ip" ]] || continue
      if wrix_ipv4_is_special "$ip"; then
        wrix_die "allowlist domain resolves to blocked local/special address: $domain -> $ip"
      fi
      wrix_firewall_allow_ipv4 "$ip" "" ""
    done <<< "$records"
  done
}

wrix_verify_nft_policy() {
  local input_chain output_chain
  input_chain=$("$WRIX_NFT_BIN" list chain inet wrix input) || wrix_die "could not inspect nft INPUT policy"
  output_chain=$("$WRIX_NFT_BIN" list chain inet wrix output) || wrix_die "could not inspect nft OUTPUT policy"
  "$WRIX_GREP_BIN" -q 'policy drop' <<< "$input_chain" || wrix_die "INPUT default-drop policy was not installed"
  "$WRIX_GREP_BIN" -q 'policy drop' <<< "$output_chain" || wrix_die "OUTPUT default-drop policy was not installed"
  "$WRIX_GREP_BIN" -q 'ip daddr 10.0.0.0/8 reject' <<< "$output_chain" || wrix_die "local-network reject rules were not installed"
}

wrix_verify_iptables_policy() {
  local input_rules output_rules ip6_output_rules
  input_rules=$("$WRIX_IPTABLES_BIN" -S INPUT) || wrix_die "could not inspect iptables INPUT policy"
  output_rules=$("$WRIX_IPTABLES_BIN" -S OUTPUT) || wrix_die "could not inspect iptables OUTPUT policy"
  ip6_output_rules=$("$WRIX_IP6TABLES_BIN" -S OUTPUT) || wrix_die "could not inspect ip6tables OUTPUT policy"
  "$WRIX_GREP_BIN" -qx -- "-P INPUT DROP" <<< "$input_rules" || wrix_die "INPUT default-drop policy was not installed"
  "$WRIX_GREP_BIN" -qx -- "-P OUTPUT DROP" <<< "$output_rules" || wrix_die "OUTPUT default-drop policy was not installed"
  "$WRIX_IPTABLES_BIN" -C OUTPUT -d 10.0.0.0/8 -j REJECT || wrix_die "local-network reject rules were not installed"
  "$WRIX_GREP_BIN" -qx -- "-P OUTPUT DROP" <<< "$ip6_output_rules" || wrix_die "IPv6 OUTPUT default-drop policy was not installed"
}

wrix_verify_firewall_policy() {
  case "$WRIX_FIREWALL_BACKEND" in
    nft) wrix_verify_nft_policy ;;
    iptables) wrix_verify_iptables_policy ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
  printf 'Network policy verified: IPv6 output default-drop (firewall=%s)\n' "$WRIX_FIREWALL_BACKEND" >&2
}

wrix_reset_firewall_policy() {
  case "$WRIX_FIREWALL_BACKEND" in
    nft)
      wrix_nft_load_base_ruleset
      wrix_nft add rule inet wrix input iifname "lo" accept
      wrix_nft add rule inet wrix input ct state established,related accept
      wrix_nft add rule inet wrix output oifname "lo" accept
      wrix_nft add rule inet wrix output ct state established,related accept
      ;;
    iptables)
      wrix_iptables -F INPUT
      wrix_iptables -F FORWARD
      wrix_iptables -F OUTPUT
      wrix_iptables -P INPUT DROP
      wrix_iptables -P FORWARD DROP
      wrix_iptables -P OUTPUT DROP
      wrix_iptables -A INPUT -i lo -j ACCEPT
      wrix_iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
      wrix_iptables -A OUTPUT -o lo -j ACCEPT
      wrix_iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
      wrix_ip6tables -F INPUT
      wrix_ip6tables -F FORWARD
      wrix_ip6tables -F OUTPUT
      wrix_ip6tables -P INPUT DROP
      wrix_ip6tables -P FORWARD DROP
      wrix_ip6tables -P OUTPUT DROP
      ;;
    *) wrix_die "firewall backend was not selected" ;;
  esac
}

wrix_wait_for_route() {
  [[ "${WRIX_WAIT_FOR_ROUTE:-}" = "1" ]] || return 0
  [[ -x "$WRIX_NC_BIN" ]] || wrix_die "nc is required while waiting for the Darwin VM route"
  [[ -x "$WRIX_SLEEP_BIN" ]] || wrix_die "sleep is required while waiting for the Darwin VM route"
  local route_ok=false
  local i
  for ((i = 1; i <= 30; i++)); do
    if "$WRIX_NC_BIN" -z -w1 1.1.1.1 443 2>/dev/null; then
      route_ok=true
      break
    fi
    "$WRIX_SLEEP_BIN" 0.5
  done
  [[ "$route_ok" = true ]] || echo "Warning: network still unreachable after route fix timeout" >&2
  unset WRIX_WAIT_FOR_ROUTE
}

apply_wrix_network_policy() {
  local mode="${WRIX_NETWORK:-open}"
  case "$mode" in
    open|limit) ;;
    *) wrix_die "WRIX_NETWORK must be 'open' or 'limit' (got: $mode)" ;;
  esac

  echo "Network mode: $mode (local-network baseline enforced; firewall=$WRIX_FIREWALL_BACKEND)" >&2
  wrix_reset_firewall_policy
  wrix_allow_dns_exceptions
  wrix_allow_local_endpoints
  wrix_block_special_ipv4_ranges
  if [[ "$mode" = "limit" ]]; then
    wrix_allow_limit_domains
  else
    wrix_firewall_allow_public_ipv4
  fi
  wrix_verify_firewall_policy
}
# END wrix network policy

wrix_wait_for_route
apply_wrix_network_policy

umask 077
: > "$WRIX_NETWORK_READY_FILE"

# The command string is constant; the stage-2 path and original argv are passed
# positionally. capsh drops NET_ADMIN from the bounding set before exec, and the
# stage-2 entrypoint independently rejects the capability in every Linux set.
exec "$WRIX_CAPSH_BIN" --drop=cap_net_admin -- -c 'exec "$@"' wrix-network-bootstrap \
  "$WRIX_AGENT_ENTRYPOINT" "$@"
