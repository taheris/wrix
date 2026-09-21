{ pkgs }:

let
  image = import ../../lib/sandbox/builder/image.nix { inherit pkgs; };
  accountRoot =
    name: builtins.head (builtins.filter (root: root.name == name) image.darwin_seed_roots);
in
pkgs.runCommandLocal "test-builder-build-users"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.nix
      pkgs.python3
    ];
  }
  ''
    set -euo pipefail
    mkdir -p "$TMPDIR/config"
    source ${../../lib/sandbox/builder/nix-daemon.sh}
    wrix_builder_write_nix_config builder "$TMPDIR/config/nix.conf"
    export NIX_CONF_DIR="$TMPDIR/config"
    export NIX_USER_CONF_FILES=/dev/null
    export NIX_CONFIG=""
    group=$(nix config show build-users-group)
    if [[ -z "$group" ]]; then
      echo "FAIL: builder Nix config runs builds as the root daemon rather than build users" >&2
      exit 1
    fi
    [[ "$(nix config show sandbox)" == false ]]
    trusted=$(nix config show trusted-users)
    python3 - '${accountRoot "passwd"}/etc/passwd' '${accountRoot "group"}/etc/group' "$group" "$trusted" <<'PY'
    import pathlib
    import sys

    passwd_path, group_path, configured_group, trusted = sys.argv[1:]
    accounts = [line.split(":") for line in pathlib.Path(passwd_path).read_text().splitlines()]
    groups = [line.split(":") for line in pathlib.Path(group_path).read_text().splitlines()]
    assert len({row[0] for row in accounts}) == len(accounts), "duplicate user names"
    assert len({row[2] for row in accounts}) == len(accounts), "duplicate UIDs"
    assert len({row[0] for row in groups}) == len(groups), "duplicate group names"
    assert len({row[2] for row in groups}) == len(groups), "duplicate GIDs"
    matching = [row for row in groups if row[0] == configured_group]
    assert len(matching) == 1, "configured build group missing from image"
    group = matching[0]
    members = group[3].split(",")
    assert len(members) == len(set(members)) == 32, "expected 32 distinct build users"
    assert not {"root", "builder", "sshd", "nobody"}.intersection(members), "service account in build pool"
    assert set(trusted.split()) == {"root", "builder"}, "build users must not be trusted Nix clients"
    users = {row[0]: row for row in accounts}
    for member in members:
        user = users[member]
        assert 30001 <= int(user[2]) <= 30032, "build UID outside dedicated range"
        assert user[3] == group[2] == "30000", "build user's primary group is inconsistent"
        assert user[5:] == ["/var/empty", "/bin/false"], "build account allows interactive login"
    print("PASS: builder config selects the image's unprivileged build-user pool")
    PY
    touch "$out"
  ''
