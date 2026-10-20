{ pkgs, wrix }:

pkgs.runCommand "cache-celler-packaging"
  {
    nativeBuildInputs = [ pkgs.jq ];
  }
  ''
    set -euo pipefail
    test -x '${wrix.rustPackage.cacheHook}/bin/wrix-cache-hook'
    test -x '${wrix.rustPackage.cachePublish}/bin/wrix-cache-publish'
    test -x '${wrix.rustPackage.supervisor}/bin/wrix-service-supervisor'
    test ! -L '${wrix.rustPackage.supervisor}/bin/wrix-service-supervisor'
    test ! -e '${wrix.rustPackage.supervisor}/bin/wrix-host-controller'
    test ! -e '${wrix.rustPackage.package}/bin/wrix-cache-serve'
    '${wrix.rustPackage.cacheHook}/bin/wrix-cache-hook' --help
    '${wrix.rustPackage.cachePublish}/bin/wrix-cache-publish' --help
    ${
      if wrix.serviceImage.source_kind == "nix-descriptor" then
        ''
          jq -e '.materialized_roots | any(test("-celler-"))' '${wrix.serviceImage.source}'
          jq -e '.materialized_roots | all(test("wrix-cache-serve") | not)' '${wrix.serviceImage.source}'
          backend="$(jq -r '.materialized_roots[] | select(test("-celler-"))' '${wrix.serviceImage.source}')"
          test -x "$backend/bin/cellerd"
          guard="$(jq -r '.materialized_roots[] | select(test("-util-linux-"))' '${wrix.serviceImage.source}')"
          test -x "$guard/bin/flock"
          jq -e '.config.labels["wrix.service.supervision"] == "2"' '${wrix.serviceImage.source}'
          supervisor="$(jq -r '.materialized_roots[] | select(endswith("-wrix-service-supervisor"))' '${wrix.serviceImage.source}')"
          test -x "$supervisor/bin/wrix-service-supervisor"
          test ! -L "$supervisor/bin/wrix-service-supervisor"
          test ! -e "$supervisor/bin/wrix-host-controller"
        ''
      else
        ''
          mkdir archive root
          tar -xf '${wrix.serviceImage.source}' -C archive
          while IFS= read -r layer; do
            tar -xf "archive/$layer" -C root
          done < <(jq -r '.[0].Layers[]' archive/manifest.json)
          test -L root/bin/cellerd
          test -L root/bin/flock
          test -L root/bin/wrix-service-supervisor
          target="$(readlink root/bin/wrix-service-supervisor)"
          test -f "root$target"
          test ! -L "root$target"
          config="$(jq -r '.[0].Config' archive/manifest.json)"
          jq -e '.config.Labels["wrix.service.supervision"] == "2"' "archive/$config"
          test ! -e root/bin/wrix-cache-serve
        ''
    }
    touch "$out"
  ''
