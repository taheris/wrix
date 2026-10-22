{
  pkgs,
  owner,
  sshdConf,
}:

pkgs.writeShellScriptBin "wrix-nix-pool-daemon" ''
  set -euo pipefail
  umask 077
  export PATH=${pkgs.coreutils}/bin:${pkgs.util-linux}/bin:${pkgs.nix}/bin
  [[ "$#" == 0 && $(id -u) == 0 ]]
  [[ -f /etc/wrix/nix-pool-service && ! -e /etc/wrix/image-agent ]]
  [[ $(</etc/wrix/nix-pool-service) == 1 ]]
  printf '0\n' > /proc/sys/vm/panic_on_oom
  base=/sys/fs/cgroup/wrix-nix-pool
  [[ ! -e "$base" && ! -e /run/wrix-nix-pool ]]
  printf '+cpu +memory +pids\n' > /sys/fs/cgroup/cgroup.subtree_control
  mkdir "$base"
  printf '200000 100000\n' > "$base/cpu.max"
  printf '1610612736\n' > "$base/memory.max"
  if [[ -e "$base/memory.swap.max" ]]; then
    printf '0\n' > "$base/memory.swap.max"
  else
    no_swap=false
    while read -r field value unit; do
      if [[ "$field" == SwapTotal: ]]; then
        [[ "$value" == 0 && "$unit" == kB ]]
        no_swap=true
        break
      fi
    done < /proc/meminfo
    [[ "$no_swap" == true ]]
  fi
  printf '256\n' > "$base/pids.max"
  printf '+cpu +memory +pids\n' > "$base/cgroup.subtree_control"
  mkdir "$base/work" "$base/control"
  printf '134217728\n' > "$base/control/memory.min"
  printf '402653184\n' > "$base/control/memory.max"
  printf '96\n' > "$base/control/pids.max"
  printf '536870912\n' > "$base/memory.min"
  printf '536870912\n' > "$base/work/memory.min"
  mkdir -m 0755 /run/wrix-nix-pool
  printf '%s\n' "$BASHPID" > /run/wrix-nix-pool/supervisor.pid
  exec 3<> /nix/var/nix/gc.lock
  flock --shared 3
  (
    printf '%s\n' "$BASHPID" > "$base/work/cgroup.procs"
    exec nix-daemon --store local
  ) &
  daemon_pid=$!
  ready=false
  for _attempt in {1..200}; do
    if [[ -f "$base/work/nix-daemon/cgroup.procs" && -n $(<"$base/work/nix-daemon/cgroup.procs") ]]; then
      printf '+cpu +memory +pids\n' > "$base/work/cgroup.subtree_control"
      printf '536870912\n' > "$base/work/nix-daemon/memory.min"
      printf '%s\n' "$daemon_pid" > /run/wrix-nix-pool/daemon.pid
      ready=true
      break
    fi
    if ! kill -0 "$daemon_pid"; then
      wait "$daemon_pid"
      exit 1
    fi
    sleep 0.01
  done
  if [[ "$ready" != true ]]; then
    printf 'Native Nix cgroup resource admission did not complete.\n' >&2
    exit 1
  fi
  (
    printf '%s\n' "$BASHPID" > "$base/control/cgroup.procs"
    exec ${owner}/bin/wrix-nix-pool-owner serve <&3
  ) &
  owner_pid=$!
  printf '%s\n' "$owner_pid" > /run/wrix-nix-pool/owner.pid
  protected=false
  for _attempt in {1..200}; do
    if [[ -S /run/wrix-nix-pool/admission.sock ]]; then
      protected=true
      break
    fi
    if ! kill -0 "$owner_pid"; then
      wait "$owner_pid"
      exit 1
    fi
    sleep 0.01
  done
  [[ "$protected" == true ]]
  export_root=/var/lib/wrix/nix-pool/export-root
  touch "$export_root/run/wrix-nix-pool/admission.sock"
  mount --bind /run/wrix-nix-pool/admission.sock "$export_root/run/wrix-nix-pool/admission.sock"
  install -m 0555 ${owner}/bin/wrix-nix-pool-owner /run/wrix-nix-pool/keys
  (
    printf '%s\n' "$BASHPID" > "$base/control/cgroup.procs"
    exec ${pkgs.openssh}/bin/sshd -D -e -f ${sshdConf}
  ) &
  sshd_pid=$!
  printf '%s\n' "$sshd_pid" > /run/wrix-nix-pool/transport.pid
  printf '1\n' > /run/wrix-nix-pool/ready
  wait -n "$daemon_pid" "$owner_pid" "$sshd_pid"
''
