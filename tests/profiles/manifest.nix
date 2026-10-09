{ pkgs, wrix }:

let
  inherit (builtins)
    all
    attrNames
    deepSeq
    hasContext
    removeAttrs
    toJSON
    tryEval
    ;
  agents = [
    "direct"
    "claude"
    "pi"
  ];
  fields = [
    "launcher"
    "profile_config"
    "ref"
    "source"
    "source_kind"
  ];

  entryMatchesSandbox =
    agent:
    let
      sandbox = wrix.mkSandbox {
        profile = wrix.profiles.base;
        inherit agent;
        agentPkg = if agent == "direct" then pkgs.hello else null;
      };
      manifest = wrix.mkProfileImages { base = sandbox.image; };
      entry = manifest.passthru.manifest.base.${agent};
    in
    attrNames manifest.passthru.manifest.base == [ agent ]
    && attrNames entry == fields
    && entry.launcher == "${sandbox.launcher}/bin/wrix"
    && entry.launcher != "${sandbox.package}/bin/wrix"
    && entry.profile_config == toString sandbox.profileConfig
    && entry.source == toString sandbox.image.source
    && entry.source_kind == sandbox.image.source_kind
    && all hasContext [
      entry.launcher
      entry.profile_config
      entry.source
      (toJSON manifest.passthru.manifest)
    ];

  imageWithoutConfig = removeAttrs (wrix.mkSandbox { }).image [ "profileConfig" ];
  missingConfig = tryEval (
    deepSeq (wrix.mkProfileImages { base = imageWithoutConfig; }).passthru.manifest true
  );
in
assert all entryMatchesSandbox agents;
assert !missingConfig.success;
pkgs.runCommand "test-profile-images-launcher" { } ''
  set -euo pipefail
  touch "$out"
''
