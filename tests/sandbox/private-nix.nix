{
  pkgs,
  linuxPkgs,
  wrix,
  profileSandboxes,
}:
let
  sandbox = wrix.mkSandbox { profile = wrix.profiles.base; };
  factoryImages = map (
    name:
    let
      selected = profileSandboxes.${name}.pi;
    in
    {
      inherit name;
      layout = toString selected.image.ociLayout;
      config = toString selected.profileConfig;
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
    "/homeless-shelter"
    "/network-bootstrap.sh"
    "/network-ready.sh"
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

  private-nix-state-preservation =
    pkgs.runCommand "test-private-nix-state-preservation"
      {
        nativeBuildInputs = [
          pkgs.python3
          pkgs.fakeroot
          pkgs.bash
          pkgs.coreutils
        ];
      }
      ''
        set -euo pipefail
        fakeroot python ${./test_private_nix_initializer.py} ${../../lib/sandbox/initialize-private-nix.sh}
        touch "$out"
      '';

  private-nix-artifacts =
    pkgs.runCommand "test-private-nix-artifacts"
      {
        nativeBuildInputs = [ pkgs.python3 ];
      }
      ''
        set -euo pipefail
        python ${./native-nix-permissions.py} --archive ${archive.source}
        python - ${factoryImagesFile} ${./native-nix-permissions.py} <<'PY'
        import json
        import pathlib
        import subprocess
        import sys
        for image in json.loads(pathlib.Path(sys.argv[1]).read_text()):
            subprocess.run([sys.executable, sys.argv[2], image["layout"]], check=True)
            config = json.loads(pathlib.Path(image["config"]).read_text())
            digest = pathlib.Path(image["digestFile"]).read_text().strip()
            assert config["image"]["digest"] == digest
            assert config["image"]["source"] == image["source"]
            assert config["profile"]["name"] == image["name"]
            assert config["agent"]["kind"] == "pi"
            if config["image"]["source_kind"] == "nix-descriptor":
                descriptor = json.loads(pathlib.Path(image["source"]).read_text())
                assert descriptor["digest"] == digest
                assert descriptor["config"]["labels"]["wrix.nix.private"] == "1"
        PY
        touch "$out"
      '';
}
