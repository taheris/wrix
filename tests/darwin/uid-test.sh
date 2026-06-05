#!/usr/bin/env bash
# Container UID mapping verification test script
# This runs INSIDE the container to verify unshare-based UID mapping works
#
# VirtioFS maps all files to UID 0 inside the container. The entrypoint uses
# unshare(1) to create a user namespace that maps inner HOST_UID to outer UID 0,
# so all files (including VirtioFS mounts) appear with correct ownership.
set -euo pipefail

# Darwin-only test: uses darwin-specific UID mapping (VirtioFS, unshare with HOST_UID).
# Platform gating is at the Nix level (tests/darwin/default.nix); this script runs
# inside a Linux container on a Darwin host, so uname returns "Linux" here.

# Precondition: VirtioFS must be in use. VirtioFS maps all files to UID 0 inside
# the container. When running outside an Apple Container VM (e.g., via wrix-mcp
# on Linux), VirtioFS is not present and this test is not applicable.
WORKSPACE_OWNER_CHECK=$(stat -c %u /workspace 2>/dev/null || echo "unknown")
if [ "$WORKSPACE_OWNER_CHECK" != "0" ]; then
  echo "SKIP: VirtioFS not detected (/workspace owned by UID $WORKSPACE_OWNER_CHECK, expected 0)"
  echo "This test only applies to Apple Container CLI VMs with VirtioFS mounts."
  exit 77
fi

echo "=== Container UID Mapping Verification ==="
echo "Running as: $(id)"
echo "HOME: $HOME"
echo ""

FAILED=0

# Test 1: VirtioFS files appear as root before unshare
echo "Test 1: VirtioFS ownership (before unshare)"
WORKSPACE_OWNER=$(stat -c %u /workspace 2>/dev/null)
GIT_OWNER=$(stat -c %u /workspace/.git 2>/dev/null)
echo "  /workspace owner UID: $WORKSPACE_OWNER"
echo "  /workspace/.git owner UID: $GIT_OWNER"
if [ "$WORKSPACE_OWNER" = "0" ] && [ "$GIT_OWNER" = "0" ]; then
  echo "  PASS: VirtioFS maps to root as expected"
else
  echo "  FAIL: Expected UID 0, got workspace=$WORKSPACE_OWNER git=$GIT_OWNER"
  FAILED=1
fi
echo ""

# Test 2: After unshare, process runs as HOST_UID
echo "Test 2: Process UID after unshare"
INNER_UID=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- id -u)
if [ "$INNER_UID" = "$HOST_UID" ]; then
  echo "  PASS: Process runs as UID $HOST_UID"
else
  echo "  FAIL: Expected UID $HOST_UID, got $INNER_UID"
  FAILED=1
fi
echo ""

# Test 3: VirtioFS files appear as HOST_UID inside unshare namespace
echo "Test 3: VirtioFS ownership inside unshare namespace"
INNER_WORKSPACE_OWNER=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- stat -c %u /workspace)
INNER_GIT_OWNER=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- stat -c %u /workspace/.git)
if [ "$INNER_WORKSPACE_OWNER" = "$HOST_UID" ] && [ "$INNER_GIT_OWNER" = "$HOST_UID" ]; then
  echo "  PASS: VirtioFS files appear as UID $HOST_UID"
else
  echo "  FAIL: Expected UID $HOST_UID, got workspace=$INNER_WORKSPACE_OWNER git=$INNER_GIT_OWNER"
  FAILED=1
fi
echo ""

# Test 4: git works inside unshare namespace (the actual bug this fixes)
echo "Test 4: git rev-parse inside unshare namespace"
GIT_OUTPUT=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
  git -C /workspace rev-parse --show-toplevel 2>&1)
GIT_EXIT=$?
if [ "$GIT_EXIT" -eq 0 ] && [ "$GIT_OUTPUT" = "/workspace" ]; then
  echo "  PASS: git rev-parse succeeds (no dubious ownership)"
else
  echo "  FAIL: git rev-parse failed (exit=$GIT_EXIT): $GIT_OUTPUT"
  FAILED=1
fi
echo ""

# Test 5: Files created inside namespace appear with correct UID on host
echo "Test 5: File creation inside unshare namespace"
unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
  bash -c 'echo "uid-test" > /workspace/.uid-mapping-test'
if [ -f /workspace/.uid-mapping-test ]; then
  # Inside the outer namespace (root), the file appears as root-owned
  FILE_OWNER=$(stat -c %u /workspace/.uid-mapping-test)
  echo "  File owner (outer namespace): $FILE_OWNER"
  if [ "$FILE_OWNER" = "0" ]; then
    echo "  PASS: File created with correct ownership mapping"
  else
    echo "  FAIL: Expected owner UID 0 in outer namespace, got $FILE_OWNER"
    FAILED=1
  fi
  rm /workspace/.uid-mapping-test
else
  echo "  FAIL: Could not create file in workspace"
  FAILED=1
fi
echo ""

# Test 6: HOME directory accessible inside namespace
echo "Test 6: HOME directory access inside unshare namespace"
HOME_OWNER=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- stat -c %u "$HOME")
HOME_WRITABLE=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- \
  bash -c "touch \$HOME/.uid-test && echo yes && rm \$HOME/.uid-test" 2>&1)
if [ "$HOME_OWNER" = "$HOST_UID" ] && [ "$HOME_WRITABLE" = "yes" ]; then
  echo "  PASS: HOME owned by $HOST_UID and writable"
else
  echo "  FAIL: HOME owner=$HOME_OWNER writable=$HOME_WRITABLE"
  FAILED=1
fi
echo ""

# Test 7: Username resolves correctly
echo "Test 7: Username resolution"
USERNAME=$(unshare --user --map-user="$HOST_UID" --map-group="$HOST_UID" -- id -un 2>/dev/null || echo "unknown")
echo "  Username: $USERNAME"
if [ "$USERNAME" = "wrix" ]; then
  echo "  PASS: Username resolves to wrix"
else
  echo "  WARN: Username is '$USERNAME' (expected 'wrix')"
  # Not a hard failure — depends on /etc/passwd setup
fi
echo ""

if [ "$FAILED" -eq 0 ]; then
  echo "=== ALL UID MAPPING TESTS PASSED ==="
  exit 0
else
  echo "=== SOME UID MAPPING TESTS FAILED ==="
  exit 1
fi
