{ system, ... }:
{
  "security.pi-auth-storage" = ''
    nix build --no-link ".#checks.${system}.pi-auth-storage"
  '';
}
