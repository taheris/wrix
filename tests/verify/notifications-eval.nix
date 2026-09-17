{
  root,
  system,
  target,
}:

let
  inherit (builtins)
    any
    attrNames
    concatLists
    concatStringsSep
    fromJSON
    getAttr
    getFlake
    hasAttr
    match
    readFile
    throw
    toString
    ;
  rootString = toString root;
  flake = getFlake "git+file://${rootString}";
  inherit (flake.inputs.nixpkgs.legacyPackages.${system}.lib) hasInfix;

  ensure = condition: message: if condition then true else throw "verify:${target}: ${message}";
  daemonText = import "${rootString}/lib/notify/daemon.nix" {
    pkgs = {
      stdenv.hostPlatform.isDarwin = true;
      bash = "bash";
      coreutils = "coreutils";
      jq = "jq";
      socat = "socat";
      terminal-notifier = "terminal-notifier";
      libnotify = "libnotify";
      writeShellApplication = args: args.text;
    };
  };
  settings = fromJSON (
    readFile flake.packages.${system}.sandbox-claude.passthru.image.claudeSettingsJson
  );
  stopCommands = concatLists (map (entry: entry.hooks or [ ]) (settings.hooks.Stop or [ ]));
  invokesNotifyClient = any (
    hook:
    (hook.type or "") == "command" && match "^wrix-notify([[:space:]].*)?$" (hook.command or "") != null
  ) stopCommands;

  checks = {
    "notifications.claude-stop-hook-config" =
      ensure invokesNotifyClient "Claude Stop hooks do not invoke the wrix-notify command";

    "notifications.macos-tcp-bind-address" =
      ensure (hasInfix "TCP-LISTEN:5959,bind=192.168.64.1" daemonText) "Darwin daemon does not bind TCP port 5959 to 192.168.64.1"
      && ensure (
        !(hasInfix "TCP-LISTEN:5959,bind=0.0.0.0" daemonText)
      ) "Darwin daemon binds TCP port 5959 to 0.0.0.0";
  };
in
if hasAttr target checks then
  if getAttr target checks then "passed" else throw "verify:${target}: failed"
else
  throw "unknown notifications eval target ${target}; known targets: ${concatStringsSep ", " (attrNames checks)}"
