{ system, ... }:
{
  "security.pi-auth-storage".script = ''
    nix build --no-link ".#checks.${system}.pi-auth-storage"
  '';
}
