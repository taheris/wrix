{
  root,
  system,
  target,
}:

let
  inherit (builtins)
    all
    attrNames
    concatStringsSep
    elemAt
    getAttr
    getFlake
    hasAttr
    length
    readFile
    throw
    toString
    ;
  rootString = toString root;
  flake = getFlake "git+file://${rootString}";
  pkgs = flake.inputs.nixpkgs.legacyPackages.${system};
  wlib = flake.legacyPackages.${system}.lib;
  inherit (pkgs) writeShellScriptBin writeText;
  inherit (pkgs.lib) hasInfix splitString toLower;

  ensure = condition: message: if condition then true else throw "verify:${target}: ${message}";
  readRepo = path: readFile "${rootString}/${path}";
  lacks = needle: text: !(hasInfix needle text);
  lacksLower = needle: text: lacks needle (toLower text);
  hasBefore =
    needle: marker: text:
    let
      parts = splitString marker text;
    in
    length parts == 2 && hasInfix needle (elemAt parts 0);

  devshellSource = readRepo "lib/devshell/default.nix";
  flakeDevshellSource = readRepo "modules/flake/devshell.nix";
  entrypointSources = [
    (readRepo "lib/sandbox/linux/entrypoint.sh")
    (readRepo "lib/sandbox/darwin/entrypoint.sh")
  ];

  flakeModuleThinConsumer =
    let
      fakeServiceImage = {
        ref = "wrix-service:test";
        source = writeText "wrix-service-image-source" "source";
        source_kind = "nix-descriptor";
        digest = writeText "wrix-service-image-digest" "sha256:test";
      };
      fakeConfig = {
        packages = {
          wrix-service-image = fakeServiceImage;
          profile-images-pi = writeText "profile-images-pi" "{}";
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
            throw "modules/flake/devshell.nix did not construct the expected rust/pi sandbox";
      };
      result = (import "${rootString}/modules/flake/devshell.nix" { }).perSystem {
        config = fakeConfig;
        inherit pkgs;
        wrix = fakeWrix;
      };
      shell = result.devShells.default;
      inherit (shell) args;
      forbiddenEnv = [
        "CARGO_BUILD_RUSTC_WRAPPER"
        "CARGO_INCREMENTAL"
        "PATH"
        "RUSTC_WRAPPER"
        "SCCACHE_DIR"
        "SCCACHE_CACHE_SIZE"
      ];
    in
    all (name: !(hasAttr name (args.env or { }))) forbiddenEnv
    && (shell.__wrixThinConsumer or false)
    && !(hasAttr "profile" args)
    && !(hasAttr "sandbox" args)
    && !(hasAttr "shellHook" args);

  checks = {
    "devshell.flake-module-does-not-own-hooks-path" =
      ensure (lacks "core.hooksPath" flakeDevshellSource) "modules/flake/devshell.nix sets core.hooksPath";

    "devshell.flake-module-thin-consumer" =
      ensure flakeModuleThinConsumer "modules/flake/devshell.nix bypasses the bound sandbox.devShell surface";

    "devshell.no-prek-install" =
      ensure (lacks "prek install" devshellSource) "mkDevShell invokes prek install"
      && ensure (lacks ".git/hooks" devshellSource) "mkDevShell mutates .git/hooks";

    "devshell.shellhook-order" =
      let
        marker = "WRIX_MKDEVSHELL_CONSUMER_MARKER_XYZ";
        hook =
          (wlib.mkDevShell {
            profile = wlib.profiles.rust;
            shellHook = "echo ${marker}";
          }).shellHook;
        profileHookPrecedesConsumer = hasBefore "RUSTC_WRAPPER" marker hook;
      in
      ensure (hasInfix marker hook) "consumer shellHook marker is absent"
      && ensure profileHookPrecedesConsumer "consumer shellHook does not follow the profile shellHook";

    "profiles.no-dev-toolchain-lib" = ensure (
      !(hasAttr "devToolchain" wlib)
    ) "wrix.devToolchain is still exposed";

    "profiles.no-rust-with-toolchain" = ensure (
      !(hasAttr "withToolchain" wlib.profiles.rust)
    ) "profiles.rust.withToolchain is still exposed";

    "profiles.sandbox-entrypoints-no-rustup" = ensure (all (
      source: lacksLower "rustup" source
    ) entrypointSources) "sandbox entrypoints contain rustup bootstrap logic";
  };
in
if hasAttr target checks then
  if getAttr target checks then "passed" else throw "verify:${target}: failed"
else
  throw "unknown profiles eval target ${target}; known targets: ${concatStringsSep ", " (attrNames checks)}"
