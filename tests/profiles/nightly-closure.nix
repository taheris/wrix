# Classify evaluated derivation graphs without exempting selected toolchains.
{
  roots,
  closure,
  toolchainClosure,
}:
let
  nightly = path: builtins.match ".*-nightly-.*" path != null;
  stableFormatterCompatibility =
    path: builtins.match ".*/[a-z0-9]+-rustfmt-nightly-[0-9]+\\.[0-9]+\\.[0-9]+\\.drv" path != null;
in
builtins.filter (
  path:
  nightly path
  && (
    !(stableFormatterCompatibility path)
    || builtins.elem path roots
    || builtins.elem path toolchainClosure
  )
) closure
