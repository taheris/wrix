{ pkgs }:

{
  container-runtime-state =
    pkgs.runCommandLocal "test-container-runtime-state"
      {
        nativeBuildInputs = [
          pkgs.bash
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        CONTAINER_UTIL="${../../lib/util/container.sh}" \
          bash "${./container-status.sh}"
        mkdir "$out"
      '';

  known-hosts-installer-skips-existing-ssh-dir =
    pkgs.runCommandLocal "test-known-hosts-installer-skips-existing-ssh-dir" { }
      ''
        set -euo pipefail
        root="$PWD/root"
        known_hosts="$PWD/known_hosts"
        mkdir -p "$root/etc/ssh" "$PWD/bin"
        printf '%s\n' 'github.com ssh-ed25519 test' > "$known_hosts"
        cat > "$PWD/bin/chmod" <<'EOF'
        #!${pkgs.bash}/bin/bash
        set -euo pipefail
        echo "chmod: changing permissions of './etc/ssh': Operation not permitted" >&2
        exit 1
        EOF
        chmod +x "$PWD/bin/chmod"
        export PATH="$PWD/bin:$PATH"
        ${pkgs.bash}/bin/bash ${../../lib/sandbox/install-known-hosts.sh} "$known_hosts" "$root"
        cmp "$known_hosts" "$root/etc/wrix/known_hosts_dir/known_hosts"
        if [[ -e "$root/etc/ssh/ssh_known_hosts" ]]; then
          echo "FAIL: installer mutated an existing /etc/ssh directory" >&2
          exit 1
        fi
        mkdir "$out"
      '';
}
