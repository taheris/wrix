#!/bin/sh

# shellcheck disable=SC3040
# Busybox ash supports `-o pipefail` even though POSIX sh does not
set -euo pipefail

BUILDER_USER="builder"
BUILDER_UID=1000
BUILDER_HOME="/home/$BUILDER_USER"

# Verify /nix/store is populated (bootstrap is done by CLI before container start)
if [ ! -d /nix/store ] || [ -z "$(/bin/ls -A /nix/store 2>/dev/null)" ]; then
    echo "ERROR: /nix/store is empty. Run 'wrapix-builder start' to initialize." >&2
    exit 1
fi

# Note: Permissions are set at bootstrap time (image build + CLI init).
# VirtioFS UID mapping shows host-owned files as owned by builder inside container,
# so no runtime chmod needed. Skipping chmod saves 30-60+ seconds on large stores.

# Generate sshd_config (can't use image's config as it may be a broken symlink
# when /nix is mounted from persistent store with different store paths)
echo "Configuring sshd..."
mkdir -p /etc/ssh
rm -f /etc/ssh/sshd_config  # Remove broken symlink if exists
cat > /etc/ssh/sshd_config <<EOF
Port 22
HostKey /etc/ssh/ssh_host_ed25519_key
AuthorizedKeysFile /home/%u/.ssh/authorized_keys
PasswordAuthentication no
PermitRootLogin no
AllowUsers $BUILDER_USER
PermitUserEnvironment yes
Subsystem sftp internal-sftp
EOF

# Install SSH host key from nix store (stable for publicHostKey in nix-darwin)
HOST_KEY="/run/keys/ssh_host_ed25519_key"
if [ -f "$HOST_KEY" ]; then
    echo "Installing SSH host key..."
    cp "$HOST_KEY" /etc/ssh/ssh_host_ed25519_key
    cp "${HOST_KEY}.pub" /etc/ssh/ssh_host_ed25519_key.pub
    chmod 600 /etc/ssh/ssh_host_ed25519_key
    chmod 644 /etc/ssh/ssh_host_ed25519_key.pub
else
    # Fallback: generate ephemeral key (publicHostKey will change on restart)
    echo "Warning: No host key at $HOST_KEY, generating ephemeral key..." >&2
    ssh-keygen -t ed25519 -f /etc/ssh/ssh_host_ed25519_key -N ""
fi

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
KEYS_FILE="/run/keys/builder_ed25519.pub"
if [ -f "$KEYS_FILE" ]; then
    echo "Installing authorized keys..."
    cp "$KEYS_FILE" "$BUILDER_HOME/.ssh/authorized_keys"
    chmod 600 "$BUILDER_HOME/.ssh/authorized_keys"
    chown "$BUILDER_UID:$BUILDER_UID" "$BUILDER_HOME/.ssh/authorized_keys"
else
    echo "Warning: No authorized keys found at $KEYS_FILE" >&2
fi

# Configure nix-daemon
# Use socket in /run to avoid VirtioFS permission issues with /nix
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
echo "NIX_DAEMON_SOCKET_PATH=/run/nix/nix-daemon.socket" > "$BUILDER_HOME/.ssh/environment"
chown "$BUILDER_UID:$BUILDER_UID" "$BUILDER_HOME/.ssh/environment"
chmod 600 "$BUILDER_HOME/.ssh/environment"

# Start nix-daemon with socket in /run (VirtioFS can't handle sockets in /nix)
echo "Starting nix-daemon..."
export NIX_DAEMON_SOCKET_PATH=/run/nix/nix-daemon.socket
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
