# Sandbox image runtime checks: verify each agent variant's image closure
# contains the expected agent runtime binary, and the `wrix spawn` image
# load contract is idempotent against a shim podman.
{
  pkgs,
  system,
  linuxPkgs,
  crane,
  fenix,
  treefmt,
  serviceCli,
}:

let
  inherit (pkgs) lib; # threaded for symmetry with other test imports
  inherit (lib)
    concatStringsSep
    elem
    filterAttrs
    hasAttr
    mapAttrsToList
    optionalString
    optionals
    subtractLists
    ;
  discardContext = value: builtins.unsafeDiscardStringContext (toString value);

  shellLib = import ../../lib/util/shell.nix { };

  isLinux = elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  sandboxLib = import ../../lib/sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      serviceCli
      ;
  };
  defaultImage = (sandboxLib.mkSandbox { profile = sandboxLib.profiles.base; }).image;
  builderImage = import ../../lib/sandbox/builder/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    asTarball = !isLinux;
  };
  serviceProfiles = import ../../lib/sandbox/profiles.nix {
    pkgs = linuxPkgs;
    hostPkgs = linuxPkgs;
    inherit crane fenix treefmt;
  };
  serviceRust = import ../../lib/services/rust.nix {
    pkgs = linuxPkgs;
    rustProfile = serviceProfiles.rust;
  };
  serviceImage = import ../../lib/services/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    inherit (serviceRust) cacheServe;
    asTarball = !isLinux;
  };
  expectedImageSourceKind = if isLinux then "nix-descriptor" else "docker-archive";

  imageStream = image: image.stream or image;
  imageLayers = image: (imageStream image).conf.drvAttrs.layersJsonFile;
  imageAssemblyDerivations = {
    profile = defaultImage;
    profile-stream = imageStream defaultImage;
    profile-config = (imageStream defaultImage).conf;
    profile-layers = imageLayers defaultImage;
    profile-pipeline = (imageLayers defaultImage).drvAttrs.pipeline;
    base = defaultImage.baseImage;
    base-layers = imageLayers defaultImage.baseImage;
    stable-profile = defaultImage.stableProfileImage;
    stable-profile-closure = defaultImage.stableProfileImage.lowerTiersClosure;
    stable-profile-layers = imageLayers defaultImage.stableProfileImage;
    stable-profile-pipeline = (imageLayers defaultImage.stableProfileImage).drvAttrs.pipeline;
    agent = defaultImage.agentImage;
    agent-closure = defaultImage.agentImage.lowerTiersClosure;
    agent-layers = imageLayers defaultImage.agentImage;
    agent-pipeline = (imageLayers defaultImage.agentImage).drvAttrs.pipeline;
    builder = builderImage;
    builder-layers = imageLayers builderImage;
    service = serviceImage;
    service-layers = imageLayers serviceImage;
  };
  nonNativeImageAssembly = filterAttrs (_name: drv: drv.system != system) imageAssemblyDerivations;
  nonNativeImageAssemblyMessage = concatStringsSep ", " (
    mapAttrsToList (name: drv: "${name}=${drv.system}") nonNativeImageAssembly
  );
  imageAssemblyNativeCheck =
    assert
      nonNativeImageAssembly == { }
      || throw "image assembly derivations must use ${system}, found ${nonNativeImageAssemblyMessage}";
    pkgs.runCommandLocal "test-image-assembly-native" { } ''
      mkdir "$out"
    '';

  archiveShellHelpers = ''
    unpack_archive() {
        local label="$1"
        local archive="$2"
        local dest="$3"
        mkdir -p "$dest"
        if [[ -x "$archive" ]]; then
            "$archive" | tar -xf - -C "$dest"
        else
            tar -xf "$archive" -C "$dest"
        fi
        if [[ ! -f "$dest/manifest.json" ]]; then
            echo "FAIL: $label archive did not contain manifest.json" >&2
            exit 1
        fi
    }

    write_unique_layers() {
        local archive_dir="$1"
        local output="$2"
        jq -r '.[0].Layers[]' "$archive_dir/manifest.json" | sort -u >"$output"
    }

    list_layer_store_paths() {
        local archive_dir="$1"
        local layers_file="$2"
        local layer
        while IFS= read -r layer; do
            [[ -n "$layer" ]] || continue
            tar -tf "$archive_dir/$layer"
        done <"$layers_file" \
            | awk 'match($0, /nix\/store\/[a-z0-9]{32}-[^\/]+/) { print "/" substr($0, RSTART, RLENGTH) }' \
            | sort -u
    }

    extract_layer_member() {
        local archive_dir="$1"
        local layers_file="$2"
        local member="$3"
        local output="$4"
        local layer dot_member
        dot_member="./$member"
        while IFS= read -r layer; do
            [[ -n "$layer" ]] || continue
            if grep -qxF "$member" < <(tar -tf "$archive_dir/$layer"); then
                tar -xOf "$archive_dir/$layer" "$member" >"$output"
                return 0
            fi
            if grep -qxF "$dot_member" < <(tar -tf "$archive_dir/$layer"); then
                tar -xOf "$archive_dir/$layer" "$dot_member" >"$output"
                return 0
            fi
        done <"$layers_file"
        return 1
    }

    member_exists_in_layers() {
        local archive_dir="$1"
        local layers_file="$2"
        local member="$3"
        local layer dot_member
        dot_member="./$member"
        while IFS= read -r layer; do
            [[ -n "$layer" ]] || continue
            if grep -qxF "$member" < <(tar -tf "$archive_dir/$layer"); then
                return 0
            fi
            if grep -qxF "$dot_member" < <(tar -tf "$archive_dir/$layer"); then
                return 0
            fi
        done <"$layers_file"
        return 1
    }
  '';

  claudeImage =
    (sandboxLib.mkSandbox {
      profile = sandboxLib.profiles.base;
      agent = "claude";
    }).image;

  defaultImageClosure = pkgs.closureInfo { rootPaths = [ defaultImage ]; };
  claudeImageClosure = pkgs.closureInfo { rootPaths = [ claudeImage ]; };

  # Closure over the actual base-image contents (built with linuxPkgs, matching
  # lib/sandbox/image.nix). The base image itself is a compressed tarball whose
  # store references are not scannable, so the membership verifier closes over
  # the contents list the image is built from.
  baseContents = import ../../lib/sandbox/base-contents.nix { pkgs = linuxPkgs; };
  baseContentsClosure = pkgs.closureInfo { rootPaths = baseContents; };

  claudeCodePkg = linuxPkgs.claude-code;
  piAgentPkg = linuxPkgs.pi-coding-agent;
  prekHooksBundle = import ../../lib/prek/bundle.nix { pkgs = linuxPkgs; };
  prekWrappers = import ../../lib/prek/wrappers.nix { pkgs = linuxPkgs; };

  # Linux-only shim verifier for the shared `imageLoadStep` snippet (the same
  # one `wrix spawn` runs). Asserts the skopeo-based install transport on
  # first call (per specs/sandbox.md § Image install path) and idempotence on
  # the second.
  wrixSpawnLoadTest = pkgs.writeShellApplication {
    name = "test-wrix-spawn-load";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          podman_log="$state/podman.log"
          skopeo_log="$state/skopeo.log"
          : >"$podman_log"
          : >"$skopeo_log"

          IMAGE_REF="localhost/wrix-loadtest:abc123"
          IMAGE_SOURCE="$tmp/image-descriptor.json"
          IMAGE_SOURCE_KIND="nix-descriptor"
          OCI_LAYOUT="$tmp/oci-layout"
          DESIRED_DIGEST="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
          # The install transport pins skopeo's containers-storage destination
          # to podman's store via `podman info`; the shim reports this spec so
          # the assertion below can verify the [driver@graphroot+runroot] ref.
          STORE_SPEC="overlay@$tmp/graphroot+$tmp/runroot"

          mkdir -p "$OCI_LAYOUT"
          jq -n \
            --arg digest "$DESIRED_DIGEST" \
            --arg layout "$OCI_LAYOUT" \
            '{schema:1,source_kind:"nix-descriptor",digest:$digest,oci_layout:$layout,oci_ref:"latest"}' \
            >"$IMAGE_SOURCE"

          cat >"$shim_dir/podman" <<PODMAN_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$podman_log'
          case "\$1" in
              image)
                  case "\$2" in
                      inspect)
                          if [[ -f '$state/loaded' ]]; then printf '%s\n' "\$5"; exit 0; else exit 1; fi
                          ;;
                      exists)
                          if [[ -f '$state/loaded' ]]; then exit 0; else exit 1; fi
                          ;;
                      *) exit 0 ;;
                  esac
                  ;;
              info)
                  printf '%s\n' '$STORE_SPEC'
                  exit 0
                  ;;
              tag)
                  : >'$state/loaded'
                  exit 0
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          cat >"$shim_dir/skopeo" <<SKOPEO_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$skopeo_log'
          for arg in "\$@"; do
              case "\$arg" in
                  oci:$OCI_LAYOUT:latest) ;;
                  containers-storage:*) : >'$state/loaded' ;;
                  nix:*|docker-archive:*|oci-archive:*)
                      echo 'archive or stock nix transport is not part of descriptor install' >&2
                      exit 2
                      ;;
              esac
          done
          exit 0
          SKOPEO_SHIM
          chmod +x "$shim_dir/skopeo"

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE IMAGE_SOURCE_KIND

          ${shellLib.imageLoadStep}

          EXPECTED_DEST="containers-storage:[$STORE_SPEC]$IMAGE_REF"
          if ! grep -qF -- "oci:$OCI_LAYOUT:latest $EXPECTED_DEST" "$skopeo_log"; then
              echo "first invocation did not copy descriptor OCI layout -> $EXPECTED_DEST:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if grep -qE '(^| )(nix:|(docker|oci)-archive:|load($| ))' "$skopeo_log" "$podman_log"; then
              echo "first invocation used a stock nix/archive/load transport:" >&2
              cat "$skopeo_log" >&2
              cat "$podman_log" >&2
              exit 1
          fi
          if ! grep -q "^tag $IMAGE_REF .*:latest$" "$podman_log"; then
              echo "first invocation did not tag $IMAGE_REF as :latest:" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          : >"$podman_log"
          : >"$skopeo_log"
          ${shellLib.imageLoadStep}

          if [[ -s "$skopeo_log" ]]; then
              echo "second invocation re-invoked skopeo (install is not idempotent):" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$podman_log"; then
              echo "second invocation did not preflight the descriptor digest $DESIRED_DIGEST:" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          echo "test-wrix-spawn-load: PASS"
        ''
      else
        ''
          echo "test-wrix-spawn-load: not available on Darwin (no podman dependency on macOS)" >&2
          exit 0
        '';
  };

  imageInstallArchivelessTest = pkgs.writeShellApplication {
    name = "test-image-install-archiveless";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          podman_log="$state/podman.log"
          skopeo_log="$state/skopeo.log"
          : >"$podman_log"
          : >"$skopeo_log"

          IMAGE_REF="localhost/wrix-archiveless:abc123"
          IMAGE_SOURCE="$tmp/image-descriptor.json"
          IMAGE_SOURCE_KIND="nix-descriptor"
          OCI_LAYOUT="$tmp/oci-layout"
          DESIRED_DIGEST="sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
          STORE_SPEC="overlay@$tmp/graphroot+$tmp/runroot"
          EXPECTED_DEST="containers-storage:[$STORE_SPEC]$IMAGE_REF"

          mkdir -p "$OCI_LAYOUT"
          jq -n \
            --arg digest "$DESIRED_DIGEST" \
            --arg layout "$OCI_LAYOUT" \
            '{schema:1,source_kind:"nix-descriptor",digest:$digest,oci_layout:$layout,oci_ref:"latest"}' \
            >"$IMAGE_SOURCE"

          cat >"$shim_dir/podman" <<PODMAN_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$podman_log'
          case "\$1" in
              image)
                  case "\$2" in
                      inspect) exit 1 ;;
                      exists) exit 1 ;;
                      *) exit 0 ;;
                  esac
                  ;;
              info)
                  printf '%s\n' '$STORE_SPEC'
                  exit 0
                  ;;
              tag) exit 0 ;;
              load)
                  echo 'podman load is not part of the archiveless descriptor path' >&2
                  exit 2
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          cat >"$shim_dir/skopeo" <<SKOPEO_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$skopeo_log'
          for arg in "\$@"; do
              case "\$arg" in
                  nix:*|docker-archive:*|oci-archive:*)
                      echo 'stock nix/archive transport is not part of the descriptor path' >&2
                      exit 2
                      ;;
                  containers-storage:*) : >'$state/installed' ;;
              esac
          done
          exit 0
          SKOPEO_SHIM
          chmod +x "$shim_dir/skopeo"

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE IMAGE_SOURCE_KIND

          ${shellLib.imageLoadStep}

          if [[ ! -f "$state/installed" ]]; then
              echo "descriptor install did not reach containers-storage" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qF -- "oci:$OCI_LAYOUT:latest $EXPECTED_DEST" "$skopeo_log"; then
              echo "descriptor install did not copy OCI layout -> $EXPECTED_DEST:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if grep -qE '(^| )(nix:|(docker|oci)-archive:|load($| ))' "$skopeo_log" "$podman_log"; then
              echo "descriptor install used a stock nix/archive/load transport:" >&2
              cat "$skopeo_log" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          echo "test-image-install-archiveless: PASS"
        ''
      else
        ''
          echo "test-image-install-archiveless: skipped on non-Linux host" >&2
          exit 0
        '';
  };

  # Linux-only integration verifier for the real packaged `skopeo` OCI layout
  # reader. The fast shim tests above prove command shape and idempotence; this
  # one catches invalid descriptor layouts without falling back to archives.
  imageInstallRealSkopeoTest = pkgs.writeShellApplication {
    name = "test-image-install-real-skopeo";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          podman_log="$state/podman.log"
          skopeo_log="$state/skopeo.log"
          : >"$podman_log"
          : >"$skopeo_log"

          IMAGE_REF="localhost/wrix-real-skopeo:abc123"
          IMAGE_SOURCE="$tmp/image-descriptor.json"
          IMAGE_SOURCE_KIND="nix-descriptor"
          OCI_LAYOUT="$tmp/oci-layout"
          STORE_SPEC="vfs@$tmp/graphroot+$tmp/runroot"

          make_empty_oci_layout() {
              local layout="$1"
              local config_json="$tmp/config.json"
              local manifest_json="$tmp/manifest.json"
              local config_digest config_size manifest_digest manifest_size
              mkdir -p "$layout/blobs/sha256"
              jq -n '{created:"1970-01-01T00:00:00Z",architecture:"amd64",os:"linux",config:{},rootfs:{type:"layers",diff_ids:[]},history:[]}' >"$config_json"
              config_digest=$(sha256sum "$config_json" | cut -d ' ' -f 1)
              config_size=$(wc -c <"$config_json" | tr -d '[:space:]')
              cp "$config_json" "$layout/blobs/sha256/$config_digest"
              jq -n \
                --arg digest "sha256:$config_digest" \
                --argjson size "$config_size" \
                '{schemaVersion:2,mediaType:"application/vnd.oci.image.manifest.v1+json",config:{mediaType:"application/vnd.oci.image.config.v1+json",digest:$digest,size:$size},layers:[]}' \
                >"$manifest_json"
              manifest_digest=$(sha256sum "$manifest_json" | cut -d ' ' -f 1)
              manifest_size=$(wc -c <"$manifest_json" | tr -d '[:space:]')
              cp "$manifest_json" "$layout/blobs/sha256/$manifest_digest"
              jq -n \
                --arg digest "sha256:$manifest_digest" \
                --argjson size "$manifest_size" \
                '{schemaVersion:2,mediaType:"application/vnd.oci.image.index.v1+json",manifests:[{mediaType:"application/vnd.oci.image.manifest.v1+json",digest:$digest,size:$size,annotations:{"org.opencontainers.image.ref.name":"latest"}}]}' \
                >"$layout/index.json"
              printf '{"imageLayoutVersion":"1.0.0"}\n' >"$layout/oci-layout"
              printf 'sha256:%s\n' "$config_digest"
          }

          DESIRED_DIGEST=$(make_empty_oci_layout "$OCI_LAYOUT")
          jq -n \
            --arg digest "$DESIRED_DIGEST" \
            --arg layout "$OCI_LAYOUT" \
            '{schema:1,source_kind:"nix-descriptor",digest:$digest,oci_layout:$layout,oci_ref:"latest"}' \
            >"$IMAGE_SOURCE"

          cat >"$shim_dir/podman" <<PODMAN_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$podman_log'
          case "\$1" in
              image)
                  case "\$2" in
                      inspect) exit 1 ;;
                      exists) exit 1 ;;
                      *) exit 0 ;;
                  esac
                  ;;
              info)
                  printf '%s\n' '$STORE_SPEC'
                  exit 0
                  ;;
              tag)
                  exit 0
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          cat >"$shim_dir/skopeo" <<SKOPEO_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$skopeo_log'
          saw_source=0
          saw_dest=0
          for arg in "\$@"; do
              case "\$arg" in
                  oci:$OCI_LAYOUT:latest) saw_source=1 ;;
                  containers-storage:*) saw_dest=1 ;;
                  nix:*|docker-archive:*|oci-archive:*)
                      echo 'stock nix/archive transport is not part of descriptor install' >&2
                      exit 2
                      ;;
              esac
          done
          if [[ "\$saw_source" == "1" && "\$saw_dest" == "1" ]]; then
              '${pkgs.skopeo}/bin/skopeo' inspect --raw "oci:$OCI_LAYOUT:latest" >/dev/null
              : >'$state/installed'
          fi
          exit 0
          SKOPEO_SHIM
          chmod +x "$shim_dir/skopeo"

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE IMAGE_SOURCE_KIND

          ${shellLib.imageLoadStep}

          if [[ ! -f "$state/installed" ]]; then
              echo "real-skopeo integration did not validate the OCI descriptor layout" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qF -- "oci:$OCI_LAYOUT:latest" "$skopeo_log"; then
              echo "real-skopeo integration did not use the descriptor OCI layout:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if grep -qE '(^| )(nix:|(docker|oci)-archive:)' "$skopeo_log"; then
              echo "real-skopeo integration used a stock nix/archive transport:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -q "^tag $IMAGE_REF .*:latest$" "$podman_log"; then
              echo "real-skopeo integration did not tag $IMAGE_REF as :latest:" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          echo "test-image-install-real-skopeo: PASS"
        ''
      else
        ''
          echo "test-image-install-real-skopeo: skipped on non-Linux host" >&2
          exit 0
        '';
  };

  digestSkipLauncher = if isLinux then serviceCli else null;

  # Linux-only verifier for the digest-preflight short-circuit (specs/sandbox.md
  # § Image install path; specs/image-builder.md Success Criteria #1). Drives
  # the live generated Linux launcher under both `wrix run` and `wrix spawn`,
  # with shim podman + skopeo binaries standing in for external runtimes.
  # The live launcher cases pre-seed the platform store's content digest;
  # the installer must short-circuit before source execution, skopeo copies,
  # tar materialization, or any `*-load` CLI call. Darwin's digest preflight
  # is verified separately by its own platform-resident verifier.
  imageInstallDigestSkipTest = pkgs.writeShellApplication {
    name = "test-image-install-digest-skip";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gawk
      pkgs.git
      pkgs.gnugrep
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          podman_log="$state/podman.log"
          skopeo_log="$state/skopeo.log"
          image_source_log="$state/image-source.log"
          : >"$podman_log"
          : >"$skopeo_log"
          : >"$image_source_log"

          IMAGE_REF="localhost/wrix-digestskip:abc123"
          IMAGE_SOURCE="$tmp/image-descriptor.json"
          IMAGE_SOURCE_KIND="nix-descriptor"
          IMAGE_DIGEST_PATH="$tmp/image-digest"
          OCI_LAYOUT="$tmp/oci-layout"
          DESIRED_DIGEST="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
          printf '%s' "$DESIRED_DIGEST" >"$IMAGE_DIGEST_PATH"
          mkdir -p "$OCI_LAYOUT"
          jq -n \
            --arg digest "$DESIRED_DIGEST" \
            --arg layout "$OCI_LAYOUT" \
            '{schema:1,source_kind:"nix-descriptor",digest:$digest,oci_layout:$layout,oci_ref:"latest"}' \
            >"$IMAGE_SOURCE"

          run_live_launcher_digest_hit() {
              local mode="$1"
              local live_state="$tmp/live-$mode"
              local live_home="$tmp/home-$mode"
              local workspace="$tmp/workspace-$mode"
              local live_source="$tmp/source-$mode"
              local profile_config="$tmp/profile-$mode.json"
              mkdir -p "$live_state" "$live_home" "$workspace"
              : >"$live_state/podman.log"
              : >"$live_state/skopeo.log"
              : >"$live_state/digest-present"
              cat >"$live_source" <<'LIVE_SOURCE'
          #!/usr/bin/env bash
          set -euo pipefail
          printf 'executed\n' >> "''${WRIX_DIGEST_SKIP_STATE:?}/source-executed"
          exit 99
          LIVE_SOURCE
              chmod +x "$live_source"
              jq -n \
                --arg source "$live_source" \
                --arg digest "$DESIRED_DIGEST" \
                '{
                  schema: 1,
                  system: "test",
                  profile: { name: "digestskip", env: {}, mounts: [], writable_dirs: [], network_allowlist: [] },
                  image: { ref: "localhost/wrix-digestskip:live", source: $source, source_kind: "nix-descriptor", digest: $digest },
                  agent: { kind: "direct" },
                  resources: { cpus: null, memory_mb: 4096, pids_limit: 4096 },
                  security: { deploy_key: null },
                  network: { default_mode: "open", ipv6: "disabled" },
                  services: { beads: { enable: "auto" }, nix_cache: { enable: false } },
                  features: { mcp_runtime: false }
                }' >"$profile_config"

              if [[ "$mode" == "run" ]]; then
                  HOME="$live_home" \
                  WRIX_DIGEST_SKIP_STATE="$live_state" \
                  WRIX_IMAGE_KEEP_FILE="$live_state/image-mru.json" \
                    ${digestSkipLauncher}/bin/wrix --profile-config "$profile_config" run "$workspace" true
              else
                  local deploy_key="$tmp/deploy-key-$mode"
                  local spawn_config="$tmp/spawn-$mode.json"
                  printf 'deploy key\n' >"$deploy_key"
                  jq -n --arg workspace "$workspace" \
                    '{workspace: $workspace, env: [], agent_args: ["true"], mounts: []}' \
                    >"$spawn_config"
                  HOME="$live_home" \
                  WRIX_DEPLOY_KEY="$deploy_key" \
                  WRIX_GIT_SIGN=0 \
                  WRIX_DIGEST_SKIP_STATE="$live_state" \
                  WRIX_IMAGE_KEEP_FILE="$live_state/image-mru.json" \
                    ${digestSkipLauncher}/bin/wrix --profile-config "$profile_config" spawn --spawn-config "$spawn_config"
              fi

              if [[ -s "$live_state/skopeo.log" ]]; then
                  echo "live $mode digest hit invoked skopeo:" >&2
                  cat "$live_state/skopeo.log" >&2
                  exit 1
              fi
              if [[ -e "$live_state/source-executed" ]]; then
                  echo "live $mode digest hit executed the image source" >&2
                  exit 1
              fi
              if [[ ! -f "$live_state/container-ran" ]]; then
                  echo "live $mode did not reach podman run after digest skip" >&2
                  cat "$live_state/podman.log" >&2
                  exit 1
              fi
              if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$live_state/podman.log"; then
                  echo "live $mode did not inspect the desired content digest:" >&2
                  cat "$live_state/podman.log" >&2
                  exit 1
              fi
              if grep -qE '(^| )load($| )' "$live_state/podman.log"; then
                  echo "live $mode issued a *-load CLI call:" >&2
                  cat "$live_state/podman.log" >&2
                  exit 1
              fi
          }
          # Runtime shims are shared by the Rust launcher live path above and
          # the shellLib imageLoadStep checks below. Each invocation selects
          # its state directory with WRIX_DIGEST_SKIP_STATE.
          cat >"$shim_dir/podman" <<'PODMAN_SHIM'
          #!/usr/bin/env bash
          set -euo pipefail
          state_dir="''${WRIX_DIGEST_SKIP_STATE:?}"
          mkdir -p "$state_dir/images"
          printf '%s\n' "$*" >>"$state_dir/podman.log"

          image_file() {
              local ref="$1"
              ref="''${ref//\//_}"
              ref="''${ref//:/_}"
              printf '%s/images/%s\n' "$state_dir" "$ref"
          }

          case "''${1:-}" in
              image)
                  case "''${2:-}" in
                      inspect)
                          target="''${!#}"
                          if [[ "$target" == sha256:* ]]; then
                              [[ -f "$state_dir/digest-present" || -f "$state_dir/installed" ]] || exit 1
                              printf '%s\n' "$target"
                              exit 0
                          fi
                          [[ -f "$(image_file "$target")" || -f "$state_dir/installed" ]] || exit 1
                          printf 'sha256:fake-image-id\n'
                          ;;
                      exists)
                          [[ ! -f "$state_dir/force-ref-miss" ]]
                          [[ -f "$(image_file "''${3:-}")" || -f "$state_dir/installed" ]]
                          ;;
                      *) exit 0 ;;
                  esac
                  ;;
              tag)
                  source="''${2:-}"
                  target="''${3:-}"
                  if [[ "$source" == sha256:* ]]; then
                      [[ -f "$state_dir/digest-present" || -f "$state_dir/installed" ]] || exit 1
                  elif [[ ! -f "$(image_file "$source")" && ! -f "$state_dir/installed" ]]; then
                      exit 1
                  fi
                  printf '%s\n' "$source" >"$(image_file "$target")"
                  ;;
              load)
                  : >"$state_dir/load-invoked"
                  ;;
              info)
                  printf 'overlay@%s/graph+%s/runroot\n' "$state_dir" "$state_dir"
                  ;;
              run)
                  : >"$state_dir/container-ran"
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          cat >"$shim_dir/skopeo" <<'SKOPEO_SHIM'
          #!/usr/bin/env bash
          set -euo pipefail
          state_dir="''${WRIX_DIGEST_SKIP_STATE:?}"
          mkdir -p "$state_dir"
          printf '%s\n' "$*" >>"$state_dir/skopeo.log"
          for a in "$@"; do
              case "$a" in
                  containers-storage:*) : >"$state_dir/installed" ;;
                  nix:*|docker-archive:*|oci-archive:*)
                      echo 'stock nix/archive transport is not part of descriptor install' >&2
                      exit 2
                      ;;
              esac
          done
          exit 0
          SKOPEO_SHIM
          chmod +x "$shim_dir/skopeo"

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          WRIX_DIGEST_SKIP_STATE="$state"
          export PATH WRIX_DIGEST_SKIP_STATE IMAGE_REF IMAGE_SOURCE IMAGE_SOURCE_KIND IMAGE_DIGEST_PATH

          run_live_launcher_digest_hit run
          run_live_launcher_digest_hit spawn

          # First invocation: digest preflight miss → install transport runs.
          ${shellLib.imageLoadStep}

          if [[ ! -f "$state/installed" ]]; then
              echo "first invocation did not reach the install transport" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qF -- "oci:$OCI_LAYOUT:latest" "$skopeo_log" || ! grep -qF -- "$IMAGE_REF" "$skopeo_log"; then
              echo "first invocation did not copy descriptor OCI layout into the selected image ref:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$podman_log"; then
              echo "first invocation did not perform a digest-preflight inspect of $DESIRED_DIGEST:" >&2
              cat "$podman_log" >&2
              exit 1
          fi
          first_source_lines=$(wc -l <"$image_source_log")
          if [[ "$first_source_lines" -ne 0 ]]; then
              echo "first invocation executed the descriptor source (got $first_source_lines calls):" >&2
              cat "$image_source_log" >&2
              exit 1
          fi

          : >"$podman_log"
          : >"$skopeo_log"

          # Second invocation: digest preflight hit → short-circuit.
          ${shellLib.imageLoadStep}

          if [[ -s "$skopeo_log" ]]; then
              echo "second invocation re-invoked skopeo (digest preflight did not short-circuit):" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if [[ -e "$state/load-invoked" ]]; then
              echo "second invocation issued a *-load CLI call (expected none):" >&2
              exit 1
          fi
          second_source_lines=$(wc -l <"$image_source_log")
          if [[ "$second_source_lines" -ne 0 ]]; then
              echo "second invocation executed the descriptor source (lines now=$second_source_lines, expected 0):" >&2
              cat "$image_source_log" >&2
              exit 1
          fi
          if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$podman_log"; then
              echo "second invocation did not perform a digest-preflight inspect of $DESIRED_DIGEST:" >&2
              cat "$podman_log" >&2
              exit 1
          fi
          if grep -qE '^load($| )' "$podman_log"; then
              echo "second invocation logged a podman load command (expected none):" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          : >"$podman_log"
          : >"$skopeo_log"
          : >"$state/force-ref-miss"
          IMAGE_DIGEST_PATH=""

          ${shellLib.imageLoadStep}

          rm -f "$state/force-ref-miss"
          if [[ -s "$skopeo_log" ]]; then
              echo "descriptor-derived digest preflight re-invoked skopeo:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$podman_log"; then
              echo "descriptor-derived digest preflight did not inspect $DESIRED_DIGEST:" >&2
              cat "$podman_log" >&2
              exit 1
          fi
          descriptor_source_lines=$(wc -l <"$image_source_log")
          if [[ "$descriptor_source_lines" -ne 0 ]]; then
              echo "descriptor-derived digest preflight executed the descriptor source (lines=$descriptor_source_lines):" >&2
              cat "$image_source_log" >&2
              exit 1
          fi

          echo "test-image-install-digest-skip: PASS"
        ''
      else
        ''
          # Darwin's digest-preflight short-circuit lives in lib/sandbox/darwin/default.nix
          # and is exercised by its own platform-resident verifier; the shared
          # `imageLoadStep` snippet driven here is Linux-only (podman + skopeo).
          echo "test-image-install-digest-skip: skipped on this platform (Linux-only shim)" >&2
          exit 0
        '';
  };

  digestMatchesStoredIdTest = pkgs.writeShellApplication {
    name = "test-image-digest-matches-stored-id";
    text = ''
      echo "test-image-digest-matches-stored-id: skipped; descriptor digest coverage lives in image-install-digest-skip" >&2
      exit 0
    '';
  };

  linuxImageArchivelessSourceTest = pkgs.writeShellApplication {
    name = "test-linux-image-archiveless-source";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.jq
      pkgs.nix
    ];
    text =
      if isLinux then
        ''
          source_kind='${defaultImage.source_kind}'
          source_path='${defaultImage.source}'
          image_path='${discardContext defaultImage}'
          digest_source_kind='${defaultImage.digest_source_kind}'

          if [[ "$source_kind" != "nix-descriptor" ]]; then
              echo "FAIL: expected source_kind=nix-descriptor, got $source_kind" >&2
              exit 1
          fi
          if [[ "$source_path" = "$image_path" ]]; then
              echo "FAIL: Linux image.source still points at the raw image output" >&2
              exit 1
          fi
          if [[ "$source_path" != *-nix-descriptor.json ]]; then
              echo "FAIL: Linux image.source is not a descriptor path: $source_path" >&2
              exit 1
          fi
          if [[ "$digest_source_kind" != "nix-descriptor" ]]; then
              echo "FAIL: Linux image digest is not descriptor-derived: $digest_source_kind" >&2
              exit 1
          fi
          legacy_stream_key="fallback_""stream"
          if jq -e --arg key "$legacy_stream_key" 'has($key)' "$source_path" >/dev/null; then
              echo "FAIL: Linux descriptor still exposes legacy stream fallback metadata" >&2
              exit 1
          fi
          if ! jq -e '
              type == "object" and
              .schema == 1 and
              .source_kind == "nix-descriptor" and
              (.digest | test("^sha256:[0-9a-f]{64}$")) and
              (.oci_manifest.media_type == "application/vnd.oci.image.manifest.v1+json") and
              (.oci_manifest.config.digest == .digest) and
              ((.layers // []) | length > 0) and
              all(.layers[]; (.digest | test("^sha256:[0-9a-f]{64}$")) and (.diff_id | test("^sha256:[0-9a-f]{64}$")) and (.size | type == "number"))
            ' "$source_path" >/dev/null; then
              echo "FAIL: Linux descriptor JSON does not expose the expected manifest/config/layer contract" >&2
              jq . "$source_path" >&2
              exit 1
          fi
          oci_layout=$(jq -r '.oci_layout // ""' "$source_path")
          oci_ref=$(jq -r '.oci_ref // ""' "$source_path")
          layer_count=$(jq -r '(.layers // []) | length' "$source_path")
          if [[ "$oci_layout" != /nix/store/*-oci ]]; then
              echo "FAIL: Linux descriptor does not point at an OCI layout: $oci_layout" >&2
              exit 1
          fi
          if [[ "$oci_layout" == "$source_path" ]]; then
              echo "FAIL: Linux descriptor source path is the OCI layout path" >&2
              exit 1
          fi
          if [[ "$oci_ref" != "latest" ]]; then
              echo "FAIL: Linux descriptor has unexpected OCI ref: $oci_ref" >&2
              exit 1
          fi
          if [[ "$layer_count" -lt 1 ]]; then
              echo "FAIL: Linux descriptor has no layer descriptors" >&2
              exit 1
          fi

          assert_no_deriver_reference() {
              local label="$1"
              local path="$2"
              local forbidden="$3"
              local deriver
              deriver=$(nix-store -q --deriver "$path")
              if [[ "$deriver" != "unknown-deriver" ]] && nix-store -q --references "$deriver" | grep -Fxq "$forbidden"; then
                  echo "FAIL: $label builder depends on the raw whole-image output: $forbidden" >&2
                  exit 1
              fi
          }
          assert_no_deriver_reference "descriptor source" "$source_path" "$image_path"
          assert_no_deriver_reference "descriptor OCI layout" "$oci_layout" "$image_path"

          echo "test-linux-image-archiveless-source: PASS"
        ''
      else
        ''
          echo "test-linux-image-archiveless-source: skipped on non-Linux host" >&2
          exit 0
        '';
  };

  imageDigestNoTarTest = pkgs.writeShellApplication {
    name = "test-image-digest-no-tar";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.nix
    ];
    text =
      if isLinux then
        ''
          digest_path='${defaultImage.digest}'
          image_path='${discardContext defaultImage}'
          digest=$(cat "$digest_path")

          if [[ ! "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
              echo "FAIL: descriptor digest has unexpected shape: $digest" >&2
              exit 1
          fi
          if nix-store -q --references "$digest_path" | grep -Fxq "$image_path"; then
              echo "FAIL: descriptor digest references the raw image output" >&2
              exit 1
          fi
          digest_root="''${digest_path%/wrix/config-digest}"
          digest_deriver=$(nix-store -q --deriver "$digest_root")
          if [[ "$digest_deriver" != "unknown-deriver" ]] && nix-store -q --references "$digest_deriver" | grep -Fxq "$image_path"; then
              echo "FAIL: descriptor digest builder depends on the raw whole-image output" >&2
              exit 1
          fi

          echo "test-image-digest-no-tar: PASS ($digest)"
        ''
      else
        ''
          echo "test-image-digest-no-tar: skipped on non-Linux host" >&2
          exit 0
        '';
  };

  imageTierGraphTest = pkgs.writeShellApplication {
    name = "test-image-tier-graph";
    text = ''
      source_kind='${defaultImage.source_kind}'
      base='${discardContext defaultImage.baseImage}'
      stable='${discardContext defaultImage.stableProfileImage}'
      agent='${discardContext defaultImage.agentImage}'
      leaf='${discardContext defaultImage}'

      if [[ "$source_kind" != "${expectedImageSourceKind}" ]]; then
          echo "FAIL: expected source_kind=${expectedImageSourceKind}, got $source_kind" >&2
          exit 1
      fi
      for tier in "$base" "$stable" "$agent" "$leaf"; do
          if [[ "$tier" != /nix/store/* ]]; then
              echo "FAIL: tier path is not store-resident: $tier" >&2
              exit 1
          fi
      done
      if [[ "$stable" != *wrix-stable-profile-* || "$agent" != *wrix-agent-* || "$leaf" != *wrix-* ]]; then
          echo "FAIL: tier names do not expose base -> stable-profile -> agent -> leaf graph" >&2
          exit 1
      fi

      echo "test-image-tier-graph: PASS"
    '';
  };

  imageNixConfigTest = pkgs.writeShellApplication {
    name = "test-image-nix-config";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gawk
      pkgs.gnugrep
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          ${archiveShellHelpers}

          extract_nix_conf() {
              local archive_dir="$1"
              local layers_file="$2"
              local output="$3"
              local layer member
              while IFS= read -r layer; do
                  [[ -n "$layer" ]] || continue
                  member=$(awk '/^\/?nix\/store\/[a-z0-9]{32}-nix\.conf\/etc\/nix\/nix\.conf$/ { print; exit }' \
                      < <(tar -tf "$archive_dir/$layer"))
                  if [[ -n "$member" ]]; then
                      tar -xOf "$archive_dir/$layer" "$member" >"$output"
                      return 0
                  fi
              done <"$layers_file"
              return 1
          }

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          unpack_archive "stable-profile" "${defaultImage.stableProfileImage}" "$tmp/stable"
          write_unique_layers "$tmp/stable" "$tmp/stable.layers"
          if ! extract_nix_conf "$tmp/stable" "$tmp/stable.layers" "$tmp/nix.conf"; then
              echo "FAIL: /etc/nix/nix.conf target is absent from the stable profile image" >&2
              exit 1
          fi

          for expected in \
              "experimental-features = nix-command flakes" \
              "sandbox = false"; do
              if ! grep -qxF "$expected" "$tmp/nix.conf"; then
                  echo "FAIL: nix.conf missing exact setting: $expected" >&2
                  cat "$tmp/nix.conf" >&2
                  exit 1
              fi
          done

          echo "test-image-nix-config: PASS"
        ''
      else
        ''
          echo "test-image-nix-config: skipped on this platform (Linux-only image archive verifier)" >&2
          exit 0
        '';
  };

  imageCaCertificatesTest = pkgs.writeShellApplication {
    name = "test-image-ca-certificates";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gawk
      pkgs.gnugrep
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          ${archiveShellHelpers}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          unpack_archive "leaf" "${defaultImage}" "$tmp/leaf"
          write_unique_layers "$tmp/leaf" "$tmp/leaf.layers"
          config_file=$(jq -r '.[0].Config' "$tmp/leaf/manifest.json")
          expected_env="SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"

          if ! jq -e --arg expected "$expected_env" '(.config.Env // []) | index($expected) != null' "$tmp/leaf/$config_file" >/dev/null; then
              echo "FAIL: image config Env does not set $expected_env" >&2
              jq '.config.Env // []' "$tmp/leaf/$config_file" >&2
              exit 1
          fi
          if ! member_exists_in_layers "$tmp/leaf" "$tmp/leaf.layers" "etc/ssl/certs/ca-bundle.crt"; then
              echo "FAIL: SSL_CERT_FILE target /etc/ssl/certs/ca-bundle.crt is absent from image layers" >&2
              exit 1
          fi
          if ! grep -qxF "${linuxPkgs.cacert}" "${defaultImageClosure}/store-paths"; then
              echo "FAIL: pkgs.cacert is absent from the default image closure" >&2
              echo "  expected: ${linuxPkgs.cacert}" >&2
              exit 1
          fi

          echo "test-image-ca-certificates: PASS"
        ''
      else
        ''
          echo "test-image-ca-certificates: skipped on this platform (Linux-only image archive verifier)" >&2
          exit 0
        '';
  };

  imageEntrypointCommandTest = pkgs.writeShellApplication {
    name = "test-image-entrypoint-command";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gawk
      pkgs.gnugrep
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          ${archiveShellHelpers}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          unpack_archive "leaf" "${defaultImage}" "$tmp/leaf"
          write_unique_layers "$tmp/leaf" "$tmp/leaf.layers"
          config_file=$(jq -r '.[0].Config' "$tmp/leaf/manifest.json")

          if ! jq -e '.config.Entrypoint == ["/entrypoint.sh"]' "$tmp/leaf/$config_file" >/dev/null; then
              echo "FAIL: image config Entrypoint is not /entrypoint.sh" >&2
              jq '.config.Entrypoint' "$tmp/leaf/$config_file" >&2
              exit 1
          fi
          if ! extract_layer_member "$tmp/leaf" "$tmp/leaf.layers" "entrypoint.sh" "$tmp/entrypoint.sh"; then
              echo "FAIL: /entrypoint.sh is absent from image layers" >&2
              exit 1
          fi

          expected_entrypoint='${../../lib/sandbox/linux/entrypoint.sh}'
          actual_hash=$(sha256sum "$tmp/entrypoint.sh" | cut -d ' ' -f 1)
          expected_hash=$(sha256sum "$expected_entrypoint" | cut -d ' ' -f 1)
          if [[ "$actual_hash" != "$expected_hash" ]]; then
              echo "FAIL: /entrypoint.sh content does not match the selected Linux entrypoint source" >&2
              echo "  actual  : $actual_hash" >&2
              echo "  expected: $expected_hash" >&2
              exit 1
          fi

          echo "test-image-entrypoint-command: PASS"
        ''
      else
        ''
          echo "test-image-entrypoint-command: skipped on this platform (Linux-only image archive verifier)" >&2
          exit 0
        '';
  };

  sourceKindMatrix = {
    base = {
      inherit ((sandboxLib.mkSandbox { profile = sandboxLib.profiles.base; })) image;
      expected_kind = expectedImageSourceKind;
    };
    rust = {
      inherit ((sandboxLib.mkSandbox { profile = sandboxLib.profiles.rust; })) image;
      expected_kind = expectedImageSourceKind;
    };
    python = {
      inherit ((sandboxLib.mkSandbox { profile = sandboxLib.profiles.python; })) image;
      expected_kind = expectedImageSourceKind;
    };
    base-pi = {
      inherit
        (
          (sandboxLib.mkSandbox {
            profile = sandboxLib.profiles.base;
            agent = "pi";
          })
        )
        image
        ;
      expected_kind = expectedImageSourceKind;
    };
    base-claude = {
      image = claudeImage;
      expected_kind = expectedImageSourceKind;
    };
    wrix-service = {
      image = serviceImage;
      expected_kind = expectedImageSourceKind;
    };
    wrix-builder = {
      image = builderImage;
      expected_kind = expectedImageSourceKind;
    };
  };
  imageAgentMarkerMatrix = {
    direct = sourceKindMatrix.base.image;
    claude = sourceKindMatrix.base-claude.image;
    pi = sourceKindMatrix.base-pi.image;
  };
  imageAgentMarkerChecks = concatStringsSep "\n" (
    mapAttrsToList (agent: image: ''
      check_agent_marker "${agent}" "${image}"
    '') imageAgentMarkerMatrix
  );
  imageAgentMarkerTest = pkgs.writeShellApplication {
    name = "test-image-agent-marker";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          ${archiveShellHelpers}

          check_agent_marker() {
              local expected_agent="$1"
              local image="$2"
              local image_dir="$tmp/$expected_agent"
              local marker_file="$tmp/$expected_agent-marker"
              local marker
              unpack_archive "$expected_agent" "$image" "$image_dir"
              write_unique_layers "$image_dir" "$image_dir.layers"
              if ! extract_layer_member "$image_dir" "$image_dir.layers" "etc/wrix/image-agent" "$marker_file"; then
                  echo "FAIL: $expected_agent image does not declare /etc/wrix/image-agent" >&2
                  exit 1
              fi
              marker=$(<"$marker_file")
              if [[ "$marker" != "$expected_agent" ]]; then
                  echo "FAIL: $expected_agent image declares agent '$marker'" >&2
                  exit 1
              fi
          }

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          ${imageAgentMarkerChecks}

          echo "test-image-agent-marker: PASS"
        ''
      else
        ''
          echo "test-image-agent-marker: skipped on this platform (Linux-only image archive verifier)" >&2
          exit 0
        '';
  };

  sourceKindChecks = concatStringsSep "\n" (
    mapAttrsToList (name: entry: ''
      check_source_kind "${name}" "${entry.expected_kind}" "${entry.image.source_kind}" "${toString entry.image.source}" "${discardContext entry.image}"
    '') sourceKindMatrix
  );

  wrixImagesSourceKindTest = pkgs.writeShellApplication {
    name = "test-wrix-images-source-kind";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ]
    ++ optionals isLinux [
      pkgs.jq
      pkgs.nix
    ];
    text = ''
      check_source_kind() {
          local name="$1"
          local expected_kind="$2"
          local source_kind="$3"
          local source_path="$4"
          local image_path="$5"
          if [[ "$source_kind" != "$expected_kind" ]]; then
              echo "FAIL: $name source_kind=$source_kind, expected $expected_kind" >&2
              exit 1
          fi
          if [[ "$source_path" != /nix/store/* ]]; then
              echo "FAIL: $name source path is not store-resident: $source_path" >&2
              exit 1
          fi
          if [[ "$source_kind" == "nix-descriptor" ]]; then
              local legacy_stream_key oci_layout oci_ref
              legacy_stream_key="fallback_""stream"
              if jq -e --arg key "$legacy_stream_key" 'has($key)' "$source_path" >/dev/null; then
                  echo "FAIL: $name descriptor exposes legacy stream fallback metadata" >&2
                  exit 1
              fi
              if ! jq -e '
                  type == "object" and
                  .schema == 1 and
                  .source_kind == "nix-descriptor" and
                  (.digest | test("^sha256:[0-9a-f]{64}$")) and
                  (.oci_manifest.media_type == "application/vnd.oci.image.manifest.v1+json") and
                  (.oci_manifest.config.digest == .digest) and
                  ((.layers // []) | length > 0) and
                  all(.layers[]; (.digest | test("^sha256:[0-9a-f]{64}$")) and (.diff_id | test("^sha256:[0-9a-f]{64}$")) and (.size | type == "number"))
                ' "$source_path" >/dev/null; then
                  echo "FAIL: $name descriptor JSON does not expose the expected manifest/config/layer contract" >&2
                  jq . "$source_path" >&2
                  exit 1
              fi
              oci_layout=$(jq -r '.oci_layout // ""' "$source_path")
              oci_ref=$(jq -r '.oci_ref // ""' "$source_path")
              if [[ "$oci_layout" != /nix/store/*-oci ]]; then
                  echo "FAIL: $name descriptor does not point at an OCI layout: $oci_layout" >&2
                  exit 1
              fi
              if [[ "$oci_layout" == "$source_path" ]]; then
                  echo "FAIL: $name descriptor source path is the OCI layout path" >&2
                  exit 1
              fi
              if [[ -z "$oci_ref" ]]; then
                  echo "FAIL: $name descriptor has no OCI ref" >&2
                  exit 1
              fi
              local source_deriver oci_deriver
              source_deriver=$(nix-store -q --deriver "$source_path")
              if [[ "$source_deriver" != "unknown-deriver" ]] && nix-store -q --references "$source_deriver" | grep -Fxq "$image_path"; then
                  echo "FAIL: $name descriptor source builder depends on the raw whole-image output" >&2
                  exit 1
              fi
              oci_deriver=$(nix-store -q --deriver "$oci_layout")
              if [[ "$oci_deriver" != "unknown-deriver" ]] && nix-store -q --references "$oci_deriver" | grep -Fxq "$image_path"; then
                  echo "FAIL: $name descriptor OCI layout builder depends on the raw whole-image output" >&2
                  exit 1
              fi
          fi
      }

      ${sourceKindChecks}

      builder_ref='${builderImage.ref}'
      builder_digest_path='${toString builderImage.digest}'
      if [[ "$builder_ref" != ${if isLinux then "localhost/wrix-builder:" else "wrix-builder:"}* ]]; then
          echo "FAIL: wrix-builder ref does not follow the platform ref contract: $builder_ref" >&2
          exit 1
      fi
      if [[ "$builder_digest_path" != /nix/store/* ]]; then
          echo "FAIL: wrix-builder digest path is not store-resident: $builder_digest_path" >&2
          exit 1
      fi
      if ! grep -Eq '^sha256:[0-9a-f]{64}$' "$builder_digest_path"; then
          echo "FAIL: wrix-builder digest file does not contain a sha256 digest" >&2
          exit 1
      fi

      echo "test-wrix-images-source-kind: PASS"
    '';
  };

  imageLabelMatrix = {
    base = {
      image = sourceKindMatrix.base.image;
      kind = "profile";
      profile = "base";
      agent = "direct";
    };
    rust = {
      image = sourceKindMatrix.rust.image;
      kind = "profile";
      profile = "rust";
      agent = "direct";
    };
    python = {
      image = sourceKindMatrix.python.image;
      kind = "profile";
      profile = "python";
      agent = "direct";
    };
    base-pi = {
      image = sourceKindMatrix.base-pi.image;
      kind = "profile";
      profile = "base";
      agent = "pi";
    };
    base-claude = {
      image = sourceKindMatrix.base-claude.image;
      kind = "profile";
      profile = "base";
      agent = "claude";
    };
    wrix-service = {
      image = serviceImage;
      kind = "service";
    };
    wrix-builder = {
      image = builderImage;
      kind = "builder";
    };
  };
  imageLabelChecks = concatStringsSep "\n" (
    mapAttrsToList (
      name: expected:
      let
        labels = expected.image.labels or (throw "${name} image labels are missing");
      in
      ''
        check_label "${name}" "wrix.managed" "${labels."wrix.managed" or ""}" "true"
        check_label "${name}" "wrix.image.kind" "${labels."wrix.image.kind" or ""}" "${expected.kind}"
      ''
      + optionalString (expected.kind == "profile") ''
        check_label "${name}" "wrix.profile.name" "${
          labels."wrix.profile.name" or ""
        }" "${expected.profile}"
        check_label "${name}" "wrix.agent.kind" "${labels."wrix.agent.kind" or ""}" "${expected.agent}"
      ''
    ) imageLabelMatrix
  );
  descriptorLabelChecks = concatStringsSep "\n" (
    optionals isLinux (
      mapAttrsToList (
        name: entry:
        let
          inherit (entry) image;
          inherit (image) labels;
          source = toString image.source;
        in
        ''
          check_descriptor_label "${name}" "${source}" "wrix.managed" "${labels."wrix.managed"}"
          check_descriptor_label "${name}" "${source}" "wrix.image.kind" "${labels."wrix.image.kind"}"
        ''
        + optionalString (hasAttr "wrix.profile.name" labels) ''
          check_descriptor_label "${name}" "${source}" "wrix.profile.name" "${labels."wrix.profile.name"}"
        ''
        + optionalString (hasAttr "wrix.agent.kind" labels) ''
          check_descriptor_label "${name}" "${source}" "wrix.agent.kind" "${labels."wrix.agent.kind"}"
        ''
      ) sourceKindMatrix
    )
  );
  wrixImageLabelsTest = pkgs.writeShellApplication {
    name = "test-wrix-image-labels";
    runtimeInputs = [ pkgs.jq ];
    text = ''
      check_label() {
          local name="$1"
          local key="$2"
          local actual="$3"
          local expected="$4"
          if [[ -z "$actual" || "$actual" != "$expected" ]]; then
              echo "FAIL: $name label $key=$actual, expected $expected" >&2
              exit 1
          fi
      }

      check_descriptor_label() {
          local name="$1"
          local source_path="$2"
          local key="$3"
          local expected="$4"
          local actual
          actual=$(jq -r --arg key "$key" '.config.labels[$key] // empty' "$source_path")
          if [[ "$actual" != "$expected" ]]; then
              echo "FAIL: $name descriptor label $key=$actual, expected $expected" >&2
              exit 1
          fi
      }

      ${imageLabelChecks}
      ${descriptorLabelChecks}

      echo "test-wrix-image-labels: PASS"
    '';
  };

  agentDirectRunnerTest = pkgs.writeShellApplication {
    name = "test-agent-direct-runner";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${defaultImageClosure}/store-paths

      if ! grep -Eq '/nix/store/[a-z0-9]{32}-loom-direct-runner$' "$closure_file"; then
          echo "FAIL: default agent=direct image closure does not contain loom-direct-runner" >&2
          echo "  closure: $closure_file" >&2
          exit 1
      fi

      echo "test-agent-direct-runner: PASS"
    '';
  };

  agentClaudeRuntimeTest = pkgs.writeShellApplication {
    name = "test-agent-claude-runtime";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${claudeImageClosure}/store-paths
      claude_code_path=${claudeCodePkg}

      if ! grep -qxF "$claude_code_path" "$closure_file"; then
          echo "FAIL: claude-code missing from claude sandbox closure" >&2
          echo "  expected: $claude_code_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      echo "test-agent-claude-runtime: PASS"
    '';
  };

  claudeRuntimeNoopTest = pkgs.writeShellApplication {
    name = "test-claude-runtime-noop";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${claudeImageClosure}/store-paths
      claude_code_path=${claudeCodePkg}

      if ! grep -qxF "$claude_code_path" "$closure_file"; then
          echo "FAIL: claude-code missing from claude sandbox closure" >&2
          echo "  expected: $claude_code_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      echo "test-claude-runtime-noop: PASS"
    '';
  };

  prekSurfaceImageMatrix = {
    base-direct = sourceKindMatrix.base.image;
    rust-direct = sourceKindMatrix.rust.image;
    python-direct = sourceKindMatrix.python.image;
    base-claude = sourceKindMatrix.base-claude.image;
    rust-claude =
      (sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.rust;
        agent = "claude";
      }).image;
    python-claude =
      (sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.python;
        agent = "claude";
      }).image;
    base-pi = sourceKindMatrix.base-pi.image;
    rust-pi =
      (sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.rust;
        agent = "pi";
      }).image;
    python-pi =
      (sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.python;
        agent = "pi";
      }).image;
  };
  prekSurfaceChecks = concatStringsSep "\n" (
    mapAttrsToList (
      name: image:
      let
        closure = pkgs.closureInfo { rootPaths = [ image ]; };
      in
      ''
        check_surface "${name}" "${closure}/store-paths" "wrix.prekHooks" "${prekHooksBundle}"
        check_surface "${name}" "${closure}/store-paths" "wrix.prePushChecks" "${prekWrappers.prePushChecks}"
        check_surface "${name}" "${closure}/store-paths" "wrix.skipIfMissing" "${prekWrappers.skipIfMissing}"
      ''
    ) prekSurfaceImageMatrix
  );
  prekHooksClosureTest = pkgs.writeShellApplication {
    name = "test-prek-hooks-closure";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      check_surface() {
          local image_name="$1"
          local closure_file="$2"
          local surface_name="$3"
          local surface_path="$4"
          if ! grep -qxF "$surface_path" "$closure_file"; then
              echo "FAIL: $surface_name not in $image_name profile image closure" >&2
              echo "  expected: $surface_path" >&2
              echo "  closure : $closure_file" >&2
              exit 1
          fi
      }

      ${prekSurfaceChecks}

      echo "test-prek-hooks-closure: PASS"
    '';
  };

  # Membership guard for the universal bottom-of-closure (specs/image-builder.md
  # § Base Image Layering). The base holds only nixpkgs-pin-dependent paths that
  # every profile actually closes over. The base profile is the minimal package
  # set (every other profile adds to it), so its image closure is the universal
  # lower bound: a base member is genuinely shared iff it is reachable there.
  # The fromImage base tar is a compressed blob whose store refs are unscannable,
  # so a member that no profile references — a profile-specific toolchain such as
  # `pkgs.rustc` (rust uses fenix's toolchain; base/python carry no Rust) or
  # `pkgs.llvmPackages.libllvm` (no profile links LLVM) — is absent from the
  # scannable image closure and is caught here as dead weight.
  baseContentsList = concatStringsSep " " (map (p: ''"${p}"'') baseContents);
  baseImageUniversalTest = pkgs.writeShellApplication {
    name = "test-base-image-universal";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      image_closure=${defaultImageClosure}/store-paths
      base_contents=${baseContentsClosure}/store-paths

      # Universality: every base member must be reachable from the base profile's
      # own packages, else it is dead weight loaded into every image's base layer.
      members=(${baseContentsList})
      for member in "''${members[@]}"; do
          if ! grep -qxF "$member" "$image_closure"; then
              echo "FAIL: wrix-base-image member not shared by the base profile closure" >&2
              echo "  member : $member" >&2
              echo "  closure: $image_closure" >&2
              exit 1
          fi
      done

      # Exclusion: no profile references rustc or libllvm, so neither is a base member.
      for toolchain in ${linuxPkgs.rustc} ${linuxPkgs.llvmPackages.libllvm}; do
          if grep -qxF "$toolchain" "$base_contents"; then
              echo "FAIL: profile-specific toolchain present in wrix-base-image contents" >&2
              echo "  unexpected: $toolchain" >&2
              exit 1
          fi
      done

      echo "test-base-image-universal: PASS"
    '';
  };

  entrypointResolverBaseTest = pkgs.writeShellApplication {
    name = "test-entrypoint-resolver-base";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gnutar
      pkgs.jq
    ];
    text = ''
      image_closure=${defaultImageClosure}/store-paths
      base_contents=${baseContentsClosure}/store-paths
      resolver=${linuxPkgs.getent.provider}

      if [[ ! -x "$resolver/bin/getent" ]]; then
          echo "FAIL: pkgs.getent.provider does not provide bin/getent as expected" >&2
          echo "  resolver: $resolver" >&2
          exit 1
      fi

      if ! grep -qxF "$resolver" "$base_contents"; then
          echo "FAIL: getent provider missing from wrix-base-image contents" >&2
          echo "  expected: $resolver" >&2
          echo "  base    : $base_contents" >&2
          exit 1
      fi

      if ! grep -qxF "$resolver" "$image_closure"; then
          echo "FAIL: getent provider missing from default sandbox image closure" >&2
          echo "  expected: $resolver" >&2
          echo "  closure : $image_closure" >&2
          exit 1
      fi

      tmp=$(mktemp -d)
      trap 'rm -rf "$tmp"' EXIT
      ${defaultImage} > "$tmp/image.tar"
      tar -xf "$tmp/image.tar" -C "$tmp"

      jq -r '.[0].Layers[]' "$tmp/manifest.json" > "$tmp/layers"

      getent_target=""
      while IFS= read -r layer; do
          line=$(tar -tvf "$tmp/$layer" | grep -E '(^| )\.?/bin/getent -> /nix/store/' || true)
          if [[ -n "$line" ]]; then
              getent_target=''${line##* -> }
          fi
      done < "$tmp/layers"

      if [[ -z "$getent_target" ]]; then
          echo "FAIL: composed image does not expose /bin/getent" >&2
          exit 1
      fi
      if [[ "$getent_target" != /nix/store/* ]]; then
          echo "FAIL: /bin/getent does not point into /nix/store: $getent_target" >&2
          exit 1
      fi

      target_member="''${getent_target#/}"
      target_present=0
      while IFS= read -r layer; do
          if tar -tf "$tmp/$layer" | grep -qxF "$target_member" \
              || tar -tf "$tmp/$layer" | grep -qxF "$getent_target"; then
              target_present=1
              break
          fi
      done < "$tmp/layers"
      if [[ "$target_present" != "1" ]]; then
          echo "FAIL: /bin/getent target is absent from image layers" >&2
          echo "  /bin/getent -> $getent_target" >&2
          exit 1
      fi

      echo "test-entrypoint-resolver-base: PASS"
    '';
  };

  # Hash-stability guard for `wrix-base-image` (specs/image-builder.md
  # § Base Image Layering). The base captures only the nixpkgs-pin-dependent
  # bottom-of-closure, so its derivation hash must not move when any
  # profile-level input changes. We build the full sandbox image under several
  # perturbations of profile.packages, profile.env, MCP configs, the merged
  # Claude settings JSON, and the agent runtime selection, then read each
  # image's chained `fromImage` base (image.baseImage) and assert one drvPath
  # across all of them. A regression that threads a profile input into
  # base-image.nix would split the drvPaths and fail here.
  baseImageOf = args: (sandboxLib.mkSandbox args).image.baseImage.drvPath;
  referenceBaseImage = baseImageOf { profile = sandboxLib.profiles.base; };
  baseImagePermutations = {
    packages = baseImageOf {
      profile = sandboxLib.profiles.base;
      packages = [ linuxPkgs.hello ];
    };
    env = baseImageOf {
      profile = sandboxLib.profiles.base;
      env = {
        WRIX_HASH_STABLE_PROBE = "v2";
      };
    };
    mcpConfigs = baseImageOf {
      profile = sandboxLib.profiles.base;
      mcpRuntime = true;
    };
    claudeSettings = baseImageOf {
      profile = sandboxLib.profiles.base;
      agent = "claude";
      agentSettings.env.ANTHROPIC_MODEL = "claude-hash-stable-probe";
    };
    agentDirect = baseImageOf {
      profile = sandboxLib.profiles.base;
      agent = "direct";
      agentPkg = linuxPkgs.hello;
    };
    profilePython = baseImageOf { profile = sandboxLib.profiles.python; };
  };
  baseImageHashStableChecks = concatStringsSep "\n" (
    mapAttrsToList (name: drvPath: ''
      if [[ "${drvPath}" != "$reference" ]]; then
          echo "FAIL: wrix-base-image drvPath changed under profile perturbation '${name}'" >&2
          echo "  reference  : $reference" >&2
          echo "  ${name}: ${drvPath}" >&2
          status=1
      fi
    '') baseImagePermutations
  );
  baseImageHashStableTest = pkgs.writeShellApplication {
    name = "test-base-image-hash-stable";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      reference="${referenceBaseImage}"
      status=0

      ${baseImageHashStableChecks}

      if [[ "$status" -ne 0 ]]; then
          exit 1
      fi

      echo "test-base-image-hash-stable: PASS (base drvPath invariant across ${toString (builtins.length (builtins.attrNames baseImagePermutations))} profile perturbations)"
    '';
  };

  # Hash-stability guard for `wrix-stable-profile-<name>` (tier 1;
  # specs/image-builder.md § Provenance-Tiered Layering). Tier 1's contents are
  # fixed per profile instance, so its derivation hash must not move when a
  # tier-2 input changes. We read each sandbox image's chained `fromImage`
  # tier-1 (image.stableProfileImage) under several tier-2 perturbations — a
  # downstream-appended package, the agent runtime selection, the merged Claude
  # settings JSON, and MCP configs — holding the profile fixed, then assert one
  # drvPath across all of them. A regression threading a tier-2 input into
  # stable-profile-image.nix would split the drvPaths and fail here.
  stableProfileImageOf = args: (sandboxLib.mkSandbox args).image.stableProfileImage.drvPath;
  referenceStableProfile = stableProfileImageOf { profile = sandboxLib.profiles.base; };
  stableProfilePermutations = {
    packages = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      packages = [ linuxPkgs.hello ];
    };
    agentDirect = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      agent = "direct";
      agentPkg = linuxPkgs.hello;
    };
    claudeSettings = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      agent = "claude";
      agentSettings.env.ANTHROPIC_MODEL = "claude-hash-stable-probe";
    };
    mcpConfigs = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      mcpRuntime = true;
    };
  };
  stableProfileHashStableChecks = concatStringsSep "\n" (
    mapAttrsToList (name: drvPath: ''
      if [[ "${drvPath}" != "$reference" ]]; then
          echo "FAIL: wrix-stable-profile drvPath changed under tier-2 perturbation '${name}'" >&2
          echo "  reference  : $reference" >&2
          echo "  ${name}: ${drvPath}" >&2
          status=1
      fi
    '') stableProfilePermutations
  );
  stableProfileHashStableTest = pkgs.writeShellApplication {
    name = "test-stable-profile-hash-stable";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      reference="${referenceStableProfile}"
      status=0

      ${stableProfileHashStableChecks}

      if [[ "$status" -ne 0 ]]; then
          exit 1
      fi

      echo "test-stable-profile-hash-stable: PASS (tier-1 drvPath invariant across ${toString (builtins.length (builtins.attrNames stableProfilePermutations))} tier-2 perturbations)"
    '';
  };

  # Membership guard for tier 1 (specs/image-builder.md § Provenance-Tiered
  # Layering): `wrix-stable-profile-<name>` holds only fixed-per-instance
  # content — no agent runtime, no downstream-appended package. The check is
  # agent-invariant: neither the consumer-supplied agent runtime nor a
  # downstream-appended package may appear in tier 1's `lowerTiersClosure`. It
  # further pins where each one DOES land: the agent runtime is tier 2 (present
  # in the agent tier's `lowerTiersClosure`, the base+stable+agent union, but
  # not the base+stable one), the appended package is tier 3 leaf (in neither
  # tier's `lowerTiersClosure`, only the whole image closure). Built via image.nix
  # directly with a light agent package so the check does not drag in claude-code.
  membershipAppendedPkg = linuxPkgs.writeShellScriptBin "wrix-stable-membership-appended" ''
    echo "downstream-appended package"
  '';
  membershipRunnerPkg = linuxPkgs.writeShellScriptBin "wrix-stable-membership-runner" ''
    echo "consumer-supplied agent runtime"
  '';
  membershipImage = import ../../lib/sandbox/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    asTarball = !isLinux;
    profile = {
      name = "membership";
      corePackages = [ linuxPkgs.coreutils ];
      packages = [
        linuxPkgs.coreutils
        membershipAppendedPkg
      ];
      env = { };
    };
    agent = "direct";
    agentPkg = membershipRunnerPkg;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };
  membershipImageClosure = linuxPkgs.closureInfo { rootPaths = [ membershipImage ]; };
  imageTierMembershipTest = pkgs.writeShellApplication {
    name = "test-image-tier-membership";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gawk
      pkgs.gnugrep
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          ${archiveShellHelpers}

          assert_new_layers() {
              local label="$1"
              local layers_file="$2"
              if [[ ! -s "$layers_file" ]]; then
                  echo "FAIL: $label emitted no tier-owned layers" >&2
                  exit 1
              fi
          }

          assert_no_overlap() {
              local label="$1"
              local emitted_paths="$2"
              local lower_closure="$3"
              local overlap
              overlap=$(comm -12 "$emitted_paths" "$lower_closure")
              if [[ -n "$overlap" ]]; then
                  echo "FAIL: $label re-emitted store paths already shipped by lower tiers" >&2
                  echo "$overlap" >&2
                  exit 1
              fi
          }

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          unpack_archive "tier0" "${membershipImage.baseImage}" "$tmp/tier0"
          unpack_archive "tier1" "${membershipImage.stableProfileImage}" "$tmp/tier1"
          unpack_archive "tier2" "${membershipImage.agentImage}" "$tmp/tier2"
          unpack_archive "leaf" "${membershipImage}" "$tmp/leaf"

          write_unique_layers "$tmp/tier0" "$tmp/tier0.layers"
          write_unique_layers "$tmp/tier1" "$tmp/tier1.layers"
          write_unique_layers "$tmp/tier2" "$tmp/tier2.layers"
          write_unique_layers "$tmp/leaf" "$tmp/leaf.layers"

          comm -23 "$tmp/tier1.layers" "$tmp/tier0.layers" >"$tmp/tier1.new.layers"
          comm -23 "$tmp/tier2.layers" "$tmp/tier1.layers" >"$tmp/tier2.new.layers"
          comm -23 "$tmp/leaf.layers" "$tmp/tier2.layers" >"$tmp/leaf.new.layers"

          assert_new_layers "tier 1" "$tmp/tier1.new.layers"
          assert_new_layers "tier 2" "$tmp/tier2.new.layers"
          assert_new_layers "leaf" "$tmp/leaf.new.layers"

          list_layer_store_paths "$tmp/tier1" "$tmp/tier1.new.layers" >"$tmp/tier1.paths"
          list_layer_store_paths "$tmp/tier2" "$tmp/tier2.new.layers" >"$tmp/tier2.paths"
          list_layer_store_paths "$tmp/leaf" "$tmp/leaf.new.layers" >"$tmp/leaf.paths"

          sort -u "${baseContentsClosure}/store-paths" >"$tmp/base.closure"
          sort -u "${membershipImage.stableProfileImage.lowerTiersClosure}/store-paths" >"$tmp/tier1.closure"
          sort -u "${membershipImage.agentImage.lowerTiersClosure}/store-paths" >"$tmp/tier2.closure"

          assert_no_overlap "tier 1" "$tmp/tier1.paths" "$tmp/base.closure"
          assert_no_overlap "tier 2" "$tmp/tier2.paths" "$tmp/tier1.closure"
          assert_no_overlap "leaf" "$tmp/leaf.paths" "$tmp/tier2.closure"

          tier1_count=$(wc -l <"$tmp/tier1.new.layers")
          tier2_count=$(wc -l <"$tmp/tier2.new.layers")
          leaf_count=$(wc -l <"$tmp/leaf.new.layers")
          echo "test-image-tier-membership: PASS (new layers tier1=$tier1_count tier2=$tier2_count leaf=$leaf_count skip lower closures)"
        ''
      else
        ''
          echo "test-image-tier-membership: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };
  stableProfileMembershipTest = pkgs.writeShellApplication {
    name = "test-stable-profile-membership";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text =
      if isLinux then
        ''
          tier1_closure=${membershipImage.stableProfileImage.lowerTiersClosure}/store-paths
          tier2_closure=${membershipImage.agentImage.lowerTiersClosure}/store-paths
          image_closure=${membershipImageClosure}/store-paths
          appended=${membershipAppendedPkg}
          runner=${membershipRunnerPkg}

          # Neither the agent runtime nor a downstream-appended package belongs in
          # tier 1 (the base+stable union).
          for above in "$appended" "$runner"; do
              if grep -qxF "$above" "$tier1_closure"; then
                  echo "FAIL: non-tier-1 path leaked into the wrix-stable-profile tier-1 closure" >&2
                  echo "  leaked : $above" >&2
                  echo "  tier-1 : $tier1_closure" >&2
                  exit 1
              fi
              if ! grep -qxF "$above" "$image_closure"; then
                  echo "FAIL: expected path missing from the leaf image closure" >&2
                  echo "  expected: $above" >&2
                  echo "  image   : $image_closure" >&2
                  exit 1
              fi
          done

          # The agent runtime is tier 2: present in the agent tier's lowerTiersClosure
          # (base+stable+agent), absent from tier 1.
          if ! grep -qxF "$runner" "$tier2_closure"; then
              echo "FAIL: agent runtime absent from the wrix-agent tier-2 closure" >&2
              echo "  expected: $runner" >&2
              echo "  tier-2  : $tier2_closure" >&2
              exit 1
          fi

          # The appended package is tier 3 leaf: in neither lower tier's closure.
          if grep -qxF "$appended" "$tier2_closure"; then
              echo "FAIL: downstream-appended package leaked into the wrix-agent tier-2 closure" >&2
              echo "  leaked : $appended" >&2
              echo "  tier-2 : $tier2_closure" >&2
              exit 1
          fi

          echo "test-stable-profile-membership: PASS (agent runtime is tier 2, appended package is tier-3 leaf, neither in tier 1)"
        ''
      else
        ''
          echo "test-stable-profile-membership: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Pinned-toolchain tier guard (specs/image-builder.md § Provenance-Tiered
  # Layering): a downstream-pinned rust toolchain is fixed per instance, so it
  # belongs to `corePackages` and lands in tier 1, never the volatile leaf. We
  # build a project-pinned rust profile from a fixture rust-toolchain.toml and
  # assert every rust-specific core package (the pinned toolchain plus its fixed
  # support packages) is reachable from the leaf image's tier-1
  # `lowerTiersClosure`.
  toolchainFixtureSha = "sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8=";
  pinnedToolchainStableTest =
    if !isLinux then
      pkgs.writeShellApplication {
        name = "test-pinned-toolchain-stable-tier";
        runtimeInputs = [ pkgs.coreutils ];
        text = ''
          echo "test-pinned-toolchain-stable-tier: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
      }
    else
      let
        pinnedProfile = sandboxLib.rustProfileFromFile {
          file = ../fixtures/rust-toolchain.toml;
          sha256 = toolchainFixtureSha;
        };
        pinnedSandbox = sandboxLib.mkSandbox {
          profile = pinnedProfile;
          agent = "claude";
        };
        rustCoreExtras = subtractLists sandboxLib.profiles.base.corePackages pinnedProfile.corePackages;
        rustCoreExtrasList = concatStringsSep " " (map (p: ''"${p}"'') rustCoreExtras);
      in
      pkgs.writeShellApplication {
        name = "test-pinned-toolchain-stable-tier";
        runtimeInputs = [
          pkgs.coreutils
          pkgs.gnugrep
        ];
        text = ''
          tier1_closure=${pinnedSandbox.image.stableProfileImage.lowerTiersClosure}/store-paths

          members=(${rustCoreExtrasList})
          if [[ "''${#members[@]}" -eq 0 ]]; then
              echo "FAIL: pinned rust profile contributed no core packages over base" >&2
              exit 1
          fi

          for member in "''${members[@]}"; do
              if ! grep -qxF "$member" "$tier1_closure"; then
                  echo "FAIL: pinned-toolchain core package absent from the wrix-stable-profile tier-1 closure" >&2
                  echo "  member : $member" >&2
                  echo "  tier-1 : $tier1_closure" >&2
                  exit 1
              fi
          done

          echo "test-pinned-toolchain-stable-tier: PASS (''${#members[@]} pinned core packages land in tier 1)"
        '';
      };

  # Probe builder for the layering tests below: a leaf image built via image.nix
  # directly with a light agent package so checks do not drag in claude-code.
  mkLeafProbe =
    {
      agent ? "claude",
      agentPkg ? null,
      settings ? { },
      extraPackages ? [ ],
      mcpServerConfigs ? { },
      entrypointSh ? ../../lib/sandbox/linux/entrypoint.sh,
    }:
    import ../../lib/sandbox/image.nix {
      pkgs = linuxPkgs;
      inherit agent entrypointSh mcpServerConfigs;
      profile = {
        name = "leafprobe";
        corePackages = [ linuxPkgs.coreutils ];
        packages = [ linuxPkgs.coreutils ] ++ extraPackages;
        env = { };
      };
      agentPkg = if agentPkg == null then linuxPkgs.hello else agentPkg;
      claudeConfig = { };
      claudeSettings = settings;
    };

  leafProbeSettingsA = mkLeafProbe { settings = { }; };
  leafProbeSettingsB = mkLeafProbe {
    settings = {
      probe = "downstream-change-leaf-only";
    };
  };
  leafProbePackageA = mkLeafProbe {
    extraPackages = [
      (linuxPkgs.writeShellScriptBin "wrix-leaf-package-probe" ''
        echo "leaf package alpha"
      '')
    ];
  };
  leafProbePackageB = mkLeafProbe {
    extraPackages = [
      (linuxPkgs.writeShellScriptBin "wrix-leaf-package-probe" ''
        echo "leaf package beta"
      '')
    ];
  };
  leafProbeMcpA = mkLeafProbe {
    mcpServerConfigs.probe = {
      command = "probe-mcp";
      args = [ "alpha" ];
    };
  };
  leafProbeMcpB = mkLeafProbe {
    mcpServerConfigs.probe = {
      command = "probe-mcp";
      args = [ "beta" ];
    };
  };
  leafProbeEntrypointA = mkLeafProbe {
    entrypointSh = linuxPkgs.writeShellScript "wrix-entrypoint-probe" ''
      echo "entrypoint alpha"
    '';
  };
  leafProbeEntrypointB = mkLeafProbe {
    entrypointSh = linuxPkgs.writeShellScript "wrix-entrypoint-probe" ''
      echo "entrypoint beta"
    '';
  };
  downstreamChangeLeafOnlyTest = pkgs.writeShellApplication {
    name = "test-downstream-change-leaf-only";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          assert_leaf_change() {
              local label="$1"
              local img_a="$2"
              local img_b="$3"
              local tier1_tar="$4"
              local tier2_tar="$5"
              local case_dir="$tmp/$label"
              local missing only_a only_b lower_count

              if [[ "$img_a" == "$img_b" ]]; then
                  echo "FAIL: $label perturbation resolved to identical image streams" >&2
                  exit 1
              fi

              mkdir -p "$case_dir"
              "$img_a" | tar -xO manifest.json >"$case_dir/a.json"
              "$img_b" | tar -xO manifest.json >"$case_dir/b.json"
              tar -xOf "$tier1_tar" manifest.json >"$case_dir/tier1.json"
              tar -xOf "$tier2_tar" manifest.json >"$case_dir/tier2.json"

              jq -r '.[0].Layers[]' "$case_dir/a.json" | sort -u >"$case_dir/a.layers"
              jq -r '.[0].Layers[]' "$case_dir/b.json" | sort -u >"$case_dir/b.layers"
              jq -r '.[0].Layers[]' "$case_dir/tier1.json" "$case_dir/tier2.json" | sort -u >"$case_dir/lower.layers"
              comm -12 "$case_dir/a.layers" "$case_dir/b.layers" >"$case_dir/shared.layers"
              lower_count=$(wc -l <"$case_dir/lower.layers")

              missing=$(comm -23 "$case_dir/lower.layers" "$case_dir/shared.layers")
              if [[ -n "$missing" ]]; then
                  echo "FAIL: $label perturbation changed a tier-0/tier-1/tier-2 layer blob" >&2
                  echo "$missing" >&2
                  exit 1
              fi

              only_a=$(comm -23 "$case_dir/a.layers" "$case_dir/b.layers" | wc -l)
              only_b=$(comm -13 "$case_dir/a.layers" "$case_dir/b.layers" | wc -l)
              if [[ "$only_a" -eq 0 && "$only_b" -eq 0 ]]; then
                  echo "FAIL: $label perturbation left the manifest identical" >&2
                  exit 1
              fi

              printf '%s\n' "$label: lower=$lower_count only_in_A=$only_a only_in_B=$only_b" >&2
          }

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          assert_leaf_change "settings" \
              "${leafProbeSettingsA}" \
              "${leafProbeSettingsB}" \
              "${leafProbeSettingsA.stableProfileImage}" \
              "${leafProbeSettingsA.agentImage}"
          assert_leaf_change "package-delta" \
              "${leafProbePackageA}" \
              "${leafProbePackageB}" \
              "${leafProbePackageA.stableProfileImage}" \
              "${leafProbePackageA.agentImage}"
          assert_leaf_change "mcp-config" \
              "${leafProbeMcpA}" \
              "${leafProbeMcpB}" \
              "${leafProbeMcpA.stableProfileImage}" \
              "${leafProbeMcpA.agentImage}"

          echo "test-downstream-change-leaf-only: PASS"
        ''
      else
        ''
          echo "test-downstream-change-leaf-only: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  archivelessGeneratedChangeTest = pkgs.writeShellApplication {
    name = "test-archiveless-generated-change";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gnutar
      pkgs.jq
      pkgs.nix
    ];
    text =
      if isLinux then
        ''
          assert_no_raw_image_input() {
              local label="$1"
              local path="$2"
              local forbidden="$3"
              local deriver
              deriver=$(nix-store -q --deriver "$path")
              if [[ "$deriver" != "unknown-deriver" ]] && nix-store -q --references "$deriver" | grep -Fxq "$forbidden"; then
                  echo "FAIL: $label builder depends on the raw whole-image output" >&2
                  exit 1
              fi
          }

          assert_generated_change() {
              local label="$1"
              local source_a="$2"
              local source_b="$3"
              local raw_a="$4"
              local raw_b="$5"
              local agent_tar="$6"
              local case_dir="$tmp/$label"
              local digest_a digest_b missing only_a only_b top_a top_b oci_a oci_b

              mkdir -p "$case_dir"
              digest_a=$(jq -r '.digest' "$source_a")
              digest_b=$(jq -r '.digest' "$source_b")
              if [[ "$digest_a" == "$digest_b" ]]; then
                  echo "FAIL: $label generated metadata change left the descriptor/config digest unchanged" >&2
                  exit 1
              fi

              jq -r '.layers[].digest' "$source_a" | sort -u >"$case_dir/a.layers"
              jq -r '.layers[].digest' "$source_b" | sort -u >"$case_dir/b.layers"
              tar -xOf "$agent_tar" manifest.json \
                | jq -r '.[0].Layers[] | sub("/layer.tar$"; "") | "sha256:" + .' \
                | sort -u >"$case_dir/lower.layers"
              comm -12 "$case_dir/a.layers" "$case_dir/b.layers" >"$case_dir/shared.layers"
              comm -23 "$case_dir/a.layers" "$case_dir/b.layers" >"$case_dir/a.only"
              comm -13 "$case_dir/a.layers" "$case_dir/b.layers" >"$case_dir/b.only"

              missing=$(comm -23 "$case_dir/lower.layers" "$case_dir/shared.layers")
              if [[ -n "$missing" ]]; then
                  echo "FAIL: $label generated metadata change altered a lower-tier layer blob" >&2
                  echo "$missing" >&2
                  exit 1
              fi

              only_a=$(wc -l <"$case_dir/a.only")
              only_b=$(wc -l <"$case_dir/b.only")
              if [[ "$only_a" -ne 1 || "$only_b" -ne 1 ]]; then
                  echo "FAIL: $label should alter only the top customisation layer (only_a=$only_a only_b=$only_b)" >&2
                  exit 1
              fi

              top_a=$(jq -r '.layers[-1].digest' "$source_a")
              top_b=$(jq -r '.layers[-1].digest' "$source_b")
              if ! grep -qxF "$top_a" "$case_dir/a.only" || ! grep -qxF "$top_b" "$case_dir/b.only"; then
                  echo "FAIL: $label changed layer is not the descriptor's final customisation layer" >&2
                  exit 1
              fi

              oci_a=$(jq -r '.oci_layout' "$source_a")
              oci_b=$(jq -r '.oci_layout' "$source_b")
              assert_no_raw_image_input "$label descriptor A" "$source_a" "$raw_a"
              assert_no_raw_image_input "$label descriptor B" "$source_b" "$raw_b"
              assert_no_raw_image_input "$label OCI layout A" "$oci_a" "$raw_a"
              assert_no_raw_image_input "$label OCI layout B" "$oci_b" "$raw_b"
          }

          assert_nix_db_registration_top_layer() {
              local label="$1"
              local leaf_stream="$2"
              local agent_tar="$3"
              local case_dir="$tmp/$label"
              local db_layer cust_layer layer

              mkdir -p "$case_dir/leaf" "$case_dir/lower"
              "$leaf_stream" >"$case_dir/leaf.tar"
              tar -xf "$case_dir/leaf.tar" -C "$case_dir/leaf"
              cust_layer=$(jq -r '.[0].Layers[-1]' "$case_dir/leaf/manifest.json")
              db_layer=""
              while IFS= read -r layer; do
                  if grep -qE '(^|\./)nix/var/nix/db/db\.sqlite$' < <(tar -tf "$case_dir/leaf/$layer"); then
                      db_layer="$layer"
                      break
                  fi
              done < <(jq -r '.[0].Layers[]' "$case_dir/leaf/manifest.json")

              if [[ -z "$db_layer" ]]; then
                  echo "FAIL: Nix DB registration was not baked into the leaf image" >&2
                  exit 1
              fi
              if [[ "$db_layer" != "$cust_layer" ]]; then
                  echo "FAIL: Nix DB registration rides in $db_layer, not top customisation layer $cust_layer" >&2
                  exit 1
              fi

              tar -xf "$agent_tar" -C "$case_dir/lower"
              while IFS= read -r layer; do
                  if grep -qE '(^|\./)nix/var/nix/db/db\.sqlite$' < <(tar -tf "$case_dir/lower/$layer"); then
                      echo "FAIL: Nix DB registration leaked into a lower tier layer" >&2
                      exit 1
                  fi
              done < <(jq -r '.[0].Layers[]' "$case_dir/lower/manifest.json")
          }

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          assert_generated_change "settings" \
              "${leafProbeSettingsA.source}" \
              "${leafProbeSettingsB.source}" \
              "${discardContext leafProbeSettingsA}" \
              "${discardContext leafProbeSettingsB}" \
              "${leafProbeSettingsA.agentImage}"
          assert_generated_change "mcp-config" \
              "${leafProbeMcpA.source}" \
              "${leafProbeMcpB.source}" \
              "${discardContext leafProbeMcpA}" \
              "${discardContext leafProbeMcpB}" \
              "${leafProbeMcpA.agentImage}"
          assert_generated_change "entrypoint" \
              "${leafProbeEntrypointA.source}" \
              "${leafProbeEntrypointB.source}" \
              "${discardContext leafProbeEntrypointA}" \
              "${discardContext leafProbeEntrypointB}" \
              "${leafProbeEntrypointA.agentImage}"
          assert_nix_db_registration_top_layer \
              "nix-db-registration" \
              "${leafProbeSettingsA}" \
              "${leafProbeSettingsA.agentImage}"

          echo "test-archiveless-generated-change: PASS"
        ''
      else
        ''
          echo "test-archiveless-generated-change: skipped on this platform (Linux-only descriptor verifier)" >&2
          exit 0
        '';
  };

  # Agent-tier-isolation guard (specs/image-builder.md § Provenance-Tiered
  # Layering). The selected agent runtime rides its own tier
  # `wrix-agent-<agent>-<name>`, chained atop the toolchain tier. Two leaf
  # images that differ only in the agent package's version (two distinct direct
  # runners) share an identical tier 0 and tier 1 — neither depends on the agent
  # axis — so every tier-0 and tier-1 blob is byte-identical, while the agent
  # tier's own tar changes. This is the weight-driven payoff: an agent-version
  # bump never re-ships the heavier toolchain below it.
  leafProbeRunnerA = mkLeafProbe {
    agent = "direct";
    agentPkg = pkgs.writeShellScriptBin "wrix-agent-probe-runner" ''
      echo "agent probe runner A"
    '';
  };
  leafProbeRunnerB = mkLeafProbe {
    agent = "direct";
    agentPkg = pkgs.writeShellScriptBin "wrix-agent-probe-runner" ''
      echo "agent probe runner B — a different version"
    '';
  };
  agentTierIsolatedTest = pkgs.writeShellApplication {
    name = "test-agent-tier-isolated";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          imgA=${leafProbeRunnerA}
          imgB=${leafProbeRunnerB}
          tier1_tar=${leafProbeRunnerA.stableProfileImage}
          agentA_tar=${leafProbeRunnerA.agentImage}
          agentB_tar=${leafProbeRunnerB.agentImage}

          if [[ "$imgA" == "$imgB" ]]; then
              echo "FAIL: the two agent-version probe images resolved to the same stream" >&2
              echo "      script; the agent perturbation did not materialise a distinct leaf" >&2
              exit 1
          fi
          if [[ "$agentA_tar" == "$agentB_tar" ]]; then
              echo "FAIL: the two agent-version probes share an agent tier tar; the agent" >&2
              echo "      runtime does not ride its own tier" >&2
              exit 1
          fi

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          "$imgA" | tar -xO manifest.json > "$tmp/a.json"
          "$imgB" | tar -xO manifest.json > "$tmp/b.json"
          tar -xOf "$tier1_tar" manifest.json > "$tmp/tier1.json"

          jq -r '.[0].Layers[]' "$tmp/a.json" | sort -u > "$tmp/a.layers"
          jq -r '.[0].Layers[]' "$tmp/b.json" | sort -u > "$tmp/b.layers"
          jq -r '.[0].Layers[]' "$tmp/tier1.json" | sort -u > "$tmp/tier1.layers"

          comm -12 "$tmp/a.layers" "$tmp/b.layers" > "$tmp/shared.layers"
          tier1_count=$(wc -l < "$tmp/tier1.layers")

          # Every tier-0 + tier-1 blob must survive byte-identical in BOTH leaves
          # across the agent-version bump.
          missing=$(comm -23 "$tmp/tier1.layers" "$tmp/shared.layers")
          if [[ -n "$missing" ]]; then
              echo "FAIL: a tier-0/tier-1 layer blob changed across the agent-version bump" >&2
              echo "  tier blobs not shared by both leaves:" >&2
              echo "$missing" >&2
              exit 1
          fi

          # The agent tier must actually change (otherwise nothing isolates it).
          only_a=$(comm -23 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          only_b=$(comm -13 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          if [[ "$only_a" -eq 0 && "$only_b" -eq 0 ]]; then
              echo "FAIL: no blob changed across the agent-version bump; the agent tier did" >&2
              echo "      not re-emit" >&2
              exit 1
          fi

          echo "test-agent-tier-isolated: PASS ($tier1_count tier-0/tier-1 blobs byte-identical across the agent-version bump; only_in_A=$only_a only_in_B=$only_b)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # the manifest diff this verifier performs is Linux-only.
          echo "test-agent-tier-isolated: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Agent-exclusivity guard (specs/image-builder.md § Provenance-Tiered Layering;
  # specs/sandbox.md § Agent runtime axis). Exactly one agent rides each image:
  # an `agent = "direct"` image carries its runner and NO claude-code, even when
  # the build is handed a real claude-code as agentPkg (the claude branch is
  # simply never selected); an `agent = "claude"` image carries claude-code and
  # not the direct runner. Built via image.nix directly so the closures are
  # scannable.
  agentExclusiveProfile = {
    name = "agentexcl";
    corePackages = [ linuxPkgs.coreutils ];
    packages = [ linuxPkgs.coreutils ];
    env = { };
  };
  agentExclusiveRunner = linuxPkgs.writeShellScriptBin "wrix-agent-exclusive-runner" ''
    echo "agent exclusive direct runner"
  '';
  mkAgentExclusiveImage =
    {
      agent,
      agentPkg ? null,
    }:
    import ../../lib/sandbox/image.nix {
      pkgs = linuxPkgs;
      inherit agent;
      profile = agentExclusiveProfile;
      agentPkg = if agentPkg == null then claudeCodePkg else agentPkg;
      entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
      claudeConfig = { };
      claudeSettings = { };
    };
  agentExclusiveDirect = mkAgentExclusiveImage {
    agent = "direct";
    agentPkg = agentExclusiveRunner;
  };
  agentExclusiveClaude = mkAgentExclusiveImage { agent = "claude"; };
  agentExclusiveDirectClosure = linuxPkgs.closureInfo { rootPaths = [ agentExclusiveDirect ]; };
  agentExclusiveClaudeClosure = linuxPkgs.closureInfo { rootPaths = [ agentExclusiveClaude ]; };
  agentExclusiveTest = pkgs.writeShellApplication {
    name = "test-agent-exclusive";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text =
      if isLinux then
        ''
          default_direct_closure=${defaultImageClosure}/store-paths
          direct_closure=${agentExclusiveDirectClosure}/store-paths
          claude_closure=${agentExclusiveClaudeClosure}/store-paths
          claude_code=${claudeCodePkg}
          pi_agent=${piAgentPkg}
          runner=${agentExclusiveRunner}

          if ! grep -Eq '/nix/store/[a-z0-9]{32}-loom-direct-runner$' "$default_direct_closure"; then
              echo "FAIL: default agent=direct image closure does not contain loom-direct-runner" >&2
              echo "  closure: $default_direct_closure" >&2
              exit 1
          fi
          if grep -qxF "$claude_code" "$default_direct_closure" || grep -qxF "$pi_agent" "$default_direct_closure"; then
              echo "FAIL: default agent=direct image closure contains a non-selected agent runtime" >&2
              exit 1
          fi

          if ! grep -qxF "$runner" "$direct_closure"; then
              echo "FAIL: direct runner missing from the agent=direct image closure" >&2
              echo "  expected: $runner" >&2
              exit 1
          fi
          if grep -qxF "$claude_code" "$direct_closure" || grep -qxF "$pi_agent" "$direct_closure"; then
              echo "FAIL: custom agent=direct image closure contains a non-selected agent runtime" >&2
              exit 1
          fi

          if ! grep -qxF "$claude_code" "$claude_closure"; then
              echo "FAIL: claude-code missing from the agent=claude image closure" >&2
              echo "  expected: $claude_code" >&2
              exit 1
          fi
          if grep -qxF "$runner" "$claude_closure" \
              || grep -qxF "$pi_agent" "$claude_closure" \
              || grep -Eq '/nix/store/[a-z0-9]{32}-loom-direct-runner$' "$claude_closure"; then
              echo "FAIL: agent=claude image closure contains a non-selected agent runtime" >&2
              exit 1
          fi

          echo "test-agent-exclusive: PASS"
        ''
      else
        ''
          echo "test-agent-exclusive: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  agentPkgThreadedRunner = linuxPkgs.writeShellScriptBin "wrix-agentpkg-threaded-runner" ''
    echo "agentPkg threaded runner"
  '';
  agentPkgThreadedImage = mkAgentExclusiveImage {
    agent = "direct";
    agentPkg = agentPkgThreadedRunner;
  };
  agentPkgThreadedClosure = linuxPkgs.closureInfo { rootPaths = [ agentPkgThreadedImage ]; };
  agentPkgThreadedTest = pkgs.writeShellApplication {
    name = "test-agent-pkg-threaded";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text =
      if isLinux then
        ''
          image_closure=${agentPkgThreadedClosure}/store-paths
          selected_agent_pkg=${agentPkgThreadedRunner}

          if ! grep -qxF "$selected_agent_pkg" "$image_closure"; then
              echo "FAIL: selected agentPkg is missing from the image closure" >&2
              echo "  expected: $selected_agent_pkg" >&2
              echo "  closure : $image_closure" >&2
              exit 1
          fi

          echo "test-agent-pkg-threaded: PASS"
        ''
      else
        ''
          echo "test-agent-pkg-threaded: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Iteration-cost-bounded probe (specs/image-builder.md Success Criteria:
  # "A one-file perturbation in profile-level inputs (one wrapper script touched)
  # leaves every layer-blob hash in the resulting image's manifest unchanged
  # except for the customisation layer and any top layer that directly depends on
  # the changed file"). Two profile images that differ only in a single wrapper
  # script's body — a one-line edit. The fromImage base, the entrypoint, and
  # every other input are held fixed, so the streamLayeredImage manifests must
  # share every layer blob except the customisation layer (which always re-hashes,
  # since it aggregates the perturbed content) and the top layer(s) carrying the
  # wrapper's store path.
  mkIterationProbeImage =
    line:
    import ../../lib/sandbox/image.nix {
      inherit pkgs;
      agent = "claude";
      profile = {
        name = "iterprobe";
        packages = [
          (pkgs.writeShellScriptBin "wrix-iteration-probe" ''
            # iteration-cost-bounded probe wrapper
            echo "iteration probe: ${line}"
          '')
        ];
        env = { };
      };
      agentPkg = pkgs.hello;
      entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
      claudeConfig = { };
      claudeSettings = { };
    };
  iterationProbeImageA = mkIterationProbeImage "alpha";
  iterationProbeImageB = mkIterationProbeImage "beta";

  # Changed-blob ceiling: the customisation layer plus the top layer(s) carrying
  # the perturbed wrapper's store path. A regression that threaded the wrapper
  # into a shared bottom-of-closure layer would churn base blobs and exceed it.
  iterationMaxChangedBlobs = 4;

  iterationCostBoundedTest = pkgs.writeShellApplication {
    name = "test-iteration-cost-bounded";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          imgA=${iterationProbeImageA}
          imgB=${iterationProbeImageB}
          max_changed=${toString iterationMaxChangedBlobs}

          if [[ "$imgA" == "$imgB" ]]; then
              echo "FAIL: probe images A and B resolved to the same stream script;" >&2
              echo "      the one-line wrapper perturbation did not materialise a distinct image" >&2
              exit 1
          fi

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          # streamLayeredImage emits a docker-archive tar; manifest.json lists each
          # layer blob as "<sha256>/layer.tar".
          "$imgA" | tar -xO manifest.json > "$tmp/a.json"
          "$imgB" | tar -xO manifest.json > "$tmp/b.json"

          jq -r '.[0].Layers[]' "$tmp/a.json" | sort -u > "$tmp/a.layers"
          jq -r '.[0].Layers[]' "$tmp/b.json" | sort -u > "$tmp/b.layers"

          # The customisation layer is streamLayeredImage's final layer.
          custA=$(jq -r '.[0].Layers[-1]' "$tmp/a.json")
          custB=$(jq -r '.[0].Layers[-1]' "$tmp/b.json")

          total=$(wc -l < "$tmp/a.layers")
          shared=$(comm -12 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          only_a=$(comm -23 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          only_b=$(comm -13 "$tmp/a.layers" "$tmp/b.layers" | wc -l)

          echo "layer blobs: total(A)=$total shared=$shared only_in_A=$only_a only_in_B=$only_b" >&2

          # The customisation layer aggregates the perturbed content, so it must move.
          if [[ "$custA" == "$custB" ]]; then
              echo "FAIL: customisation layer blob unchanged under the wrapper perturbation ($custA)" >&2
              echo "      the manifest diff cannot be attributed to the changed file" >&2
              exit 1
          fi

          if [[ "$only_a" -gt "$max_changed" || "$only_b" -gt "$max_changed" ]]; then
              echo "FAIL: one-file perturbation changed more than $max_changed layer blobs" >&2
              echo "      only_in_A=$only_a only_in_B=$only_b (expected <= $max_changed:" >&2
              echo "      customisation layer + the top layer(s) carrying the wrapper)" >&2
              echo "  only in A:" >&2
              comm -23 "$tmp/a.layers" "$tmp/b.layers" >&2
              echo "  only in B:" >&2
              comm -13 "$tmp/a.layers" "$tmp/b.layers" >&2
              exit 1
          fi

          if [[ "$shared" -lt $((total - max_changed)) ]]; then
              echo "FAIL: too few shared layer blobs ($shared of $total); the base and" >&2
              echo "      sibling layers should be invariant under a one-file perturbation" >&2
              exit 1
          fi

          echo "test-iteration-cost-bounded: PASS ($shared/$total layer blobs identical; only_in_A=$only_a only_in_B=$only_b within bound $max_changed)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # the manifest diff this verifier performs is Linux-only. Darwin's
          # iteration-cost bound is covered by base-image pinning alone (see
          # specs/image-builder.md § Out of Scope).
          echo "test-iteration-cost-bounded: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Customisation-layer-bounded probe. Opening the in-image Nix store
  # read-write (includeNixDB + the prekHooksBundle load-db in image.nix)
  # creates Nix's gc-reserved-space file `nix/var/nix/db/reserved` — an 8 MiB
  # all-zero block that landed in the streamLayeredImage customisation layer
  # and re-hashed it on every profile-level input change. image.nix removes
  # the file at the end of extraCommands; Nix recreates it lazily in-container.
  # This asserts the removal holds: the customisation layer must not carry
  # `db/reserved`, and its uncompressed tar must stay under the bound the 8 MiB
  # block used to blow past.
  customisationLayerMaxBytes = 4 * 1024 * 1024;
  customisationLayerBoundedTest = pkgs.writeShellApplication {
    name = "test-customisation-layer-bounded";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.gnugrep
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          img=${iterationProbeImageA}
          max_bytes=${toString customisationLayerMaxBytes}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          "$img" | tar -xf - -C "$tmp"

          cust=$(jq -r '.[0].Layers[-1]' "$tmp/manifest.json")
          layer="$tmp/$cust"

          if [[ ! -f "$layer" ]]; then
              echo "FAIL: customisation layer tar $cust not found in image archive" >&2
              exit 1
          fi

          if tar -tf "$layer" | grep -qE '(^|/)nix/var/nix/db/reserved$'; then
              echo "FAIL: customisation layer still carries nix/var/nix/db/reserved" >&2
              echo "      (the gc-reserved-space padding was not elided)" >&2
              exit 1
          fi

          layer_bytes=$(stat -c %s "$layer")
          echo "customisation layer: $cust ($layer_bytes bytes)" >&2

          if [[ "$layer_bytes" -gt "$max_bytes" ]]; then
              echo "FAIL: customisation layer is $layer_bytes bytes (bound $max_bytes)" >&2
              echo "      reserved padding elision should keep it well under the bound" >&2
              exit 1
          fi

          echo "test-customisation-layer-bounded: PASS ($layer_bytes bytes, reserved absent)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # extracting the customisation layer here is Linux-only. The reserved
          # elision itself runs identically under buildLayeredImage on Darwin.
          echo "test-customisation-layer-bounded: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Nix-DB-consistency verifier (specs/image-builder.md § In-Container Nix Store
  # Consistency; Success Criteria "The baked image's Nix database is consistent
  # with its on-disk store"). The image must register its FULL on-disk closure
  # valid — the leaf's own contents AND every path the fromImage base +
  # stable-profile tiers physically lay down (notably the tier-1 generated
  # passwd/group/nix.conf store paths and prekHooksBundle). includeNixDB alone
  # registers only the leaf's `contents`, leaving those tier paths on disk but
  # unregistered — orphans that make the unprivileged in-container user's
  # additive Nix ops try to chmod a root-owned path and fail with EPERM.
  #
  # The test reconstructs the composed image's on-disk store by listing every
  # store path across all three tiers' layer tars, then reads the baked
  # db.sqlite and asserts no on-disk store path is missing from ValidPaths. It
  # also asserts the DB rides in the leaf customisation layer only — present in
  # that final leaf layer, absent from both lower tiers — so registration does
  # not perturb the tier-0/tier-1 blobs. Built via image.nix directly with a
  # light agent package so the check does not drag in claude-code, but through
  # the same tier chain that produces the diagnosed orphan.
  nixDbProbeImage = import ../../lib/sandbox/image.nix {
    inherit pkgs;
    agent = "claude";
    profile = {
      name = "nixdbprobe";
      corePackages = [ pkgs.coreutils ];
      packages = [ pkgs.coreutils ];
      env = { };
    };
    agentPkg = pkgs.hello;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };
  imageNixDbConsistentTest = pkgs.writeShellApplication {
    name = "test-image-nix-db-consistent";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.gnugrep
      pkgs.jq
      pkgs.sqlite
    ];
    text =
      if isLinux then
        ''
          leaf_stream=${nixDbProbeImage}
          tier2_tar=${nixDbProbeImage.agentImage}
          tier1_tar=${nixDbProbeImage.stableProfileImage}
          tier0_tar=${nixDbProbeImage.baseImage}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          "$leaf_stream" > "$tmp/leaf.tar"

          # Unpack each tier's docker-archive (manifest.json + <sha>/layer.tar
          # blobs) and list the store paths every layer blob carries. The union
          # across all four tiers is the composed image's on-disk store.
          unpack_dir="$tmp/archives"
          mkdir -p "$unpack_dir"
          list_tier_store_paths() {
              local arc="$1" d
              d=$(mktemp -d -p "$unpack_dir")
              tar -xf "$arc" -C "$d"
              local layer
              while IFS= read -r layer; do
                  tar -tf "$d/$layer"
              done < <(jq -r '.[0].Layers[]' "$d/manifest.json")
          }

          {
              list_tier_store_paths "$tmp/leaf.tar"
              list_tier_store_paths "$tier2_tar"
              list_tier_store_paths "$tier1_tar"
              list_tier_store_paths "$tier0_tar"
          } | grep -oE 'nix/store/[a-z0-9]{32}-[^/]+' | sort -u | sed 's#^#/#' \
              > "$tmp/ondisk"

          # The baked Nix DB rides in the leaf customisation layer; find the
          # layer carrying it and extract the whole db dir (the sqlite store is
          # WAL-mode, so the -wal/-shm sidecars must travel with db.sqlite for
          # the registered rows to be visible).
          leafx="$tmp/leafx"
          mkdir -p "$leafx"
          tar -xf "$tmp/leaf.tar" -C "$leafx"
          dbroot="$tmp/dbroot"
          mkdir -p "$dbroot"
          db=""
          db_layer=""
          while IFS= read -r layer; do
              if tar -tf "$leafx/$layer" | grep -qE '(^|\./)nix/var/nix/db/db\.sqlite$'; then
                  tar -xf "$leafx/$layer" -C "$dbroot"
                  db="$dbroot/nix/var/nix/db/db.sqlite"
                  db_layer="$layer"
                  break
              fi
          done < <(jq -r '.[0].Layers[]' "$leafx/manifest.json")

          if [[ -z "$db" || ! -f "$db" ]]; then
              echo "FAIL: no nix/var/nix/db/db.sqlite found in any leaf layer" >&2
              echo "      (includeNixDB / load-db did not bake a database)" >&2
              exit 1
          fi

          # The DB must ride in the leaf's customisation layer (its final layer):
          # registration is metadata that copies no lower-tier store path up, so
          # it stays in the volatile leaf tier.
          cust_layer=$(jq -r '.[0].Layers[-1]' "$leafx/manifest.json")
          if [[ "$db_layer" != "$cust_layer" ]]; then
              echo "FAIL: baked Nix DB rides in leaf layer '$db_layer', not the" >&2
              echo "      customisation layer '$cust_layer'" >&2
              exit 1
          fi

          # The DB rides in the leaf ONLY: a db.sqlite in a lower tier would mean
          # registration perturbed a tier-0/tier-1/tier-2 blob and broke the
          # provenance-tiered chain's byte-identical-lower-tiers guarantee.
          for tier_tar in "$tier2_tar" "$tier1_tar" "$tier0_tar"; do
              td=$(mktemp -d -p "$unpack_dir")
              tar -xf "$tier_tar" -C "$td"
              while IFS= read -r layer; do
                  if tar -tf "$td/$layer" | grep -qE '(^|\./)nix/var/nix/db/db\.sqlite$'; then
                      echo "FAIL: lower tier '$tier_tar' carries a baked Nix DB;" >&2
                      echo "      the DB must ride in the leaf customisation layer only" >&2
                      exit 1
                  fi
              done < <(jq -r '.[0].Layers[]' "$td/manifest.json")
          done

          # Store paths are extracted read-only; SQLite opened read-write needs
          # to write the -wal/-shm sidecars while reading WAL-resident rows.
          chmod -R u+rwX "$dbroot/nix/var/nix/db"
          sqlite3 "$db" 'SELECT path FROM ValidPaths' | sort -u \
              > "$tmp/registered"

          ondisk_count=$(wc -l < "$tmp/ondisk")
          registered_count=$(wc -l < "$tmp/registered")
          if [[ "$ondisk_count" -eq 0 ]]; then
              echo "FAIL: reconstructed on-disk store is empty; the test did not" >&2
              echo "      enumerate the image's store paths" >&2
              exit 1
          fi
          if [[ "$registered_count" -eq 0 ]]; then
              echo "FAIL: baked Nix DB registered no ValidPaths" >&2
              exit 1
          fi

          # Every on-disk store path must be registered valid — no orphan.
          orphans=$(comm -23 "$tmp/ondisk" "$tmp/registered")
          if [[ -n "$orphans" ]]; then
              orphan_count=$(printf '%s\n' "$orphans" | grep -c '^' || true)
              echo "FAIL: $orphan_count on-disk store path(s) are not registered valid" >&2
              echo "      in the baked Nix DB (orphans break additive in-container Nix):" >&2
              printf '%s\n' "$orphans" | sed 's/^/  /' >&2
              exit 1
          fi

          echo "test-image-nix-db-consistent: PASS ($ondisk_count on-disk store paths all registered valid; no orphan)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # reconstructing the on-disk store from the leaf docker-archive here
          # is Linux-only. The full-closure registration in image.nix runs
          # identically under buildLayeredImage on Darwin.
          echo "test-image-nix-db-consistent: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Nix-DB no-dangling verifier (specs/image-builder.md § In-Container Nix Store
  # Consistency; Success Criteria "The baked image's Nix database registers no
  # dangling path"). The complement of the orphan check above: every path the
  # baked DB registers VALID must exist on disk. The full build closure
  # (leafContents ++ lowerTiersRootPaths) over-registers — it drags in
  # prekHooksBundle (config.Env-only, never in any tier's `contents`) and its
  # config.Env-unique closure, which buildLayeredImage never materializes into a
  # store layer. Registering those bakes a dangling (registered-but-absent) path
  # that makes an additive `nix build` trust the DB into feeding a missing path
  # to a builder, which then fails with `No such file or directory`. image.nix
  # registers over `lowerTiersContents` (the materialized contents closure), so
  # the registered set must equal the on-disk set with zero dangling paths.
  #
  # The test reconstructs the composed image's on-disk store the same way the
  # orphan check does — the union of store paths across all three tiers' layer
  # tars — then reads the baked db.sqlite ValidPaths and asserts no REGISTERED
  # path is absent from disk. Built via image.nix directly with a light
  # agent package so the check does not drag in claude-code, through the same
  # tier chain that produced the diagnosed dangling registration.
  imageNixDbNoDanglingTest = pkgs.writeShellApplication {
    name = "test-image-nix-db-no-dangling";
    runtimeInputs = optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.gnugrep
      pkgs.jq
      pkgs.sqlite
    ];
    text =
      if isLinux then
        ''
          leaf_stream=${nixDbProbeImage}
          tier2_tar=${nixDbProbeImage.agentImage}
          tier1_tar=${nixDbProbeImage.stableProfileImage}
          tier0_tar=${nixDbProbeImage.baseImage}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          "$leaf_stream" > "$tmp/leaf.tar"

          # Unpack each tier's docker-archive and list the store paths every
          # layer blob carries. The union across all four tiers is the composed
          # image's on-disk store.
          unpack_dir="$tmp/archives"
          mkdir -p "$unpack_dir"
          list_tier_store_paths() {
              local arc="$1" d
              d=$(mktemp -d -p "$unpack_dir")
              tar -xf "$arc" -C "$d"
              local layer
              while IFS= read -r layer; do
                  tar -tf "$d/$layer"
              done < <(jq -r '.[0].Layers[]' "$d/manifest.json")
          }

          {
              list_tier_store_paths "$tmp/leaf.tar"
              list_tier_store_paths "$tier2_tar"
              list_tier_store_paths "$tier1_tar"
              list_tier_store_paths "$tier0_tar"
          } | grep -oE 'nix/store/[a-z0-9]{32}-[^/]+' | sort -u | sed 's#^#/#' \
              > "$tmp/ondisk"

          # Extract the baked Nix DB from the leaf customisation layer (WAL-mode
          # sqlite: the -wal/-shm sidecars must travel with db.sqlite).
          leafx="$tmp/leafx"
          mkdir -p "$leafx"
          tar -xf "$tmp/leaf.tar" -C "$leafx"
          dbroot="$tmp/dbroot"
          mkdir -p "$dbroot"
          db=""
          while IFS= read -r layer; do
              if tar -tf "$leafx/$layer" | grep -qE '(^|\./)nix/var/nix/db/db\.sqlite$'; then
                  tar -xf "$leafx/$layer" -C "$dbroot"
                  db="$dbroot/nix/var/nix/db/db.sqlite"
                  break
              fi
          done < <(jq -r '.[0].Layers[]' "$leafx/manifest.json")

          if [[ -z "$db" || ! -f "$db" ]]; then
              echo "FAIL: no nix/var/nix/db/db.sqlite found in any leaf layer" >&2
              echo "      (includeNixDB / load-db did not bake a database)" >&2
              exit 1
          fi

          chmod -R u+rwX "$dbroot/nix/var/nix/db"
          sqlite3 "$db" 'SELECT path FROM ValidPaths' | sort -u \
              > "$tmp/registered"

          ondisk_count=$(wc -l < "$tmp/ondisk")
          registered_count=$(wc -l < "$tmp/registered")
          if [[ "$ondisk_count" -eq 0 ]]; then
              echo "FAIL: reconstructed on-disk store is empty; the test did not" >&2
              echo "      enumerate the image's store paths" >&2
              exit 1
          fi
          if [[ "$registered_count" -eq 0 ]]; then
              echo "FAIL: baked Nix DB registered no ValidPaths" >&2
              exit 1
          fi

          # Every registered-valid path must exist on disk — no dangling.
          dangling=$(comm -13 "$tmp/ondisk" "$tmp/registered")
          if [[ -n "$dangling" ]]; then
              dangling_count=$(printf '%s\n' "$dangling" | grep -c '^' || true)
              echo "FAIL: $dangling_count registered-valid path(s) are absent from the" >&2
              echo "      image's on-disk store (dangling registrations break additive" >&2
              echo "      in-container 'nix build' with 'No such file or directory'):" >&2
              printf '%s\n' "$dangling" | sed 's/^/  /' >&2
              exit 1
          fi

          echo "test-image-nix-db-no-dangling: PASS ($registered_count registered paths all present on disk; no dangling)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # reconstructing the on-disk store from the leaf docker-archive here
          # is Linux-only. The materialized-contents registration in image.nix
          # runs identically under buildLayeredImage on Darwin.
          echo "test-image-nix-db-no-dangling: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

in
{
  inherit
    wrixSpawnLoadTest
    imageInstallArchivelessTest
    imageInstallRealSkopeoTest
    imageInstallDigestSkipTest
    digestMatchesStoredIdTest
    linuxImageArchivelessSourceTest
    imageDigestNoTarTest
    imageTierGraphTest
    imageNixConfigTest
    imageCaCertificatesTest
    imageEntrypointCommandTest
    imageAgentMarkerTest
    imageTierMembershipTest
    wrixImagesSourceKindTest
    wrixImageLabelsTest
    agentDirectRunnerTest
    agentClaudeRuntimeTest
    claudeRuntimeNoopTest
    prekHooksClosureTest
    baseImageUniversalTest
    entrypointResolverBaseTest
    baseImageHashStableTest
    stableProfileHashStableTest
    stableProfileMembershipTest
    pinnedToolchainStableTest
    downstreamChangeLeafOnlyTest
    archivelessGeneratedChangeTest
    agentTierIsolatedTest
    agentExclusiveTest
    agentPkgThreadedTest
    iterationCostBoundedTest
    customisationLayerBoundedTest
    imageNixDbConsistentTest
    imageNixDbNoDanglingTest
    imageAssemblyNativeCheck
    ;
}
