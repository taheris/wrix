{
  root,
  system,
}:

let
  inherit (builtins)
    attrNames
    filter
    getFlake
    listToAttrs
    match
    ;
  flake = getFlake "git+file://${toString root}";
  wlib = flake.legacyPackages.${system}.lib;
  profile = wlib.profiles.base;
  shell = nixCache: wlib.mkDevShell { inherit profile nixCache; };
  cacheEnv =
    derivation:
    listToAttrs (
      map (name: {
        inherit name;
        value = derivation.${name};
      }) (filter (name: match "WRIX_CACHE_.*|WRIX_NIX_CACHE_.*" name != null) (attrNames derivation))
    );
  ensure = condition: message: if condition then true else throw "cache overrides: ${message}";
in
ensure (cacheEnv (shell true) == { }) "default shell exports implicit cache defaults"
&& ensure (cacheEnv (shell { }) == { }) "empty attrset exports implicit cache defaults"
&& ensure (
  cacheEnv (shell false) == { WRIX_NIX_CACHE_ENABLE = "0"; }
) "cache opt-out is not explicit"
&& ensure (
  cacheEnv (shell {
    warm.checks = true;
  }) == {
    WRIX_CACHE_WARM_CHECKS = "1";
  }
) "partial override fills in unrelated defaults"
&& ensure (
  cacheEnv (shell {
    warm.checks = false;
    warm.includeRoots = [ ];
    warnSize = "50G";
  }) == {
    WRIX_CACHE_WARM_CHECKS = "0";
    WRIX_CACHE_WARM_INCLUDE = "";
    WRIX_CACHE_SOFT_LIMIT_BYTES = "53687091200";
  }
) "explicit built-in values or empty-list resets are lost"
&& ensure (
  cacheEnv (shell {
    requireTrustedNix = false;
    publish.excludeRoots = [
      ".#one"
      ".#two"
    ];
    pendingTtl = "2d";
    pruneInterval = "3h";
  }) == {
    WRIX_NIX_CACHE_REQUIRE_TRUSTED = "0";
    WRIX_CACHE_PUBLISH_EXCLUDE = ".#one\n.#two";
    WRIX_CACHE_PENDING_RETENTION_SECS = "172800";
    WRIX_CACHE_PRUNE_INTERVAL_SECS = "10800";
  }
) "explicit overrides are not normalized for the runtime"
