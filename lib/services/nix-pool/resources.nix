{
  pkgs,
  owner,
  sshdConf,
  network,
}:

pkgs.writeShellScriptBin "wrix-nix-pool-daemon" ''
  set -euo pipefail
  set -E
  umask 077
  resource_phase=admission
  resources_failed() {
    local status="$?"
    printf 'Nix pool resource admission failed in %s (status %s).\n' "$resource_phase" "$status" >&2
    exit "$status"
  }
  trap resources_failed ERR
  export PATH=${pkgs.coreutils}/bin:${pkgs.util-linux}/bin:${pkgs.nix}/bin
  [[ "$#" == 0 && $(id -u) == 0 ]]
  [[ -f /etc/wrix/nix-pool-service && ! -e /etc/wrix/image-agent ]]
  [[ $(</etc/wrix/nix-pool-service) == 1 ]]
  if [[ $(</proc/sys/vm/panic_on_oom) != 0 ]]; then
    printf '0\n' > /proc/sys/vm/panic_on_oom
  fi
  resource_phase=bootstrap-processes
  supervisor=/sys/fs/cgroup/wrix-nix-pool-supervisor
  [[ -d "$supervisor" && ! -L "$supervisor" ]]
  source ${./bootstrap-process.sh}
  supervised=false
  while read -r pid; do
    if [[ "$pid" == "$BASHPID" ]]; then supervised=true; fi
  done < "$supervisor/cgroup.procs"
  [[ "$supervised" == true ]]
  nix_pool_bootstrap_root_kernel /sys/fs/cgroup /proc
  resource_phase=aggregate-limits
  base=/sys/fs/cgroup/wrix-nix-pool
  [[ ! -e "$base" && ! -e /run/wrix-nix-pool ]]
  printf '+cpu +memory +pids\n' > /sys/fs/cgroup/cgroup.subtree_control
  mkdir "$base"
  printf '200000 100000\n' > "$base/cpu.max"
  printf '3221225472\n' > "$base/memory.max"
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
  mkdir "$base/work" "$base/control" "$base/seed"
  printf '1610612736\n' > "$base/work/memory.max"
  printf '144\n' > "$base/work/pids.max"
  printf '805306368\n' > "$base/seed/memory.max"
  printf '16\n' > "$base/seed/pids.max"
  printf '1\n' > "$base/seed/memory.oom.group"
  printf '134217728\n' > "$base/control/memory.min"
  printf '402653184\n' > "$base/control/memory.max"
  printf '96\n' > "$base/control/pids.max"
  printf '536870912\n' > "$base/memory.min"
  printf '536870912\n' > "$base/work/memory.min"
  resource_phase=network-policy
  ${network.install}/bin/wrix-nix-pool-egress
  resource_phase=private-transport
  mkdir -m 0755 /run/wrix-nix-pool
  data_owner=$(</run/wrix-nix-pool-data-owner)
  host_key=/var/lib/wrix/nix-pool/host-key
  [[ -f "$host_key" && ! -L "$host_key" ]]
  [[ $(stat -c '%u:%g:%a:%h' "$host_key") == "$data_owner:600:1" ]]
  install -m 0600 "$host_key" /run/wrix-nix-pool/host-key
  [[ $(stat -c '%u:%g:%a:%h' /run/wrix-nix-pool/host-key) == 0:0:600:1 ]]
  mkdir -m 0755 /run/wrix-nix-pool/export-root
  mount --rbind /var/lib/wrix/nix-pool/export-root /run/wrix-nix-pool/export-root
  [[ $(stat -c '%u:%g:%a' /run/wrix-nix-pool/export-root) == 0:0:755 ]]
  printf '%s\n' "$BASHPID" > /run/wrix-nix-pool/supervisor.pid
  resource_phase=native-daemon
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
  resource_phase=native-admission
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
  resource_phase=readonly-export
  export_root=/run/wrix-nix-pool/export-root
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
  resource_phase=serving
  completed=
  if wait -n -p completed "$daemon_pid" "$owner_pid" "$sshd_pid"; then
    status=0
  else
    status=$?
  fi
  work_events=$(<"$base/work/cgroup.events")
  seed_events=$(<"$base/seed/cgroup.events")
  if [[ "$status" == 0 && "$completed" == "$daemon_pid" && -f /run/wrix-nix-pool/stopping && "$work_events" == *'populated 0'* && "$seed_events" == *'populated 0'* ]]; then
    kill -TERM "$sshd_pid" "$owner_pid"
    for child in "$sshd_pid" "$owner_pid"; do
      if wait "$child"; then
        :
      else
        [[ "$?" == 143 ]]
      fi
    done
    sync -f /nix
    if [[ -e /var/lib/wrix/nix-pool/resuming || -L /var/lib/wrix/nix-pool/resuming ]]; then
      [[ -f /var/lib/wrix/nix-pool/resuming && ! -L /var/lib/wrix/nix-pool/resuming ]]
      [[ $(stat -c '%u:%g:%a:%h' /var/lib/wrix/nix-pool/resuming) == "$data_owner:600:1" ]]
      [[ $(</var/lib/wrix/nix-pool/resuming) == 1 ]]
      rm -- /var/lib/wrix/nix-pool/resuming
    fi
    (set -o noclobber; printf '1\n' > /var/lib/wrix/nix-pool/released)
    sync -f /var/lib/wrix/nix-pool
    exit 0
  fi
  if [[ "$status" == 0 ]]; then status=1; fi
  exit "$status"
''
