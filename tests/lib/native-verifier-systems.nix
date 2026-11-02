{ pkgs }:
let
  targets = [
    "oci-layout-builder"
    "private-nix-api"
    "private-nix-state-preservation"
    "known-hosts-runtime-store-purity"
    "known-hosts-runtime-file-symlink"
  ];
  systems = [
    "aarch64-darwin"
    "aarch64-linux"
    "x86_64-linux"
  ];
  cases = builtins.concatMap (
    system:
    let
      registry = import ../verify/sandbox.nix { inherit pkgs system; };
    in
    map (target: {
      inherit system target;
      inherit (registry."sandbox.${target}") script;
    }) targets
  ) systems;
  definitions = pkgs.writeText "native-verifier-systems.json" (builtins.toJSON cases);
  nix = pkgs.writeScriptBin "nix" ''
    #!${pkgs.python3}/bin/python3
    import json, sys
    print(json.dumps(sys.argv[1:]))
  '';
in
pkgs.runCommand "native-verifier-systems" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  export PATH="${nix}/bin:$PATH"
  python - ${definitions} <<'PY'
  import json, pathlib, subprocess, sys
  for case in json.loads(pathlib.Path(sys.argv[1]).read_text()):
      result = subprocess.run(["${pkgs.bash}/bin/bash", "-euc", case["script"]], check=True, capture_output=True, text=True)
      assert json.loads(result.stdout) == ["build", "--no-link", f'.#checks.{case["system"]}.{case["target"]}'], result.stdout
  PY
  touch "$out"
''
