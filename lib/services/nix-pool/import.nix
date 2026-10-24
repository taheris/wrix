{ pkgs }:
pkgs.stdenv.mkDerivation {
  pname = "wrix-nix-pool-import";
  version = "1";
  src = ./import.cc;
  dontUnpack = true;
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.nix.dev ];
  buildPhase = ''
    (
      set -euo pipefail
      runHook preBuild
      read -r -a flags <<< "$(pkg-config --cflags --libs nix-main nix-store nix-util)"
      "$CXX" -std=c++2b "$src" "''${flags[@]}" -o wrix-nix-pool-import
      runHook postBuild
    )
  '';
  doCheck = true;
  checkPhase = ''
    (
      set -euo pipefail
      runHook preCheck
      if ./wrix-nix-pool-import /nix/store/00000000000000000000000000000000-input > denied 2>&1; then
        printf 'Trusted importer accepted a non-service caller.\\n' >&2
        exit 1
      else
        [[ "$?" == 1 ]]
      fi
      grep -q 'fixed service role' denied
      runHook postCheck
    )
  '';
  installPhase = ''
    (
      set -euo pipefail
      runHook preInstall
      install -Dm755 wrix-nix-pool-import "$out/bin/wrix-nix-pool-import"
      runHook postInstall
    )
  '';
}
