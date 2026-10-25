{ lib }:
let
  protectedPaths = [
    "/nix"
    "/etc/nix"
    "/etc/wrix/private-nix-version"
    "/etc/wrix/shared-nix-version"
    "/homeless-shelter"
    "/network-bootstrap.sh"
    "/network-ready.sh"
    "/shared-nix-client.sh"
    "/run/wrix/nix-pool"
    "/run/wrix/nix-client"
    "/tool-cache.sh"
    "/entrypoint.sh"
    "/krun-init.sh"
    "/krun-relay"
    "/usr/local/libexec/wrix-network"
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
  overlaps = a: b: a == b || lib.hasPrefix "${a}/" b || lib.hasPrefix "${b}/" a;
  validateDestination =
    destination:
    let
      parts = lib.splitString "/" destination;
      normalized = "/" + lib.concatStringsSep "/" (builtins.filter (part: part != "") parts);
    in
    if
      !builtins.isString destination
      || !lib.hasPrefix "/" destination
      || builtins.any (
        part:
        builtins.elem part [
          "."
          ".."
        ]
      ) parts
      || normalized == "/"
      || builtins.any (overlaps normalized) protectedPaths
    then
      throw "sandbox mounts and writableDirs must use absolute paths outside the private Nix and trusted startup paths: ${toString destination}"
    else
      null;
in
{
  version = 1;
  inherit reservedEnv validateDestination;
  validateProfile =
    profile:
    let
      destinations = map (mount: mount.dest) (profile.mounts or [ ]) ++ (profile.writableDirs or [ ]);
    in
    builtins.deepSeq (map validateDestination destinations) profile;
}
