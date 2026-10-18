{
  pkgs,
  linuxPkgs,
  crane,
  fenix,
  treefmt,
  serviceCli,
  rustNextest,
}:
let
  inherit (pkgs.lib) concatMap;
  cases =
    concatMap
      (
        system:
        let
          sandbox = import ../../lib/sandbox {
            inherit
              pkgs
              linuxPkgs
              crane
              fenix
              treefmt
              serviceCli
              system
              ;
          };
        in
        map
          (
            name:
            let
              inherit ((sandbox.mkSandbox { profile = sandbox.profiles.${name}; })) image;
            in
            {
              inherit system name;
              path = image.profileConfig;
            }
          )
          [
            "base"
            "rust"
            "python"
          ]
      )
      [
        "x86_64-linux"
        "aarch64-darwin"
      ];
  manifest = pkgs.writeText "builtin-profile-configs.json" (builtins.toJSON cases);
in
rustNextest.overrideAttrs {
  pname = "profile-config-parser-agreement";
  WRIX_TEST_PROFILE_CONFIGS = manifest;
  checkPhase = ''
    runHook preCheck
    cargo test -p wrix-sandbox --test config_contract nix_generated_builtin_profiles_match_parser -- --ignored --exact
    runHook postCheck
  '';
}
