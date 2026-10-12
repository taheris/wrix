{ pkgs }:

{
  native-test-preparation =
    pkgs.runCommandLocal "test-native-test-preparation"
      {
        nativeBuildInputs = [
          pkgs.bash
          pkgs.python3
        ];
      }
      ''
        set -euo pipefail
        python ${../prek/test_native_fixtures.py} ${../prepare-native-fixtures.sh}
        touch "$out"
      '';

  image-verifier-artifacts =
    pkgs.runCommandLocal "test-image-verifier-artifacts"
      {
        nativeBuildInputs = [ pkgs.python3 ];
      }
      ''
        set -euo pipefail
        python ${../lib}/test_image_artifact.py
        touch "$out"
      '';

  known-hosts-runtime-store-purity = pkgs.runCommandLocal "known-hosts-runtime-store-purity" { } ''
    set -euo pipefail
    root="$PWD/root"
    store="$PWD/store-ssh"
    known_hosts="$PWD/known_hosts"
    mkdir -p "$root/etc" "$store"
    printf '%s\n' 'Host *' > "$store/ssh_config"
    printf '%s\n' 'github.com ssh-ed25519 test' > "$known_hosts"
    chmod 0444 "$store/ssh_config"
    chmod 0555 "$store"
    ln -s "$store" "$root/etc/ssh"
    ${pkgs.bash}/bin/bash ${../../lib/sandbox/install-known-hosts.sh} "$known_hosts" "$root" runtime
    [[ ! -L "$root/etc/ssh" ]]
    [[ ! -e "$store/ssh_known_hosts" ]]
    cmp "$store/ssh_config" "$root/etc/ssh/ssh_config"
    [[ $(stat -c %a "$root/etc/ssh/ssh_config") == 444 ]]
    cmp "$known_hosts" "$root/etc/ssh/ssh_known_hosts"
    ${pkgs.bash}/bin/bash ${../../lib/sandbox/install-known-hosts.sh} "$known_hosts" "$root" runtime
    [[ ! -e "$store/ssh_known_hosts" ]]
    touch "$out"
  '';

  known-hosts-runtime-file-symlink = pkgs.runCommandLocal "known-hosts-runtime-file-symlink" { } ''
    set -euo pipefail
    root="$PWD/root"
    store_file="$PWD/store-known-hosts"
    known_hosts="$PWD/known_hosts"
    mkdir -p "$root/etc/ssh"
    printf '%s\n' 'unchanged store contents' > "$store_file"
    printf '%s\n' 'github.com ssh-ed25519 test' > "$known_hosts"
    cp "$store_file" "$PWD/expected"
    ln -s "$store_file" "$root/etc/ssh/ssh_known_hosts"
    ${pkgs.bash}/bin/bash ${../../lib/sandbox/install-known-hosts.sh} "$known_hosts" "$root" runtime
    [[ ! -L "$root/etc/ssh/ssh_known_hosts" ]]
    cmp "$store_file" "$PWD/expected"
    cmp "$known_hosts" "$root/etc/ssh/ssh_known_hosts"
    touch "$out"
  '';

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
