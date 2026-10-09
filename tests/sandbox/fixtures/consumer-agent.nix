{ pkgs }:

pkgs.writeShellApplication {
  name = "consumer-agent";
  runtimeInputs = [ pkgs.jq ];
  text = ''
    jq -nc --args '$ARGS.positional' -- "$@"
    IFS= read -r request
    printf 'reply:%s\n' "$request"
    printf 'consumer stderr\n' >&2
    exit 23
  '';
}
