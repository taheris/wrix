let
  classify = import ./nightly-closure.nix;
  stable = "/nix/store/aaaa-rust-stable-1.99.0.drv";
  compiler = "/nix/store/bbbb-rustc-nightly-2026-10-30.drv";
  formatter = "/nix/store/cccc-rustfmt-nightly-1.99.0.drv";
  nightlyFormatter = "/nix/store/dddd-rustfmt-nightly-2026-10-30.drv";
  scan =
    extra:
    classify (
      {
        roots = [ stable ];
        closure = [ stable ];
        toolchainClosure = [ stable ];
      }
      // extra
    );
in
assert scan { } == [ ];
assert
  scan {
    closure = [
      stable
      compiler
    ];
  } == [ compiler ];
assert
  scan {
    closure = [
      stable
      formatter
    ];
  } == [ ];
assert
  scan {
    roots = [
      stable
      formatter
    ];
    closure = [
      stable
      formatter
    ];
  } == [ formatter ];
assert
  scan {
    closure = [
      stable
      formatter
    ];
    toolchainClosure = [
      stable
      formatter
    ];
  } == [ formatter ];
assert
  scan {
    closure = [
      stable
      nightlyFormatter
    ];
  } == [ nightlyFormatter ];
true
