# Compatibility import: credential persistence and launch planning belong to Rust.
{ serviceCli, ... }:
{
  mkSandbox = _: serviceCli;
}
