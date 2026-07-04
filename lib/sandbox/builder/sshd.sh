#!/usr/bin/env bash
set -euo pipefail

wrix_builder_write_sshd_config() {
  local builder_user="$1"
  local output_path="$2"

  cat > "$output_path" <<EOF
Port 22
ListenAddress 127.0.0.1
HostKey /etc/ssh/ssh_host_ed25519_key
AuthorizedKeysFile /home/%u/.ssh/authorized_keys
PasswordAuthentication no
PermitRootLogin no
AllowUsers $builder_user
PermitUserEnvironment yes
Subsystem sftp internal-sftp
EOF
}
