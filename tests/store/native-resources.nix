{
  bash,
  core,
  util,
  python,
  curl,
  system,
}:
kind:
let
  scripts = {
    public = ''
      ${curl}/bin/curl --fail --silent --show-error --max-time 20 https://cache.nixos.org/nix-cache-info > response
      [[ -s response ]]
      printf ok > "$out"
    '';
    private = ''
      ${core}/bin/id >&2
      ${core}/bin/cat /proc/self/cgroup /proc/self/uid_map >&2
      if denial=$( (exec 3<>/dev/tcp/127.0.0.1/18443) 2>&1); then exit 1; fi
      printf '%s\n' "$denial" >&2
      [[ "$denial" == *Operation?not?permitted* ]]
      printf ok > "$out"
    '';
    ipv6 = ''
      if denial=$( (exec 3<>/dev/tcp/::1/18443) 2>&1); then exit 1; fi
      [[ "$denial" == *Operation?not?permitted* ]]
      printf ok > "$out"
    '';
    ordinary = scripts.private;
    disk = ''
      if denial=$(${util}/bin/fallocate --length=9663676416 allocation 2>&1); then exit 1; fi
      ${core}/bin/rm allocation
      [[ "$denial" == *No?space?left?on?device* ]]
      printf ok > "$out"
    '';
    memory = ''
      value=xxxxxxxxxxxxxxxx
      for ((i=0;i<30;i++)); do value="$value$value"; done
      printf unexpected > "$out"
    '';
    pids = ''
      ${python}/bin/python3 - <<'PY'
      import errno
      import subprocess
      children = []
      denied = False
      try:
          for _ in range(256):
              try:
                  children.append(subprocess.Popen(['${core}/bin/sleep', '30']))
              except OSError as error:
                  assert error.errno == errno.EAGAIN, error
                  denied = True
                  break
          assert denied and children
      finally:
          for child in children:
              child.terminate()
          for child in children:
              child.wait(timeout=5)
      PY
      printf ok > "$out"
    '';
    cpu = ''
      ${python}/bin/python3 - <<'PY'
      import subprocess
      children = [subprocess.Popen(['${python}/bin/python3', '-c',
          'import time; end=time.monotonic()+2; exec("while time.monotonic()<end: pass")']) for _ in range(4)]
      for child in children:
          assert child.wait(timeout=15) == 0
      PY
      printf ok > "$out"
    '';
  };
  fixed = builtins.elem kind [
    "public"
    "private"
    "ipv6"
  ];
in
assert builtins.hasAttr kind scripts;
builtins.derivation (
  {
    name = "wrix-native-boundary-${kind}";
    inherit system;
    builder = "${bash}/bin/bash";
    LC_ALL = "C";
    args = [
      "-c"
      ("set -euo pipefail\n" + scripts.${kind})
    ];
  }
  // (
    if fixed then
      {
        outputHashMode = "flat";
        outputHashAlgo = "sha256";
        outputHash = builtins.hashString "sha256" "ok";
      }
    else
      { }
  )
)
