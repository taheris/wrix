{ isDarwin }:

{
  command,
  onFailure,
}:

let
  runDirect = ''
    if ! ${command}; then
      ${onFailure}
    fi
  '';
in
if isDarwin then
  runDirect
else
  ''
    if command -v systemd-run >/dev/null 2>&1 \
       && command -v systemctl >/dev/null 2>&1 \
       && systemctl --user show-environment >/dev/null 2>&1; then
      if ! systemd-run --user --scope --quiet --collect -- ${command}; then
        ${onFailure}
      fi
    else
      ${runDirect}
    fi
  ''
