{ pkgs }:
pkgs.runCommand "wrix-worker-seccomp.json" { nativeBuildInputs = [ pkgs.jq ]; } ''
  set -euo pipefail
  jq '
    .syscalls |= (map(.names -= ["sethostname", "setdomainname"])
      | map(select(.names | length > 0))
      + [{names: ["sethostname", "setdomainname"], action: "SCMP_ACT_ALLOW", args: []}])
  ' ${pkgs.podman.src}/vendor/go.podman.io/common/pkg/seccomp/seccomp.json > "$out"
  jq -e --slurpfile baseline ${pkgs.podman.src}/vendor/go.podman.io/common/pkg/seccomp/seccomp.json '
    def without_uts:
      .syscalls |= (map(.names -= ["sethostname", "setdomainname"])
        | map(select(.names | length > 0)));
    (without_uts == ($baseline[0] | without_uts)) and
    ([.syscalls[] | select(.names | index("sethostname") or index("setdomainname"))]
      == [{names: ["sethostname", "setdomainname"], action: "SCMP_ACT_ALLOW", args: []}])
  ' "$out" > /dev/null
''
