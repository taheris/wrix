{
  pkgs,
  linuxPkgs,
  wrix,
  profileSandboxes,
}:
let
  verifyTools = import ../../lib/verify/tools.nix { inherit pkgs; };
  sandbox = wrix.mkSandbox { profile = wrix.profiles.base; };
  factoryImages = map (
    name:
    let
      selected = profileSandboxes.${name}.pi;
    in
    {
      inherit name;
      layout =
        if selected.image.source_kind == "docker-archive" then null else toString selected.image.ociLayout;
      sourceKind = selected.image.source_kind;
      config = toString selected.image.profileConfig;
      source = toString selected.image.source;
      digestFile = toString selected.image.digest;
    }
  ) (builtins.attrNames profileSandboxes);
  factoryImagesFile = pkgs.writeText "private-nix-images.json" (builtins.toJSON factoryImages);
  rejects = args: !(builtins.tryEval (wrix.mkSandbox args)).success;
  protectedPaths = [
    "/"
    "/nix"
    "//nix/store"
    "/nix/var/nix"
    "/etc"
    "/etc/nix"
    "/etc/wrix"
    "/etc/wrix/private-nix-version"
    "/etc/wrix/shared-nix-version"
    "/homeless-shelter"
    "/network-bootstrap.sh"
    "/network-ready.sh"
    "/shared-nix-client.sh"
    "/run/wrix/nix-pool"
    "/run/wrix/nix-client/daemon.sock"
    "/tool-cache.sh"
    "/entrypoint.sh"
    "/krun-init.sh"
    "/krun-relay"
    "/usr/local/libexec/wrix-network"
    "/tmp/../nix"
    "relative"
  ];
  reservedEnv = [
    "IS_SANDBOX"
    "NIX_CONFIG"
    "NIX_CONF_DIR"
    "NIX_DAEMON_SOCKET_PATH"
    "NIX_LOG_DIR"
    "NIX_REMOTE"
    "NIX_STATE_DIR"
    "NIX_STORE_DIR"
    "NIX_USER_CONF_FILES"
  ];
  archive = import ../../lib/sandbox/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    profile = wrix.profiles.base;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
    agent = "pi";
    agentPkg = linuxPkgs.hello;
    asTarball = true;
  };
in
{
  private-nix-api =
    assert wrix.capabilities.privateNix == 1;
    assert !(wrix.capabilities ? imagePolicies);
    assert !(wrix ? mkNativeSandbox);
    assert !(sandbox.image ? imagePolicy);
    assert sandbox.image.labels."wrix.nix.private" == "1";
    assert builtins.all (name: profileSandboxes.${name}.pi.image.labels."wrix.nix.private" == "1") (
      builtins.attrNames profileSandboxes
    );
    assert archive.labels."wrix.nix.private" == "1";
    assert archive.source_kind == "docker-archive";
    assert builtins.all (
      destination:
      rejects {
        mounts = [
          {
            source = "/fixture";
            dest = destination;
          }
        ];
      }
    ) protectedPaths;
    assert builtins.all (
      destination:
      rejects {
        profile = wrix.profiles.base // {
          writableDirs = [ destination ];
        };
      }
    ) protectedPaths;
    assert builtins.all (name: rejects { env.${name} = "override"; }) reservedEnv;
    assert builtins.all (
      name:
      rejects {
        profile = wrix.profiles.base // {
          hostEnv.${name} = "override";
        };
      }
    ) reservedEnv;
    assert builtins.all (name: rejects { runtimeSecrets.${name} = "optional"; }) reservedEnv;
    assert builtins.all (name: rejects { agentSettings.env.${name} = "override"; }) reservedEnv;
    pkgs.runCommand "test-private-nix-api" { } ''touch "$out"'';

  private-nix-state-preservation = pkgs.runCommand "test-private-nix-state-preservation" { } ''
    set -euo pipefail
    test -e ${import ./private-nix-initializer.nix { inherit pkgs; }}
    touch "$out"
  '';

  private-nix-image-roots = import ./image-gc-roots.nix { inherit pkgs; };

  private-nix-artifacts = pkgs.runCommand "test-private-nix-artifacts" { } ''
    set -euo pipefail
    ${pkgs.lib.getExe verifyTools} private-nix-artifacts ${factoryImagesFile} ${archive.source}
    touch "$out"
  '';
}
