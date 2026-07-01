_:

{
  perSystem =
    {
      config,
      pkgs,
      wrix,
      ...
    }:
    let
      serviceImage = config.packages.wrix-service-image;
      sandbox = wrix.mkSandbox {
        profile = wrix.profiles.rust;
        agent = "pi";
      };
    in
    {
      devShells.default = sandbox.devShell {
        env = {
          LOOM_PROFILES_MANIFEST = "${config.packages.profile-images-pi}";
          WRIX_AGENT = "pi";
          WRIX_SERVICE_IMAGE = serviceImage.ref;
          WRIX_SERVICE_IMAGE_SOURCE = "${serviceImage.source}";
          WRIX_SERVICE_IMAGE_SOURCE_KIND = serviceImage.source_kind;
          WRIX_SERVICE_IMAGE_DIGEST = "${serviceImage.digest}";
        };

        packages = [
          config.treefmt.build.wrapper
          pkgs.flock
          pkgs.podman
          pkgs.skopeo
        ];
      };
    };
}
