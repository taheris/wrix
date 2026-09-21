{ system, nonce }:

builtins.derivation {
  name = "wrix-builder-build-user";
  inherit system nonce;
  builder = "/bin/sh";
  PATH = "/bin:/usr/bin";
  preferLocalBuild = true;
  allowSubstitutes = false;
  args = [
    "-c"
    ''
      set -euo pipefail
      uid=$(id -u)
      user=$(id -un)
      group=$(id -gn)
      if [[ "$uid" == 0 || "$user" != nixbld* || "$group" != nixbld ]]; then
        echo "FAIL: build ran as $user ($uid), group $group, rather than a Nix build user" >&2
        exit 1
      fi

      printf 'original\n' > read-only
      chmod 0444 read-only
      if (printf 'changed\n' >> read-only); then
        echo "FAIL: build user bypassed read-only file permissions" >&2
        exit 1
      fi
      mkdir locked
      chmod 0555 locked
      if touch locked/config.lock; then
        echo "FAIL: build user bypassed read-only directory permissions" >&2
        exit 1
      fi
      printf '%s\n' "$user" > "$out"
    ''
  ];
}
