{
  pkgs,
  linuxPkgs,
  wrix,
  nativeSandbox,
}:
let
  privatePolicy = {
    kind = "private-native-nix";
    version = 1;
  };
  defaultPolicy = {
    kind = "default";
    version = 1;
  };
  plain = wrix.mkSandbox { profile = wrix.profiles.base; };
  explicitDefault = wrix.mkSandbox {
    profile = wrix.profiles.base;
    imagePolicy = defaultPolicy;
  };
  native = wrix.mkSandbox {
    profile = wrix.profiles.base;
    imagePolicy = privatePolicy;
  };
  rejected = policy: !(builtins.tryEval (wrix.mkSandbox { imagePolicy = policy; })).success;
  mkArchive =
    imagePolicy:
    import ../../lib/sandbox/image.nix {
      pkgs = linuxPkgs;
      hostPkgs = pkgs;
      profile = wrix.profiles.base;
      entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
      claudeConfig = { };
      claudeSettings = { };
      agent = "pi";
      agentPkg = linuxPkgs.hello;
      asTarball = true;
      inherit imagePolicy;
    };
  archive = mkArchive privatePolicy;
  legacyArchive = mkArchive defaultPolicy;
in
{
  image-policy-api =
    assert builtins.elem 1 wrix.capabilities.imagePolicies.private-native-nix;
    assert plain.image.drvPath == explicitDefault.image.drvPath;
    assert plain.image.source.drvPath == explicitDefault.image.source.drvPath;
    assert plain.profileConfig.drvPath == explicitDefault.profileConfig.drvPath;
    assert native.image.imagePolicy == privatePolicy;
    assert native.image.drvPath != plain.image.drvPath;
    assert native.image.source.drvPath != plain.image.source.drvPath;
    assert native.image.digest != plain.image.digest;
    assert native.profileConfig.drvPath != plain.profileConfig.drvPath;
    assert native.image.baseImage.drvPath == plain.image.baseImage.drvPath;
    assert native.image.stableProfileImage.drvPath == plain.image.stableProfileImage.drvPath;
    assert archive.source_kind == "docker-archive";
    assert archive.source.drvPath != legacyArchive.source.drvPath;
    assert archive.digest.drvPath != legacyArchive.digest.drvPath;
    assert rejected {
      kind = "unknown";
      version = 1;
    };
    assert rejected {
      kind = "private-native-nix";
      version = 2;
    };
    assert rejected {
      kind = "private-native-nix";
      version = "1";
    };
    assert rejected {
      kind = "private-native-nix";
      version = 1.0;
    };
    assert rejected {
      kind = "private-native-nix";
      version = 1;
      typo = true;
    };
    assert rejected { kind = "private-native-nix"; };
    assert rejected "private-native-nix";
    pkgs.runCommand "test-image-policy-api" { } ''touch "$out"'';

  image-policy-state-preservation =
    pkgs.runCommand "test-image-policy-state-preservation"
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

  image-policy-artifacts =
    pkgs.runCommand "test-image-policy-artifacts"
      {
        nativeBuildInputs = [ pkgs.python3 ];
      }
      ''
        set -euo pipefail
        python ${./native-nix-permissions.py} ${nativeSandbox.image.ociLayout}
        python ${./native-nix-permissions.py} --archive ${archive.source}
        python ${./native-nix-permissions.py} --archive --legacy ${legacyArchive.source}
        [[ "$(cat ${archive.digest})" != "$(cat ${legacyArchive.digest})" ]]
        python - ${native.profileConfig} ${plain.profileConfig} ${nativeSandbox.profileConfig} ${nativeSandbox.image.digest} <<'PY'
        import json
        import pathlib
        import sys
        native, default, factory = [json.loads(pathlib.Path(p).read_text()) for p in sys.argv[1:4]]
        for field in ("source", "digest", "ref"):
            assert native["image"][field] != default["image"][field], field
        assert factory["image"]["digest"] == pathlib.Path(sys.argv[4]).read_text().strip()
        assert factory["agent"]["kind"] == "pi"
        PY
        touch "$out"
      '';
}
