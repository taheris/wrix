{
  root,
  system,
  target,
  describe ? false,
}:

let
  inherit (builtins)
    all
    attrNames
    concatLists
    concatStringsSep
    getAttr
    mapAttrs
    getFlake
    hasAttr
    throw
    toString
    ;
  rootString = toString root;
  flake = getFlake "git+file://${rootString}";
  pkgs = flake.inputs.nixpkgs.legacyPackages.${system};
  wlib = flake.legacyPackages.${system}.lib;
  inherit (pkgs) writeShellScriptBin writeText;
  inherit (pkgs.lib) hasInfix toLower;

  ensure = condition: message: if condition then true else throw "verify:${target}: ${message}";
  inputDefinition = import ../lib/inputs.nix { inherit root; };
  readRepo = inputDefinition.read;
  sourceCheck = sources: check: {
    inputs = inputDefinition.nix (concatLists (map (source: source.inputs) sources));
    passed = check (map (source: source.text) sources);
  };
  opaque = passed: {
    inherit passed;
    inputs = null;
  };
  lacks = needle: text: !(hasInfix needle text);
  lacksLower = needle: text: lacks needle (toLower text);
  devshellSource = readRepo "lib/devshell/default.nix";
  flakeDevshellSource = readRepo "modules/flake/devshell.nix";
  entrypointSources = [
    (readRepo "lib/sandbox/linux/entrypoint.sh")
    (readRepo "lib/sandbox/darwin/entrypoint.sh")
  ];

  repositoryDevshellUsesSandbox =
    let
      fakeConfig = {
        packages = {
          profile-images-pi = writeText "profile-images-pi" "{}";
          loom = writeShellScriptBin "loom" "exit 0";
        };
        treefmt.build.wrapper = writeShellScriptBin "treefmt" "exit 0";
      };
      fakeRustProfile = {
        name = "fake-rust-profile";
      };
      fakeSandbox = {
        package = writeShellScriptBin "wrix" "exit 0";
        profile = fakeRustProfile;
        devShell = args: {
          __wrixThinConsumer = true;
          inherit args;
        };
      };
      fakeWrix = {
        profiles.rust = fakeRustProfile;
        mkSandbox =
          args:
          if args.profile == fakeRustProfile && args.agent == "pi" then
            fakeSandbox
          else
            throw "repository devshell did not construct the expected rust/pi sandbox";
      };
      result = (import "${rootString}/modules/flake/devshell.nix" { }).perSystem {
        config = fakeConfig;
        inherit pkgs;
        linuxLoomCli = fakeConfig.packages.loom;
        wrix = fakeWrix;
      };
      shell = result.devShells.default;
      inherit (shell) args;
      forbiddenEnv = [
        "CARGO_BUILD_RUSTC_WRAPPER"
        "CARGO_INCREMENTAL"
        "PATH"
        "RUSTC"
        "RUSTC_WRAPPER"
        "SCCACHE_DIR"
        "SCCACHE_CACHE_SIZE"
        "WRIX_SERVICE_IMAGE"
        "WRIX_SERVICE_IMAGE_DIGEST"
        "WRIX_SERVICE_IMAGE_SOURCE"
        "WRIX_SERVICE_IMAGE_SOURCE_KIND"
      ];
    in
    all (name: !(hasAttr name (args.env or { }))) forbiddenEnv
    && (shell.__wrixThinConsumer or false)
    && !(hasAttr "profile" args)
    && !(hasAttr "sandbox" args)
    && !(hasAttr "shellHook" args);

  checks = {
    "devshell.flake-module-does-not-own-hooks-path" = sourceCheck [ flakeDevshellSource ] (
      sources:
      ensure (lacks "core.hooksPath" (builtins.head sources)) "modules/flake/devshell.nix sets core.hooksPath"
    );

    "devshell.sandbox-boundary" = opaque (
      ensure repositoryDevshellUsesSandbox "repository devshell bypasses the bound sandbox.devShell surface"
    );

    "devshell.no-prek-install" = sourceCheck [ devshellSource ] (
      sources:
      let
        source = builtins.head sources;
      in
      ensure (lacks "prek install" source) "mkDevShell invokes prek install"
      && ensure (lacks ".git/hooks" source) "mkDevShell mutates .git/hooks"
    );

    "profiles.beads-metrics-disabled" = opaque (
      ensure (all
        (
          profile:
          let
            sandbox = wlib.mkSandbox { inherit profile; };
          in
          profile.env.BD_DISABLE_METRICS == "1"
          && profile.hostEnv.BD_DISABLE_METRICS == "1"
          && (wlib.mkDevShell { inherit profile; }).BD_DISABLE_METRICS == "1"
          && sandbox.profile.env.BD_DISABLE_METRICS == "1"
          && (sandbox.devShell { }).BD_DISABLE_METRICS == "1"
        )
        [
          wlib.profiles.base
          wlib.profiles.rust
          wlib.profiles.python
        ]
      ) "profiles, sandboxes, and devshells must disable Beads metrics"
    );

    "profiles.no-dev-toolchain-lib" = opaque (
      ensure (!(hasAttr "devToolchain" wlib)) "wrix.devToolchain is still exposed"
    );

    "profiles.no-rust-with-toolchain" = opaque (
      ensure (
        !(hasAttr "withToolchain" wlib.profiles.rust)
      ) "profiles.rust.withToolchain is still exposed"
    );

    "profiles.sandbox-entrypoints-no-rustup" = sourceCheck entrypointSources (
      sources:
      ensure (all (
        source: lacksLower "rustup" source
      ) sources) "sandbox entrypoints contain rustup bootstrap logic"
    );
  };
in
if describe then
  mapAttrs (_: check: check.inputs) checks
else if hasAttr target checks then
  if (getAttr target checks).passed then "passed" else throw "verify:${target}: failed"
else
  throw "unknown profiles eval target ${target}; known targets: ${concatStringsSep ", " (attrNames checks)}"
