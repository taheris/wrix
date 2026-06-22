# Shared shell code snippets for sandbox implementations
#
# These are Nix strings containing shell code that can be interpolated
# into the generated launcher scripts for both Linux and Darwin.
{
  pkgs ? null,
}:

let
  jqBin = if pkgs == null then "jq" else "${pkgs.jq}/bin/jq";
in
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

  # Idempotent install of the sandbox image into the local containers store.
  # Expects $IMAGE_REF, $IMAGE_SOURCE, $IMAGE_SOURCE_KIND, $IMAGE_DIGEST_PATH,
  # and `verbose` in the caller's scope.
  #
  # Preflight is content-digest based (specs/sandbox.md § Image install path):
  # when the image's OCI config digest is already present in the local store,
  # the install pipeline is short-circuited — no stream invocation, no `*-load`
  # CLI call. This catches drv-hash rebuilds that leave the image content
  # untouched (where `mkImageRef`'s tag changes but the content does not),
  # the case the prior `podman image exists $IMAGE_REF` preflight missed.
  #
  # $IMAGE_DIGEST_PATH may be empty (for example, when `wrix spawn` selects an
  # image override not represented by the supplied ProfileConfig); descriptor
  # and archive sources derive the selected source digest before preflight.
  #
  # On miss, the install transport dispatches by source kind. Linux descriptors
  # flow through `skopeo copy nix:<descriptor> → containers-storage:<ref>`;
  # tar-loadable archives flow through `docker-archive:<path>`.
  imageLoadStep = ''
    if [[ -z "$IMAGE_SOURCE" ]]; then
      verbose "Using cached image $IMAGE_REF"
    else
      _wrix_skip_load=0
      _wrix_source_kind="''${IMAGE_SOURCE_KIND:-legacy-stream}"
      _wrix_desired_digest=""
      if [[ -n "''${IMAGE_DIGEST_PATH:-}" ]]; then
        if [[ "$IMAGE_DIGEST_PATH" =~ ^sha256:[0-9a-f]{64}$ ]]; then
          _wrix_desired_digest="$IMAGE_DIGEST_PATH"
        elif [[ -s "$IMAGE_DIGEST_PATH" ]]; then
          _wrix_digest_candidate=$(cat "$IMAGE_DIGEST_PATH")
          if [[ "$_wrix_digest_candidate" =~ ^sha256:[0-9a-f]{64}$ ]]; then
            _wrix_desired_digest="$_wrix_digest_candidate"
          fi
        fi
      fi
      if [[ -z "$_wrix_desired_digest" ]]; then
        case "$_wrix_source_kind" in
          nix-descriptor)
            if ! _wrix_desired_digest=$(${jqBin} -er '.digest // empty | strings | select(test("^sha256:[0-9a-f]{64}$"))' "$IMAGE_SOURCE"); then
              echo "Error: nix-descriptor image source is missing a sha256 digest: $IMAGE_SOURCE" >&2
              exit 1
            fi
            ;;
          docker-archive)
            if ! _wrix_desired_digest=$(skopeo inspect --raw "docker-archive:$IMAGE_SOURCE" | ${jqBin} -er '.config.digest // empty | strings | select(test("^sha256:[0-9a-f]{64}$"))'); then
              echo "Error: docker-archive image source is missing a sha256 digest: $IMAGE_SOURCE" >&2
              exit 1
            fi
            ;;
        esac
      fi
      if [[ -n "$_wrix_desired_digest" ]]; then
        if podman image inspect --format '{{.Id}}' "$_wrix_desired_digest" >/dev/null 2>&1; then
          # best-effort: aliasing the desired ref to the matching content is
          # convenience only — a non-zero exit (e.g. ref already points at
          # the same Id) is benign; tar bytes still aren't streamed.
          podman tag "$_wrix_desired_digest" "$IMAGE_REF" >/dev/null 2>&1 || true
          _wrix_skip_load=1
        fi
      elif podman image exists "$IMAGE_REF" 2>/dev/null; then
        _wrix_skip_load=1
      fi

      if [[ "$_wrix_skip_load" == "1" ]]; then
        verbose "Using cached image $IMAGE_REF"
      else
        verbose "Loading image from $IMAGE_SOURCE..."
        _wrix_img_tmp=$(mktemp -d)
        # The containers-storage leg writes into podman's store. skopeo resolves
        # that store on its own and, when XDG_RUNTIME_DIR is unset, falls back to
        # the rootful runroot (/run/containers/storage) a non-root user cannot
        # create. Pin the destination to podman's actual store so skopeo writes
        # exactly where podman reads.
        _wrix_store_ref="containers-storage:$IMAGE_REF"
        _wrix_store_spec=$(podman info \
          --format '{{.Store.GraphDriverName}}@{{.Store.GraphRoot}}+{{.Store.RunRoot}}' \
          2>/dev/null) || _wrix_store_spec=""
        if [[ "$_wrix_store_spec" == *@*+* ]]; then
          _wrix_store_ref="containers-storage:[$_wrix_store_spec]$IMAGE_REF"
        fi
        case "$_wrix_source_kind" in
          nix-descriptor)
            _wrix_skopeo_err="$_wrix_img_tmp/skopeo-nix.err"
            if skopeo --insecure-policy copy --quiet \
              "nix:$IMAGE_SOURCE" \
              "$_wrix_store_ref" 2>"$_wrix_skopeo_err"; then
              :
            else
              _wrix_skopeo_status=$?
              if grep -Eq 'unknown transport.*nix' "$_wrix_skopeo_err"; then
                _wrix_fallback_stream=$(${jqBin} -er '.fallback_stream // empty | strings | select(length > 0)' "$IMAGE_SOURCE") || {
                  cat "$_wrix_skopeo_err" >&2
                  echo "Error: nix-descriptor source is not supported by this skopeo and has no fallback_stream: $IMAGE_SOURCE" >&2
                  exit "$_wrix_skopeo_status"
                }
                verbose "skopeo lacks nix transport; falling back to descriptor stream $_wrix_fallback_stream"
                "$_wrix_fallback_stream" >"$_wrix_img_tmp/image.tar"
                skopeo --insecure-policy copy --quiet \
                  "docker-archive:$_wrix_img_tmp/image.tar" \
                  "$_wrix_store_ref"
              else
                cat "$_wrix_skopeo_err" >&2
                exit "$_wrix_skopeo_status"
              fi
            fi
            ;;
          docker-archive)
            skopeo --insecure-policy copy --quiet \
              "docker-archive:$IMAGE_SOURCE" \
              "$_wrix_store_ref"
            ;;
          legacy-stream)
            "$IMAGE_SOURCE" >"$_wrix_img_tmp/image.tar"
            skopeo --insecure-policy copy --quiet \
              "docker-archive:$_wrix_img_tmp/image.tar" \
              "$_wrix_store_ref"
            ;;
          *)
            echo "Error: unsupported image source_kind: $_wrix_source_kind" >&2
            exit 1
            ;;
        esac
        rm -rf "$_wrix_img_tmp"
        IMAGE_REPO="''${IMAGE_REF%:*}"
        # best-effort: :latest is pruneStaleImages' keep-anchor; a tag
        # failure only loses the convenience alias.
        podman tag "$IMAGE_REF" "$IMAGE_REPO:latest" 2>/dev/null || true
        verbose "Loaded image $IMAGE_REF"
      fi
    fi
  '';

  # Clean up stale staging directories from previous runs (PIDs that no longer exist)
  # Expects $WRIX_CACHE to be set
  cleanStaleStagingDirs = ''
    mkdir -p "$WRIX_CACHE/mounts"
    for stale_dir in "$WRIX_CACHE/mounts"/*; do
      [ -d "$stale_dir" ] || continue
      stale_pid=$(basename "$stale_dir")
      if ! kill -0 "$stale_pid" 2>/dev/null; then
        rm -rf "$stale_dir"
      fi
    done
  '';

  # Create PID-based staging directory with cleanup trap
  # Sets $STAGING_ROOT and registers EXIT trap
  # Expects $WRIX_CACHE to be set
  createStagingDir = ''
    STAGING_ROOT="$WRIX_CACHE/mounts/$$"
    mkdir -p "$STAGING_ROOT"
    trap 'rm -rf "$STAGING_ROOT"' EXIT
  '';

  # Stage .beads config and metadata for Dolt-backed sandbox clients.
  # Sets $BEADS_STAGING to the staging path (empty if no .beads found).
  # Expects $PROJECT_DIR and $STAGING_ROOT to be set.
  # chmod 700: bind-mount carries perms in, bd warns if .beads/ is g+r/o+r.
  stageBeads = ''
    BEADS_STAGING=""
    if [[ -d "$PROJECT_DIR/.beads" ]]; then
      BEADS_STAGING="$STAGING_ROOT/beads"
      mkdir -p "$BEADS_STAGING"
      chmod 700 "$BEADS_STAGING"
      [[ -f "$PROJECT_DIR/.beads/config.yaml" ]] && cp "$PROJECT_DIR/.beads/config.yaml" "$BEADS_STAGING/"
      [[ -f "$PROJECT_DIR/.beads/metadata.json" ]] && cp "$PROJECT_DIR/.beads/metadata.json" "$BEADS_STAGING/"
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
    _vpn_conflict=false
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
      _vpn_conflict=true
      _prefix="''${_net%.*}"
      netstat -rn | grep -q "^''${_prefix}\.128.*bridge" && return 0
      _vmnet_if=$(ifconfig 2>/dev/null \
        | grep -B5 "192.168.64" \
        | grep -oE '^[a-z][a-z0-9]+' | head -1)
      if [[ -n "$_vmnet_if" ]]; then
        echo "Adding vmnet route (VPN detected on $_default_if)" >&2
        sudo route add -net "$_net/25" \
          -interface "$_vmnet_if"
        sudo route add -net "''${_net%.*}.128/25" \
          -interface "$_vmnet_if"
      fi
    }
    _fix_vmnet_route
  '';

  # Generate deploy key name expression
  # If deployKey is provided, uses that; otherwise generates repo-hostname format at runtime
  mkDeployKeyExpr =
    deployKey:
    if deployKey != null then
      ''"${deployKey}"''
    else
      ''$(basename "$PROJECT_DIR")-$(hostname -s 2>/dev/null || uname -n)'';

  # Remember the image a workspace just used in a shared eight-record MRU.
  rememberImageRef = ''
    if [[ -n "''${IMAGE_REF:-}" && -n "''${WRIX_CACHE:-}" ]]; then
      mkdir -p "$WRIX_CACHE"
      _wrix_mru_file="$WRIX_CACHE/image-mru.json"
      _wrix_mru_tmp=$(mktemp "$WRIX_CACHE/image-mru.XXXXXX")
      _wrix_mru_old=$(mktemp "$WRIX_CACHE/image-mru-old.XXXXXX")
      _wrix_digest=""
      _wrix_image_id=""

      if [[ -n "''${IMAGE_DIGEST_PATH:-}" ]]; then
        if [[ "$IMAGE_DIGEST_PATH" == sha256:* ]]; then
          _wrix_digest="$IMAGE_DIGEST_PATH"
        elif [[ -s "$IMAGE_DIGEST_PATH" ]]; then
          _wrix_digest=$(cat "$IMAGE_DIGEST_PATH")
        fi
      fi

      if command -v podman >/dev/null 2>&1; then
        if _wrix_image_id=$(podman image inspect --format '{{.Id}}' "$IMAGE_REF" 2>/dev/null); then
          :
        else
          _wrix_image_id=""
        fi
      elif command -v container >/dev/null 2>&1; then
        if _wrix_image_id=$(container image inspect "$IMAGE_REF" 2>/dev/null | ${jqBin} -r '.[0].id // .[0].digest // empty'); then
          :
        else
          _wrix_image_id=""
        fi
      fi

      if [[ -f "$_wrix_mru_file" ]]; then
        if ${jqBin} -e 'type == "array"' "$_wrix_mru_file" >/dev/null; then
          cp "$_wrix_mru_file" "$_wrix_mru_old"
        else
          echo "wrix: resetting invalid image MRU: $_wrix_mru_file" >&2
          printf '[]\n' > "$_wrix_mru_old"
        fi
      else
        printf '[]\n' > "$_wrix_mru_old"
      fi

      ${jqBin} -n \
        --arg ref "$IMAGE_REF" \
        --arg digest "$_wrix_digest" \
        --arg id "$_wrix_image_id" \
        --slurpfile old "$_wrix_mru_old" \
        'def compact_record: with_entries(select(.value != ""));
         def record_key: [(.ref // ""), (.digest // ""), (.id // "")] | @json;
         ([{ ref: $ref, digest: $digest, id: $id } | compact_record] + ($old[0] // []))
         | map(select((.ref // "") != "" or (.digest // "") != "" or (.id // "") != ""))
         | reduce .[] as $item ({ seen: {}, out: [] };
             ($item | record_key) as $key
             | if .seen[$key] then . else (.seen[$key] = true | .out += [$item]) end)
         | .out[:8]' \
        > "$_wrix_mru_tmp"
      mv "$_wrix_mru_tmp" "$_wrix_mru_file"
      rm -f "$_wrix_mru_old"
    fi
  '';

  # Prune wrix-owned images outside the current/container-used/MRU keep set.
  # Legacy wrix-* refs are included so older unlabelled wrix tags age out; an
  # unlabelled <none>:<none> image is never an automatic-delete candidate.
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
            pattern = "^localhost/wrix-";
            imageId = ''${bin} image inspect --format '{{.Id}}' "$1"'';
            imageDigest = ''${bin} image inspect --format '{{.Digest}}' "$1"'';
            managedLabel = ''${bin} image inspect --format '{{ index .Config.Labels "wrix.managed" }}' "$1"'';
            holder = ''${bin} ps -a --filter "ancestor=$_target" --format '{{.Names}}' | head -n1'';
          };
          container = {
            list = "${bin} image list | tail -n +2";
            delete = "${bin} image delete";
            pattern = "^wrix-";
            imageId = ''${bin} image inspect "$1" | ${jqBin} -r ".[0].id // .[0].digest // empty"'';
            imageDigest = ''${bin} image inspect "$1" | ${jqBin} -r ".[0].digest // .[0].id // empty"'';
            managedLabel = ''${bin} image inspect "$1" | ${jqBin} -r ".[0].labels[\"wrix.managed\"] // .[0].Labels[\"wrix.managed\"] // empty"'';
            holder = "printf ''";
          };
        }
        .${runtime};
    in
    ''
      _wrix_keep_refs=$(mktemp)
      _wrix_keep_ids=$(mktemp)
      _wrix_keep_digests=$(mktemp)

      _wrix_normal_value() {
        local _value="$1"
        case "$_value" in
          ""|"<none>"|"<no value>"|"null") return 1 ;;
          *) printf '%s\n' "$_value" ;;
        esac
      }

      _wrix_add_keep_ref() {
        local _value
        if _value=$(_wrix_normal_value "$1"); then
          printf '%s\n' "$_value" >> "$_wrix_keep_refs"
        fi
      }

      _wrix_add_keep_id() {
        local _value
        if _value=$(_wrix_normal_value "$1"); then
          printf '%s\n' "$_value" >> "$_wrix_keep_ids"
        fi
      }

      _wrix_add_keep_digest() {
        local _value
        if _value=$(_wrix_normal_value "$1"); then
          printf '%s\n' "$_value" >> "$_wrix_keep_digests"
        fi
      }

      _wrix_image_id() {
        ${spec.imageId}
      }

      _wrix_image_digest() {
        ${spec.imageDigest}
      }

      _wrix_managed_label() {
        ${spec.managedLabel}
      }

      _wrix_keep_match() {
        local _ref="$1"
        local _id="$2"
        local _digest="$3"
        if [[ -n "$_ref" ]] && grep -Fxq "$_ref" "$_wrix_keep_refs"; then
          return 0
        fi
        if [[ -n "$_id" ]] && grep -Fxq "$_id" "$_wrix_keep_ids"; then
          return 0
        fi
        if [[ -n "$_digest" ]] && grep -Fxq "$_digest" "$_wrix_keep_digests"; then
          return 0
        fi
        return 1
      }

      if [[ -n "''${IMAGE_REF:-}" ]]; then
        _wrix_add_keep_ref "$IMAGE_REF"
        if _wrix_current_id=$(_wrix_image_id "$IMAGE_REF" 2>/dev/null); then
          _wrix_add_keep_id "$_wrix_current_id"
        fi
        if _wrix_current_digest=$(_wrix_image_digest "$IMAGE_REF" 2>/dev/null); then
          _wrix_add_keep_digest "$_wrix_current_digest"
        fi
      fi
      if [[ -n "''${IMAGE_DIGEST_PATH:-}" ]]; then
        if [[ "$IMAGE_DIGEST_PATH" == sha256:* ]]; then
          _wrix_add_keep_digest "$IMAGE_DIGEST_PATH"
        elif [[ -s "$IMAGE_DIGEST_PATH" ]]; then
          _wrix_add_keep_digest "$(cat "$IMAGE_DIGEST_PATH")"
        fi
      fi

      _wrix_mru_file="''${WRIX_IMAGE_KEEP_FILE:-}"
      if [[ -z "$_wrix_mru_file" && -n "''${WRIX_CACHE:-}" ]]; then
        _wrix_mru_file="$WRIX_CACHE/image-mru.json"
      fi
      if [[ -n "$_wrix_mru_file" && -f "$_wrix_mru_file" ]]; then
        if ${jqBin} -e 'type == "array"' "$_wrix_mru_file" >/dev/null; then
          ${jqBin} -r '.[] | .ref // empty' "$_wrix_mru_file" >> "$_wrix_keep_refs"
          ${jqBin} -r '.[] | .id // empty' "$_wrix_mru_file" >> "$_wrix_keep_ids"
          ${jqBin} -r '.[] | .digest // empty' "$_wrix_mru_file" >> "$_wrix_keep_digests"
        else
          while IFS= read -r _legacy_ref; do
            _wrix_add_keep_ref "$_legacy_ref"
          done < "$_wrix_mru_file"
        fi
      elif [[ -n "''${WRIX_CACHE:-}" && -f "$WRIX_CACHE/image-refs" ]]; then
        while IFS= read -r _legacy_ref; do
          _wrix_add_keep_ref "$_legacy_ref"
        done < "$WRIX_CACHE/image-refs"
      fi

      ${spec.list} \
        | while read -r _repo _tag _listed_id _rest; do
            [[ -n "''${_repo:-}" ]] || continue
            _ref=""
            if [[ "$_repo" != "<none>" && "$_tag" != "<none>" ]]; then
              _ref="$_repo:$_tag"
            fi
            _target="''${_ref:-$_listed_id}"
            if ! _target=$(_wrix_normal_value "$_target"); then
              continue
            fi

            _managed=""
            if _managed=$(_wrix_managed_label "$_target" 2>/dev/null); then
              :
            else
              _managed=""
            fi
            _legacy=0
            if [[ -n "$_ref" && "$_ref" =~ ${spec.pattern} ]]; then
              _legacy=1
            fi
            if [[ "$_managed" != "true" && "$_legacy" != "1" ]]; then
              continue
            fi

            _id="$_listed_id"
            if _inspected_id=$(_wrix_image_id "$_target" 2>/dev/null); then
              if _normal_id=$(_wrix_normal_value "$_inspected_id"); then
                _id="$_normal_id"
              fi
            fi
            _digest=""
            if _inspected_digest=$(_wrix_image_digest "$_target" 2>/dev/null); then
              if _normal_digest=$(_wrix_normal_value "$_inspected_digest"); then
                _digest="$_normal_digest"
              fi
            fi
            if _wrix_keep_match "$_ref" "$_id" "$_digest"; then
              continue
            fi

            _holder=""
            if _holder=$(${spec.holder} 2>/dev/null); then
              if [[ -n "$_holder" ]]; then
                continue
              fi
            fi

            if ! _err=$(${spec.delete} "$_target" 2>&1); then
              case "$_err" in
                *"in use"*|*"is using"*)
                  echo "prune-stale-images: $_target pinned by a container — upgrades on next start" >&2
                  ;;
                *)
                  echo "prune-stale-images: could not remove $_target: $_err" >&2
                  ;;
              esac
            fi
          done

      rm -f "$_wrix_keep_refs" "$_wrix_keep_ids" "$_wrix_keep_digests"
    '';
}
