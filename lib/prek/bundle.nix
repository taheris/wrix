{ pkgs }:

pkgs.runCommand "wrapix-prek-hooks" { } ''
  install -Dm 555 ${./hooks/pre-commit}         $out/pre-commit
  install -Dm 555 ${./hooks/pre-push}           $out/pre-push
  install -Dm 555 ${./hooks/prepare-commit-msg} $out/prepare-commit-msg
  install -Dm 555 ${./hooks/post-checkout}      $out/post-checkout
  install -Dm 555 ${./hooks/post-merge}         $out/post-merge
  install -Dm 444 ${./lock.sh}                  $out/_lib/lock.sh
''
