# Shared shell code snippets for sandbox implementations
#
# These are Nix strings containing shell code that can be interpolated
# into the generated launcher scripts for both Linux and Darwin.
_:

{
  # Safe path expansion function - only expands ~ and $HOME/$USER, not arbitrary commands
  # Usage: src=$(expand_path "$src")
  expandPathFn = ''
    expand_path() {
      local p="$1"
      p="''${p/#\~/$HOME}"
      p="''${p//\$HOME/$HOME}"
      p="''${p//\$USER/$USER}"
      echo "$p"
    }
  '';

  # Clean up stale staging directories from previous runs (PIDs that no longer exist)
  # Expects $WRAPIX_CACHE to be set
  cleanStaleStagingDirs = ''
    mkdir -p "$WRAPIX_CACHE/mounts"
    for stale_dir in "$WRAPIX_CACHE/mounts"/*; do
      [ -d "$stale_dir" ] || continue
      stale_pid=$(basename "$stale_dir")
      if ! kill -0 "$stale_pid" 2>/dev/null; then
        rm -rf "$stale_dir"
      fi
    done
  '';

  # Create PID-based staging directory with cleanup trap
  # Sets $STAGING_ROOT and registers EXIT trap
  # Expects $WRAPIX_CACHE to be set
  createStagingDir = ''
    STAGING_ROOT="$WRAPIX_CACHE/mounts/$$"
    mkdir -p "$STAGING_ROOT"
    trap 'rm -rf "$STAGING_ROOT"' EXIT
  '';

  # Stage .beads config files for container-local database isolation.
  # Copies config.yaml, metadata.json, and issues.jsonl to a staging directory
  # so containers get their own .beads without mounting the host's.
  # Sets $BEADS_STAGING to the staging path (empty if no .beads found).
  # Expects $PROJECT_DIR and $STAGING_ROOT to be set.
  # chmod 700: bind-mount carries perms in, bd warns if .beads/ is g+r/o+r.
  stageBeads = ''
    BEADS_STAGING=""
    if [ -d "$PROJECT_DIR/.beads" ]; then
      BEADS_STAGING="$STAGING_ROOT/beads"
      mkdir -p "$BEADS_STAGING"
      chmod 700 "$BEADS_STAGING"
      [ -f "$PROJECT_DIR/.beads/config.yaml" ] && cp "$PROJECT_DIR/.beads/config.yaml" "$BEADS_STAGING/"
      [ -f "$PROJECT_DIR/.beads/metadata.json" ] && cp "$PROJECT_DIR/.beads/metadata.json" "$BEADS_STAGING/"
      [ -f "$PROJECT_DIR/.beads/issues.jsonl" ] && cp "$PROJECT_DIR/.beads/issues.jsonl" "$BEADS_STAGING/"
    fi
  '';

  # Ensure the vmnet route exists so the host can reach Apple containers.
  # VPNs (Tailscale) capture the default route via utun*, sending container
  # subnet traffic to the tunnel instead of the local vmnet bridge.  Adds
  # /25 split routes through the bridge interface when a conflict is
  # detected; no-ops when routes already exist or no VPN is active.
  # Pipefail-safe: every command that can fail is guarded.  The outer
  # function returns 0 even on failure — callers should treat route issues
  # as warnings, not fatal errors.
  fixVmnetRoute = ''
    _fix_vmnet_route() {
      local _subnet _net _default_if _prefix _vmnet_if
      _subnet=$(container network inspect default 2>/dev/null \
        | grep -oE '"ipv4Subnet":"[^"]+"' | head -1 \
        | sed 's/.*"ipv4Subnet":"//;s/"$//;s/\\//g') || return 0
      [[ -z "$_subnet" ]] && return 0
      _net="''${_subnet%%/*}"
      _default_if=$(route -n get default 2>/dev/null \
        | awk '/interface:/{print $2}') || return 0
      [[ "$_default_if" == utun* ]] || return 0
      _prefix="''${_net%.*}"
      netstat -rn | grep -q "^$_prefix.*bridge" && return 0
      _vmnet_if=$(ifconfig 2>/dev/null \
        | grep -B5 "192.168.64" \
        | grep -oE '^[a-z][a-z0-9]+' | head -1)
      if [[ -n "$_vmnet_if" ]]; then
        echo "Adding vmnet route (VPN detected on $_default_if)" >&2
        sudo route add -net "$_net/25" \
          -interface "$_vmnet_if" 2>/dev/null || true
        sudo route add -net "''${_net%.*}.128/25" \
          -interface "$_vmnet_if" 2>/dev/null || true
      fi
    }
    _fix_vmnet_route || true
  '';

  # Generate deploy key name expression
  # If deployKey is provided, uses that; otherwise generates repo-hostname format at runtime
  mkDeployKeyExpr =
    deployKey:
    if deployKey != null then
      ''"${deployKey}"''
    else
      ''$(basename "$PROJECT_DIR")-$(hostname -s 2>/dev/null || uname -n)'';

  # Prune stale image tags across every wrapix-* repo (not just the active
  # one). After a fresh load, :latest and the new hash tag are aliases for
  # the same image ID; old hash tags from prior rebuilds are stray. For each
  # wrapix-* repo, keep :latest and any tag that aliases it (same ID/digest)
  # and delete the rest. Without this, rebuilding one profile leaves stale
  # hashes from every other profile accumulating forever, since the old
  # filter was scoped to the currently-invoked profile only.
  #
  # runtime:
  #   "podman"    — Linux, city, module, beads-dolt; repos are
  #                 localhost/wrapix-*.
  #   "container" — Darwin's Apple container CLI; repos are wrapix-*.
  # cmd: override the CLI binary (absolute path for systemd units, etc.).
  pruneStaleImages =
    {
      runtime ? "podman",
      cmd ? null,
    }:
    let
      bin = if cmd != null then cmd else runtime;
      spec =
        {
          podman = {
            list = "${bin} images --format '{{.Repository}} {{.Tag}} {{.ID}}'";
            delete = "${bin} rmi";
            pattern = "^localhost/wrapix-";
            # Name the container pinning a given tag (via $_stale at shell
            # runtime). Empty if none — lets callers print a friendly notice
            # naming the holder instead of a generic rmi error.
            holder = ''${bin} ps -a --filter "ancestor=$_stale" --format '{{.Names}}' | head -n1'';
            # Workspace label set by beads-dolt cmd_start. Empty for legacy
            # containers or when the holder disappears mid-check.
            holderWorkspace = ''${bin} inspect --format '{{ index .Config.Labels "wrapix.workspace" }}' "$_holder"'';
          };
          container = {
            list = "${bin} image list | tail -n +2";
            delete = "${bin} image delete";
            pattern = "^wrapix-";
            # Apple's container CLI has no ancestor filter — fall through
            # to the generic "pinned by a container" message.
            holder = "echo ''";
            holderWorkspace = "echo ''";
          };
        }
        .${runtime};
    in
    ''
      ${spec.list} \
        | awk '
            $1 ~ "${spec.pattern}" {
              if ($2 == "latest") { latest[$1] = $3 }
              else { rows[NR] = $1 " " $2 " " $3 }
            }
            END {
              for (i in rows) {
                split(rows[i], f, " ")
                if (f[3] != latest[f[1]]) print f[1] ":" f[2]
              }
            }' \
        | while read -r _stale; do
            if ! _err=$(${spec.delete} "$_stale" 2>&1); then
              case "$_err" in
                *"in use"*|*"is using"*)
                  if _holder=$(${spec.holder}); then
                    _holder_ws=$(${spec.holderWorkspace}) || _holder_ws=""
                    # Skip cross-workspace pins silently: the holder is
                    # correctly pinned to its own workspace's flake.lock,
                    # not actually stale from its perspective.
                    if [ -n "$_holder_ws" ] && [ "$_holder_ws" != "$PWD" ]; then
                      continue
                    fi
                    if [ -n "$_holder" ]; then
                      echo "prune-stale-images: $_stale pinned by container $_holder — upgrades on next start" >&2
                    else
                      echo "prune-stale-images: $_stale pinned by a container — upgrades on next start" >&2
                    fi
                  else
                    echo "prune-stale-images: $_stale pinned by a container (holder lookup failed)" >&2
                  fi
                  ;;
                *)
                  echo "prune-stale-images: could not remove $_stale: $_err" >&2
                  ;;
              esac
            fi
          done
    '';
}
