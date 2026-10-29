# Pinned upstream recipe from nixpkgs f20cd90e28fe0403873f473b29e42662cd86e8c4,
# pkgs/by-name/sc/sccache/package.nix (MIT). The drain patch is version-specific;
# updating other flake inputs must not silently select another compiler server.
{
  lib,
  fetchFromGitHub,
  rustPlatform,
  pkg-config,
  openssl,
  distributed ? false,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  version = "0.18.0";
  pname = "sccache";

  src = fetchFromGitHub {
    owner = "mozilla";
    repo = "sccache";
    rev = "v${finalAttrs.version}";
    sha256 = "sha256-QGsDxUAQzAr6Aia5/D21cGbVV0YJyuH3aBmEr9NynXQ=";
  };

  cargoHash = "sha256-Jr4+46/yhcuTC017TA2UZQJUNIBuioI3xZnxxD/rKuc=";

  buildFeatures = lib.optionals distributed [
    "dist-client"
    "dist-server"
  ];

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ openssl ];

  # Upstream tests require client/server setup unavailable in a pure build.
  # Wrix separately qualifies the patched server through real native readiness.
  doCheck = false;

  meta = {
    description = "Ccache with Cloud Storage";
    mainProgram = "sccache";
    homepage = "https://github.com/mozilla/sccache";
    changelog = "https://github.com/mozilla/sccache/releases/tag/v${finalAttrs.version}";
    maintainers = with lib.maintainers; [ doronbehar ];
    license = lib.licenses.asl20;
  };
})
