# Pinned upstream recipe from nixpkgs f20cd90e28fe0403873f473b29e42662cd86e8c4,
# pkgs/by-name/sc/sccache/package.nix (MIT). The drain patch is version-specific;
# updating other flake inputs must not silently select another compiler server.
/*
  Copyright (c) 2003-2026 Eelco Dolstra and the Nixpkgs/NixOS contributors

  Permission is hereby granted, free of charge, to any person obtaining
  a copy of this software and associated documentation files (the
  "Software"), to deal in the Software without restriction, including
  without limitation the rights to use, copy, modify, merge, publish,
  distribute, sublicense, and/or sell copies of the Software, and to
  permit persons to whom the Software is furnished to do so, subject to
  the following conditions:

  The above copyright notice and this permission notice shall be
  included in all copies or substantial portions of the Software.

  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
  EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
  MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
  NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
  LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
  OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
  WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/
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
