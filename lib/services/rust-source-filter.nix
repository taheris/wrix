{
  lib,
  filterCargoSources,
  root,
}:
path: type:
let
  relative = lib.removePrefix "${toString root}/" path;
in
relative != ".config/nextest.toml"
&& builtins.match "tests(/.*)?" relative == null
&& builtins.match "crates/[^/]+/(tests|examples)(/.*)?" relative == null
&& (relative == "lib/sandbox/network-bootstrap.sh" || filterCargoSources path type)
