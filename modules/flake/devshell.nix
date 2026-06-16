_:

{
  perSystem =
    {
      config,
      pkgs,
      system,
      wrix,
      ...
    }:
    let
      isDarwin = builtins.elem system [
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      serviceImageTag = builtins.substring 0 8 (
        builtins.hashString "sha256" (
          builtins.unsafeDiscardStringContext (toString config.packages.wrix-service-image)
        )
      );
      serviceImagePrefix = if isDarwin then "" else "localhost/";
    in
    {
      devShells.default = wrix.mkDevShell {
        profile = wrix.profiles.rust;

        env = {
          LOOM_PROFILES_MANIFEST = "${config.packages.profile-images-pi}";
          WRIX_AGENT = "pi";
          WRIX_SERVICE_IMAGE = "${serviceImagePrefix}wrix-service:${serviceImageTag}";
          WRIX_SERVICE_IMAGE_SOURCE = "${config.packages.wrix-service-image}";
        };

        packages = [
          config.packages.wrix
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.podman
          pkgs.skopeo
        ];
      };
    };
}
