{ pkgs }:

let
  mkHook =
    name:
    pkgs.writeShellApplication {
      name = "wrapix-prek-hook-${name}";
      runtimeInputs = [ pkgs.prek ];
      text = builtins.readFile (./hooks + "/${name}");
    };

  hooks = {
    pre-commit = mkHook "pre-commit";
    pre-push = mkHook "pre-push";
    prepare-commit-msg = mkHook "prepare-commit-msg";
    post-checkout = mkHook "post-checkout";
    post-merge = mkHook "post-merge";
  };

in
pkgs.runCommand "wrapix-prek-hooks" { } ''
  install -Dm 555 ${hooks.pre-commit}/bin/wrapix-prek-hook-pre-commit $out/pre-commit
  install -Dm 555 ${hooks.pre-push}/bin/wrapix-prek-hook-pre-push $out/pre-push
  install -Dm 555 ${hooks.prepare-commit-msg}/bin/wrapix-prek-hook-prepare-commit-msg $out/prepare-commit-msg
  install -Dm 555 ${hooks.post-checkout}/bin/wrapix-prek-hook-post-checkout $out/post-checkout
  install -Dm 555 ${hooks.post-merge}/bin/wrapix-prek-hook-post-merge $out/post-merge
''
