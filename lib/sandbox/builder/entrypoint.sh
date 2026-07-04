#!/bin/sh

# shellcheck disable=SC3010,SC3040
# Busybox ash supports `-o pipefail` and `[[ ]]` even though POSIX sh does not
set -euo pipefail

BUILDER_USER="builder"
BUILDER_UID=1000
BUILDER_HOME="/home/$BUILDER_USER"

. /usr/lib/wrix-builder/sshd.sh

# Verify /nix/store is populated (bootstrap is done by CLI before container start)
if [[ ! -d /nix/store ]] || [[ -z "$(/bin/ls -A /nix/store 2>/dev/null)" ]]; then
    echo "ERROR: /nix/store is empty. Run 'wrix-builder start' to initialize." >&2
    exit 1
fi

# Note: Permissions are set at bootstrap time (image build + CLI init).
# VirtioFS UID mapping shows host-owned files as owned by builder inside container,
# so no runtime chmod needed. Skipping chmod saves 30-60+ seconds on large stores.

echo "Configuring sshd..."
mkdir -p /etc/ssh
rm -f /etc/ssh/sshd_config
wrix_builder_write_sshd_config "$BUILDER_USER" /etc/ssh/sshd_config

# Install SSH host key from the host-user key directory.
HOST_KEY="/run/keys/host_ed25519"
if [[ ! -f "$HOST_KEY" ]] || [[ ! -f "${HOST_KEY}.pub" ]]; then
    echo "ERROR: Builder host key missing under /run/keys" >&2
    exit 1
fi

echo "Installing SSH host key..."
cp "$HOST_KEY" /etc/ssh/ssh_host_ed25519_key
cp "${HOST_KEY}.pub" /etc/ssh/ssh_host_ed25519_key.pub
chmod 600 /etc/ssh/ssh_host_ed25519_key
chmod 644 /etc/ssh/ssh_host_ed25519_key.pub

# Setup builder user if not exists
if ! id "$BUILDER_USER" >/dev/null 2>&1; then
    echo "Creating builder user..."
    echo "$BUILDER_USER:x:$BUILDER_UID:$BUILDER_UID::$BUILDER_HOME:/bin/bash" >> /etc/passwd
    echo "$BUILDER_USER:x:$BUILDER_UID:" >> /etc/group
fi

# Create home directory
mkdir -p "$BUILDER_HOME"
chown "$BUILDER_UID:$BUILDER_UID" "$BUILDER_HOME"

# Setup SSH directory
mkdir -p "$BUILDER_HOME/.ssh"
chmod 700 "$BUILDER_HOME/.ssh"
chown "$BUILDER_UID:$BUILDER_UID" "$BUILDER_HOME/.ssh"

# Install authorized keys from mounted keys
KEYS_FILE="/run/keys/client_ed25519.pub"
if [[ ! -f "$KEYS_FILE" ]]; then
    echo "ERROR: Builder client public key missing under /run/keys" >&2
    exit 1
fi

echo "Installing authorized keys..."
cp "$KEYS_FILE" "$BUILDER_HOME/.ssh/authorized_keys"
chmod 600 "$BUILDER_HOME/.ssh/authorized_keys"
chown "$BUILDER_UID:$BUILDER_UID" "$BUILDER_HOME/.ssh/authorized_keys"

# Configure the Nix daemon
# Use /run for the client endpoint to avoid VirtioFS permission issues with /nix
echo "Configuring nix..."
mkdir -p /etc/nix /run/nix
cat > /etc/nix/nix.conf <<EOF
experimental-features = nix-command flakes
sandbox = false
trusted-users = root $BUILDER_USER
max-jobs = auto
cores = 0
min-free = 1073741824
max-free = 3221225472
EOF

# Set NIX_DAEMON_SOCKET_PATH for SSH sessions (non-interactive commands need this)
echo "NIX_DAEMON_SOCKET_PATH=/run/nix/daemon.sock" > "$BUILDER_HOME/.ssh/environment"
chown "$BUILDER_UID:$BUILDER_UID" "$BUILDER_HOME/.ssh/environment"
chmod 600 "$BUILDER_HOME/.ssh/environment"

# Start the Nix daemon with its client endpoint under /run
echo "Starting nix-daemon..."
export NIX_DAEMON_SOCKET_PATH=/run/nix/daemon.sock
nix-daemon &

# Start sshd in background
echo "Starting sshd..."
/bin/sshd -e

# Keep container alive and reap zombies (act as init)
echo "Services started, waiting..."
while true; do
    sleep 60 &
    wait
done
