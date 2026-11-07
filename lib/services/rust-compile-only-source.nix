# Compile-only Cargo packages do not consume Nextest runner configuration.
{ extraSrcs, ... }@args:
args
// {
  extraSrcs = builtins.removeAttrs extraSrcs [ ".config/nextest.toml" ];
}
