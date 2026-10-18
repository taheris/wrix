{ wlib, root }:
let
  inherit (builtins) all deepSeq tryEval;
  rule = host: ports: { inherit host ports; };
  base = [
    (rule "api.anthropic.com" [ 443 ])
    (rule "cache.nixos.org" [ 443 ])
    (rule "github.com" [
      22
      443
    ])
    (rule "ssh.github.com" [ 443 ])
  ];
  rust =
    base
    ++ map (host: rule host [ 443 ]) [
      "crates.io"
      "static.crates.io"
      "index.crates.io"
    ];
  python =
    base
    ++ map (host: rule host [ 443 ]) [
      "pypi.org"
      "files.pythonhosted.org"
    ];
  pinned =
    networkAllowlist:
    wlib.rustProfile {
      toolchain = root + "/tests/fixtures/rust-toolchain.toml";
      sha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";
      inherit networkAllowlist;
    };
  constructors = [
    (rules: wlib.deriveProfile wlib.profiles.base { networkAllowlist = rules; })
    pinned
  ];
  accepts = constructor: rules: (tryEval (deepSeq (constructor rules).networkAllowlist true)).success;
  invalid = [
    [ "example.org" ]
    [ "example.org:443" ]
    [ { host = "example.org"; } ]
    [ (rule "" [ 443 ]) ]
    [ (rule "example.org" [ ]) ]
    [ (rule "example.org" [ 0 ]) ]
    [ (rule "example.org" [ (-1) ]) ]
    [ (rule "example.org" [ 65536 ]) ]
    [ (rule "example.org" [ 443.5 ]) ]
    [ (rule "example.org" [ "443" ]) ]
    [ (rule "example.org" [ true ]) ]
    [ (rule "example.org" [ null ]) ]
    [ ((rule "example.org" [ 443 ]) // { extra = true; }) ]
  ];
  first = [ (rule "example.org" [ 8443 ]) ];
  second = [
    (rule "example.org" [ 22 ])
    (rule "other.example" [
      1
      65535
    ])
  ];
  derived = wlib.deriveProfile wlib.profiles.base { networkAllowlist = first; };
in
{
  "profiles.network-rule-contract" = all (
    constructor:
    accepts constructor (first ++ second) && all (rules: !(accepts constructor rules)) invalid
  ) constructors;
  "profiles.builtin-network-ports" =
    wlib.profiles.base.networkAllowlist == base
    && wlib.profiles.rust.networkAllowlist == rust
    && wlib.profiles.python.networkAllowlist == python;
  "profiles.network-rule-extension" =
    (wlib.deriveProfile derived { networkAllowlist = second; }).networkAllowlist
    == base ++ first ++ second
    && (pinned (first ++ second)).networkAllowlist == rust ++ first ++ second;
}
