# Generate SSH keys for wrix-builder
#
# Creates key pairs in the nix store so that:
# 1. Keys are stable across rebuilds (same derivation = same store path)
# 2. publicHostKey is available at nix-darwin eval time
# 3. Client key is accessible to root (nix-daemon) for remote builds
# 4. wrix-builder can reference keys directly from the store
#
# Security note: Keys are generated without passphrases for automated use.
# Keys in /nix/store are world-readable, but this is mitigated by:
# - SSH port bound to localhost only (not network-accessible)
# - Password authentication disabled
# - Keys only grant access to local builder (no external systems)
# See specs/linux-builder.md for trust model.
#
{ pkgs }:

pkgs.runCommand "wrix-builder-keys"
  {
    nativeBuildInputs = [ pkgs.openssh ];
  }
  ''
    mkdir -p $out

    # Generate host key (for server identity)
    ssh-keygen -t ed25519 -f $out/ssh_host_ed25519_key -N "" -C "wrix-builder-host" </dev/null

    # Generate client key (for SSH authentication to builder)
    ssh-keygen -t ed25519 -f $out/builder_ed25519 -N "" -C "wrix-builder-client" </dev/null

    # Create base64-encoded public host key for nix-darwin buildMachines
    base64 < $out/ssh_host_ed25519_key.pub | tr -d '\n' > $out/public_host_key_base64
  ''
