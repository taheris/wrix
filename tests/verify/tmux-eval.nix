{
  root,
  system,
}:
let
  inherit (builtins)
    all
    attrNames
    deepSeq
    elem
    fromTOML
    getFlake
    pathExists
    readFile
    tryEval
    ;
  flake = getFlake "git+file://${toString root}";
  inherit (flake.legacyPackages.${system}) lib ciChecks;
  pkgs = flake.inputs.nixpkgs.legacyPackages.${system};
  inherit (pkgs.lib) hasInfix;
  linuxSystem = if pkgs.stdenv.hostPlatform.isDarwin then "aarch64-linux" else system;
  imagePkgs = flake.inputs.nixpkgs.legacyPackages.${linuxSystem};
  registry = import (root + "/lib/mcp") { inherit pkgs; };
  rejected =
    options: !(tryEval (deepSeq (lib.mkSandbox { mcp.tmux = options; }).profile true)).success;
  cargo = fromTOML (readFile (root + "/Cargo.toml"));
  lock = fromTOML (readFile (root + "/Cargo.lock"));
  nativeCargo =
    manifest: resolved:
    !(elem "lib/mcp/tmux/tmux-mcp" manifest.workspace.members)
    && !(manifest.workspace.dependencies ? signal-hook)
    && !(manifest.workspace.dependencies ? signal-hook-registry)
    && all (
      package:
      !(elem package.name [
        "tmux-mcp"
        "signal-hook"
      ])
    ) resolved.package;
  fixture = dependencies: {
    workspace = {
      members = [ ];
      inherit dependencies;
    };
  };
  transitiveRegistry.package = [ { name = "signal-hook-registry"; } ];
  verifiers = import ./default.nix {
    inherit pkgs system;
    focusBinary = lib.rustPackage.focusNotification;
    linuxPkgs = imagePkgs;
  };
  sources = map (path: readFile (root + "/${path}")) [
    "lib/default.nix"
    "lib/mcp/default.nix"
    "lib/sandbox/default.nix"
    "lib/sandbox/image.nix"
    "lib/sandbox/mcp-manifest.sh"
    "lib/sandbox/linux/entrypoint.sh"
    "lib/sandbox/darwin/entrypoint.sh"
    "crates/wrix-sandbox/src/command/launch.rs"
    "modules/flake/packages.nix"
    "tests/default.nix"
  ];
in
assert !(lib ? tmuxMcpPackage);
assert !(flake.packages.${system} ? tmux-mcp);
assert !(ciChecks ? tmux-mcp-clippy) && !(ciChecks ? tmux-mcp-nextest);
assert attrNames registry == [ "playwright" ];
assert
  rejected { }
  && rejected {
    audit = "/workspace/debug.log";
    auditFull = true;
  };
assert elem imagePkgs.tmux lib.profiles.base.packages;
assert nativeCargo cargo lock;
assert nativeCargo (fixture { }) transitiveRegistry;
assert !(nativeCargo (fixture { signal-hook-registry = "1"; }) transitiveRegistry);
assert !(nativeCargo (fixture { signal-hook = "1"; }) transitiveRegistry);
assert !(nativeCargo (fixture { }) { package = [ { name = "tmux-mcp"; } ]; });
assert !(nativeCargo (fixture { }) { package = [ { name = "signal-hook"; } ]; });
assert
  !(nativeCargo {
    workspace = {
      members = [ "lib/mcp/tmux/tmux-mcp" ];
      dependencies = { };
    };
  } transitiveRegistry);
assert all (path: !(pathExists (root + "/${path}"))) [
  "lib/mcp/tmux"
  "lib/sandbox/pi-mcp-extension.ts"
  "tests/mcp/tmux"
  "tests/verify/tmux-mcp.nix"
];
assert all (
  source:
  all (symbol: !(hasInfix symbol source)) [
    "tmuxMcp"
    "tmux-mcp"
    "WRIX_MCP_TMUX_"
  ]
) sources;
assert all (
  target:
  !(hasInfix "tmux-mcp." target) && target != "verify:profiles.rust-build-package-consumer-boundary"
) verifiers.targets;
"PASS: native tmux remains; the retired MCP surface is absent and rejected"
