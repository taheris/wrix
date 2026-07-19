{ pkgs, linuxPkgs }:

let
  inherit (builtins) toJSON;
  inherit (pkgs)
    bash
    coreutils
    gawk
    gnugrep
    gnused
    gnutar
    jq
    openssh
    writeShellApplication
    ;

  builderImage = import ../../lib/sandbox/builder/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    asTarball = true;
  };
  darwinTransportBuilder = import ../../lib/builder {
    inherit pkgs linuxPkgs;
    asTarball = true;
  };
  expectedSourceKind = "docker-archive";
  expectedRefPrefix = "wrix-builder:";
  labelsJson = toJSON builderImage.labels;

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
            fail "$label archive did not contain manifest.json"
        fi
    }

    write_unique_layers() {
        local archive_dir="$1"
        local output="$2"
        jq -r '.[0].Layers[]' "$archive_dir/manifest.json" | sort -u >"$output"
    }

    extract_layer_member() {
        local archive_dir="$1"
        local layers_file="$2"
        local member="$3"
        local output="$4"
        local layer
        local dot_member
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
  '';
in
{
  sshdHardeningTest = writeShellApplication {
    name = "test-linux-builder-sshd-hardening";
    runtimeInputs = [
      coreutils
      gawk
      gnugrep
      gnutar
      jq
    ];
    text = ''
      fail() {
          local message="$1"
          printf 'FAIL: %s\n' "$message" >&2
          exit 1
      }

      ${archiveShellHelpers}

      directive_value() {
          local key="$1"
          local path="$2"
          awk -v key="$key" 'tolower($1) == key { $1 = ""; sub(/^[[:space:]]+/, ""); print }' "$path"
      }

      require_directive() {
          local key="$1"
          local expected="$2"
          local actual
          actual="$(directive_value "$key" "$config")"
          if [[ "$actual" != "$expected" ]]; then
              fail "sshd_config $key is '$actual', expected '$expected'"
          fi
      }

      tmp="$(mktemp -d)"
      trap 'rm -rf "$tmp"' EXIT
      config="$tmp/sshd_config"

      unpack_archive "builder" "${builderImage}" "$tmp/image"
      write_unique_layers "$tmp/image" "$tmp/image.layers"
      config_file="$(jq -r '.[0].Config' "$tmp/image/manifest.json")"
      if ! jq -e '.config.Entrypoint == ["/entrypoint.sh"]' "$tmp/image/$config_file" >/dev/null; then
          jq '.config.Entrypoint' "$tmp/image/$config_file" >&2
          fail "builder image entrypoint is not /entrypoint.sh"
      fi
      if ! extract_layer_member "$tmp/image" "$tmp/image.layers" "entrypoint.sh" "$tmp/entrypoint.sh"; then
          fail "builder image is missing /entrypoint.sh"
      fi
      if ! extract_layer_member "$tmp/image" "$tmp/image.layers" "usr/lib/wrix-builder/sshd.sh" "$tmp/sshd.sh"; then
          fail "builder image is missing /usr/lib/wrix-builder/sshd.sh"
      fi
      if [[ "$(sha256sum "$tmp/entrypoint.sh" | cut -d ' ' -f 1)" != "$(sha256sum "${../../lib/sandbox/builder/entrypoint.sh}" | cut -d ' ' -f 1)" ]]; then
          fail "builder image /entrypoint.sh does not match the entrypoint source"
      fi
      if [[ "$(sha256sum "$tmp/sshd.sh" | cut -d ' ' -f 1)" != "$(sha256sum "${../../lib/sandbox/builder/sshd.sh}" | cut -d ' ' -f 1)" ]]; then
          fail "builder image sshd helper does not match the helper source"
      fi

      # shellcheck source=/dev/null
      source "$tmp/sshd.sh"
      wrix_builder_write_sshd_config builder "$config"

      require_directive port 22
      require_directive listenaddress 127.0.0.1
      require_directive passwordauthentication no
      require_directive permitrootlogin no
      require_directive allowusers builder
      require_directive authorizedkeysfile /home/%u/.ssh/authorized_keys
      require_directive hostkey /etc/ssh/ssh_host_ed25519_key

      echo "test-linux-builder-sshd-hardening: PASS"
    '';
  };

  sourceKindLoadTransportTest = writeShellApplication {
    name = "test-linux-builder-source-kind-load-transport";
    runtimeInputs = [
      bash
      coreutils
      gawk
      gnugrep
      gnused
      gnutar
      openssh
    ];
    text = ''
      WRIX_BUILDER_BIN='${darwinTransportBuilder}/bin/wrix-builder' \
        bash ${./key-material.sh} test_loads_image_through_source_kind_contract
    '';
  };

  imageSourceKindTest = writeShellApplication {
    name = "test-linux-builder-image-source-kind";
    runtimeInputs = [
      coreutils
      gnugrep
      gnutar
      jq
    ];
    text = ''
      fail() {
          local message="$1"
          printf 'FAIL: %s\n' "$message" >&2
          exit 1
      }

      require_labels_json() {
          local label_json="$1"
          if ! printf '%s\n' "$label_json" | jq -e '
              .["wrix.managed"] == "true" and
              .["wrix.image.kind"] == "builder"
            ' >/dev/null; then
              fail "builder image passthru labels do not retain wrix ownership: $label_json"
          fi
      }

      require_descriptor_contract() {
          local descriptor_digest
          local expected_digest
          expected_digest="$(<"$digest_path")"
          descriptor_digest="$(jq -r '.digest // empty' "$source_path")"
          if [[ "$descriptor_digest" != "$expected_digest" ]]; then
              fail "descriptor digest $descriptor_digest does not match digest file $expected_digest"
          fi
          if ! jq -e '
              type == "object" and
              .schema == 1 and
              .source_kind == "nix-descriptor" and
              .image.name == "wrix-builder" and
              .image.tag == "latest" and
              (.digest | test("^sha256:[0-9a-f]{64}$")) and
              (.oci_layout | type == "string" and startswith("/nix/store/")) and
              (.oci_ref | type == "string" and length > 0) and
              (.config.entrypoint == ["/entrypoint.sh"]) and
              .config.labels["wrix.managed"] == "true" and
              .config.labels["wrix.image.kind"] == "builder"
            ' "$source_path" >/dev/null; then
              jq . "$source_path" >&2
              fail "builder descriptor does not expose the shared image-source contract"
          fi
      }

      require_docker_archive_contract() {
          local archive_dir="$tmp/archive"
          local config_path
          mkdir -p "$archive_dir"
          if [[ -x "$source_path" ]]; then
              "$source_path" | tar -xf - -C "$archive_dir"
          else
              tar -xf "$source_path" -C "$archive_dir"
          fi
          config_path="$(jq -r '.[0].Config // empty' "$archive_dir/manifest.json")"
          if [[ -z "$config_path" || ! -f "$archive_dir/$config_path" ]]; then
              fail "builder docker archive does not contain a config JSON"
          fi
          if ! jq -e '
              ((.config.Labels // .config.labels // .Config.Labels // .Labels // {}) as $labels |
              $labels["wrix.managed"] == "true" and
              $labels["wrix.image.kind"] == "builder")
            ' "$archive_dir/$config_path" >/dev/null; then
              jq . "$archive_dir/$config_path" >&2
              fail "builder docker archive labels do not retain wrix ownership"
          fi
      }

      tmp="$(mktemp -d)"
      trap 'rm -rf "$tmp"' EXIT

      source_kind='${builderImage.source_kind}'
      expected_source_kind='${expectedSourceKind}'
      source_path='${toString builderImage.source}'
      digest_path='${toString builderImage.digest}'
      image_ref='${builderImage.ref}'
      labels_json='${labelsJson}'

      if [[ "$source_kind" != "$expected_source_kind" ]]; then
          fail "builder source_kind is $source_kind, expected $expected_source_kind"
      fi
      if [[ "$source_path" != /nix/store/* ]]; then
          fail "builder source path is not store-resident: $source_path"
      fi
      if [[ "$digest_path" != /nix/store/* ]]; then
          fail "builder digest path is not store-resident: $digest_path"
      fi
      if [[ "$image_ref" != ${expectedRefPrefix}* ]]; then
          fail "builder image ref does not follow the platform contract: $image_ref"
      fi
      if ! grep -Eq '^sha256:[0-9a-f]{64}$' "$digest_path"; then
          fail "builder digest file does not contain a sha256 digest"
      fi

      require_labels_json "$labels_json"
      case "$source_kind" in
          nix-descriptor)
              require_descriptor_contract
              ;;
          docker-archive)
              require_docker_archive_contract
              ;;
          *)
              fail "unsupported builder source_kind in verifier: $source_kind"
              ;;
      esac

      echo "test-linux-builder-image-source-kind: PASS"
    '';
  };
}
