#!/usr/bin/env bash
# Container network verification test script
# This runs INSIDE the container to verify networking is working
set -euo pipefail

# Darwin-only test: uses darwin-specific networking (VZNATNetworkDeviceAttachment, vmnet).
# Platform gating is at the Nix level (tests/darwin/default.nix); this script runs
# inside a Linux container on a Darwin host, so uname returns "Linux" here.

echo "=== Container Network Verification ==="
echo "Running as: $(id)"
echo "Date: $(date)"
echo ""

FAILED=0

# Test 1: Check network interfaces
# Pick whichever interface carries the default route rather than hard-coding
# eth0 — on Linux hosts the container may expose eno1/enp*/ens* etc., and on
# Darwin vmnet still routes through eth0.
echo "Test 1: Network interfaces"
if ip addr show 2>/dev/null; then
  DEFAULT_IF=$(ip route show default 2>/dev/null | awk '/^default/ {print $5; exit}')
  if [ -n "$DEFAULT_IF" ] && ip addr show "$DEFAULT_IF" 2>/dev/null | grep -q "inet "; then
    IF_IP=$(ip addr show "$DEFAULT_IF" | grep "inet " | awk '{print $2}')
    echo "  PASS: $DEFAULT_IF configured with $IF_IP"
  else
    echo "  FAIL: no default-route interface has an IPv4 address"
    FAILED=1
  fi
else
  echo "  FAIL: Cannot list interfaces"
  FAILED=1
fi
echo ""

# Test 2: Check routing table
echo "Test 2: Routing table"
if ip route show 2>/dev/null | grep -q "default via"; then
  GATEWAY=$(ip route show default | awk '{print $3}')
  echo "  PASS: Default route via $GATEWAY"
else
  echo "  FAIL: No default route"
  ip route show 2>/dev/null || true
  FAILED=1
fi
echo ""

# Test 3: Check DNS configuration
echo "Test 3: DNS configuration (/etc/resolv.conf)"
if [ -f /etc/resolv.conf ]; then
  cat /etc/resolv.conf
  # Verify explicit DNS servers are configured (Tailscale MagicDNS + Cloudflare)
  if grep -q "100.100.100.100" /etc/resolv.conf || grep -q "1.1.1.1" /etc/resolv.conf; then
    echo "  PASS: resolv.conf has explicit DNS servers"
  else
    echo "  PASS: resolv.conf exists (using vmnet DNS)"
  fi
else
  echo "  FAIL: /etc/resolv.conf not found"
  FAILED=1
fi
echo ""

# Test 4: Ping gateway (informational - requires cap_net_raw)
GATEWAY=$(ip route show default 2>/dev/null | awk '{print $3}')
echo "Test 4: Ping gateway ($GATEWAY)"
if [ -n "$GATEWAY" ] && ping -c 2 -W 5 "$GATEWAY" >/dev/null 2>&1; then
  echo "  PASS: Gateway reachable via ICMP"
else
  echo "  INFO: ICMP unavailable (expected without cap_net_raw)"
fi
echo ""

# Test 5: TCP connectivity to external IP (REQUIRED - verifies network stack works)
echo "Test 5: TCP connectivity (curl http://1.1.1.1)"
if command -v curl >/dev/null 2>&1; then
  if curl -sS --connect-timeout 5 --max-time 10 -o /dev/null http://1.1.1.1 2>/dev/null; then
    echo "  PASS: TCP connectivity works"
  else
    echo "  FAIL: TCP connectivity failed"
    FAILED=1
  fi
else
  echo "  FAIL: curl not available"
  FAILED=1
fi
echo ""

# Test 6: DNS resolution (informational - depends on DNS server availability)
echo "Test 6: DNS resolution (cloudflare.com)"
if getent hosts cloudflare.com >/dev/null 2>&1; then
  echo "  INFO: DNS resolution works"
  getent hosts cloudflare.com
else
  echo "  INFO: DNS resolution unavailable"
fi
echo ""

# Test 7: HTTPS connectivity (REQUIRED - verifies CA certs and full internet access)
echo "Test 7: External HTTPS connectivity"
if command -v curl >/dev/null 2>&1; then
  if curl -sS --connect-timeout 5 --max-time 10 -o /dev/null https://cloudflare.com 2>/dev/null; then
    echo "  PASS: HTTPS connectivity works (CA certs present)"
  else
    echo "  FAIL: HTTPS connectivity failed"
    FAILED=1
  fi
else
  echo "  FAIL: curl not available"
  FAILED=1
fi
echo ""

# Summary
echo "=== Network Diagnostics Summary ==="
echo "Interfaces:"
ip -4 addr show 2>/dev/null | grep inet || echo "  (could not get IPs)"
echo ""
echo "Default route:"
ip route show default 2>/dev/null || echo "  (no default route)"
echo ""

if [ "$FAILED" -eq 0 ]; then
  echo "=== NETWORK TESTS PASSED ==="
  echo "(TCP + HTTPS verified. ICMP requires cap_net_raw.)"
  exit 0
else
  echo "=== NETWORK TESTS FAILED ==="
  exit 1
fi
