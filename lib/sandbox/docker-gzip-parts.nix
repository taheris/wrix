# Independent gzip members preserve the exact uncompressed Docker tar stream.
# Reuse only identical outer-tar timestamps and the same compressor recipe.
{ pkgs, imageTools }:
{
  layout,
  conf,
  parentParts ? null,
}:
pkgs.runCommandLocal "wrix-docker-gzip-parts"
  {
    nativeBuildInputs = [
      pkgs.jq
      pkgs.coreutils
    ];
  }
  ''
    set -euo pipefail
    mkdir -p "$out"
    jq -er .mtime ${conf} > "$out/mtime"
    printf '%s\n' '${pkgs.pigz}/bin/pigz -n' > "$out/compressor"
    reusable=0
    ${pkgs.lib.optionalString (parentParts != null) ''
      if cmp -s "$out/mtime" ${parentParts}/mtime && \
         cmp -s "$out/compressor" ${parentParts}/compressor; then
        reusable=1
      fi
    ''}
    jq -er '.layers[].digest | select(startswith("sha256:")) | ltrimstr("sha256:")' \
      ${layout}/wrix/manifest.json > layers
    while IFS= read -r digest; do
      ${
        if parentParts == null then
          ""
        else
          ''
            if [[ "$reusable" == 1 && -f ${parentParts}/"$digest.gz" ]]; then
              ln -s ${parentParts}/"$digest.gz" "$out/$digest.gz"
              continue
            fi
          ''
      }
      ${imageTools}/bin/wrix-image-tools docker-layer-entry ${layout} ${conf} "$digest" \
        | ${pkgs.pigz}/bin/pigz -n -p "$NIX_BUILD_CORES" > "$out/$digest.gz"
    done < layers
    cp layers "$out/order"
  ''
