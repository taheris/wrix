{
  lib,
  filterCargoSources,
  root,
}:
let
  inherit (lib) hasPrefix removePrefix;
  production = import ./rust-source-filter.nix { inherit lib filterCargoSources root; };
  serverOnly = [
    "crates/wrix-service-server/src"
    "crates/wrix-service/src/bin"
    "crates/wrix-service/src/control/server.rs"
    "crates/wrix-service/src/control/gateway"
    "crates/wrix-service/src/control/serving"
    "crates/wrix-service/src/gateway/server.rs"
    "crates/wrix-service/src/gateway/bootstrap"
    "crates/wrix-service/src/gateway/traffic"
    "crates/wrix-service/src/gateway/transport"
    "crates/wrix-service/src/gateway/test"
    "crates/wrix-service/src/gateway/management/server.rs"
    "crates/wrix-service/src/route/server.rs"
    "crates/wrix-service/src/route/guest_test.rs"
    "crates/wrix-service/src/lifecycle/compiler/server.rs"
    "crates/wrix-service/src/lifecycle/compiler/startup"
  ];
in
path: type:
let
  relative = removePrefix "${toString root}/" path;
in
production path type
&& !(builtins.any (prefix: relative == prefix || hasPrefix "${prefix}/" relative) serverOnly)
