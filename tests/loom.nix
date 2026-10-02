{
  pkgs,
  loomCli,
  linuxLoomCli,
  packages,
  devShell,
  lock,
}:

let
  inherit (builtins) all attrNames elem;
  inherit (pkgs.lib) concatMapStrings hasPrefix optionalString;
  sourceNode = lock.nodes.${lock.nodes.root.inputs.loom-src};
  sandboxNames = builtins.filter (hasPrefix "sandbox") (attrNames packages) ++ [
    "default"
    "debug"
  ];
  environmentFor = name: packages.${name}.image.profileEnv;
  hostTools = devShell.nativeBuildInputs ++ devShell.buildInputs;

  sourceOnly =
    assert sourceNode.flake == false;
    assert !(sourceNode ? inputs);
    assert sourceNode.original.ref == "main";
    assert sourceNode.locked ? rev;
    pkgs.runCommand "test-loom-source-only" { } ''
      set -euo pipefail
      touch "$out"
    '';

  wiring =
    assert loomCli.system == pkgs.stdenv.hostPlatform.system;
    assert linuxLoomCli.stdenv.hostPlatform.isLinux;
    assert toString loomCli.src == toString linuxLoomCli.src;
    assert elem loomCli hostTools;
    assert all (name: elem linuxLoomCli (environmentFor name).paths) sandboxNames;
    pkgs.runCommand "test-loom-package-wiring" { } ''
      set -euo pipefail
      touch "$out"
    '';

  devshellCli =
    assert elem loomCli hostTools;
    pkgs.runCommand "test-loom-devshell" { nativeBuildInputs = [ loomCli ]; } ''
      set -euo pipefail
      [[ "$(command -v loom)" == "${loomCli}/bin/loom" ]]
      [[ ! -e "${loomCli}/bin/loom-direct-runner" ]]
      [[ ! -e "${loomCli}/bin/mock-loom-agent" ]]
      [[ ! -e "${loomCli}/bin/bd-shim" ]]
      loom gate verify --help > "$out"
    '';

  agentImages =
    assert all (name: elem linuxLoomCli (environmentFor name).paths) sandboxNames;
    pkgs.runCommand "test-loom-agent-images" { } (
      ''
        set -euo pipefail
        mkdir -p "$out"
      ''
      + optionalString pkgs.stdenv.hostPlatform.isLinux (
        concatMapStrings (name: ''
          PATH="${environmentFor name}/bin" loom gate verify --help > "$out/${name}"
        '') sandboxNames
      )
    );

  ciApp = name: check: {
    inherit name;
    executable = name;
    package = pkgs.writeShellScriptBin name ''
      set -euo pipefail
      [[ -e "${check}" ]]
    '';
  };
in
{
  checks = {
    loom-source-only = sourceOnly;
    loom-package-wiring = wiring;
    loom-devshell = devshellCli;
  };
  ciApps = [
    (ciApp "test-loom-devshell" devshellCli)
    (ciApp "test-loom-agent-images" agentImages)
  ];
}
