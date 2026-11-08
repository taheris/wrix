{
  pkgs,
  wrix,
  linuxPkgs,
}:

let
  inherit (builtins)
    all
    attrValues
    deepSeq
    getContext
    toJSON
    toString
    tryEval
    unsafeDiscardStringContext
    ;
  mkImage =
    profile: agent: content:
    let
      source =
        if profile == "rust" && agent == "direct" then
          pkgs.writeText "selected-image-source" content
        else
          pkgs.runCommand "unselected-image-${profile}-${agent}" { inherit content; } "exit 99";
      ref = "fixture-${profile}-${agent}:${builtins.hashString "sha256" content}";
      source_kind = "nix-descriptor";
    in
    {
      inherit
        source
        ref
        source_kind
        agent
        ;
      profileConfig = pkgs.writeText "selected-profile-config.json" (toJSON {
        schema = 1;
        image = { inherit source source_kind ref; };
        agent.kind = agent;
      });
    };
  matrix = builtins.listToAttrs (
    map
      (profile: {
        name = profile;
        value = builtins.listToAttrs (
          map
            (agent: {
              name = agent;
              value = mkImage profile agent "initial";
            })
            [
              "direct"
              "claude"
              "pi"
            ]
        );
      })
      [
        "base"
        "rust"
        "python"
      ]
  );
  catalogue = wrix.mkProfileCatalogue matrix;
  selected = catalogue.provisions.rust.direct;
  selectedChanged = wrix.mkProfileCatalogue (
    matrix
    // {
      rust = matrix.rust // {
        direct = mkImage "rust" "direct" "changed";
      };
    }
  );
  configOnlyChanged = wrix.mkProfileCatalogue (
    matrix
    // {
      rust = matrix.rust // {
        direct = matrix.rust.direct // {
          profileConfig = pkgs.writeText "changed-profile-config.json" (toJSON {
            schema = 1;
            image = { inherit (matrix.rust.direct) source source_kind ref; };
            agent.kind = "direct";
            profile.env.CONFIG_ONLY = "changed";
          });
        };
      };
    }
  );
  unselectedChanged = wrix.mkProfileCatalogue (
    matrix
    // {
      python = matrix.python // {
        pi = mkImage "python" "pi" "changed";
      };
    }
  );
  rejects =
    image: !(tryEval (deepSeq (wrix.mkProfileCatalogue { rust.direct = image; }).drvPath true)).success;
  realImages = builtins.mapAttrs (
    _name: profile:
    builtins.listToAttrs (
      map
        (agent: {
          name = agent;
          value =
            (wrix.mkSandbox (
              {
                inherit profile agent;
              }
              // pkgs.lib.optionalAttrs (agent == "direct") { agentPkg = linuxPkgs.hello; }
            )).image;
        })
        [
          "direct"
          "claude"
          "pi"
        ]
    )
  ) { inherit (wrix.profiles) base rust python; };
  realCatalogue = wrix.mkProfileCatalogue realImages;
  entries = catalogue.catalogue.profiles;
  context = getContext (toJSON catalogue.catalogue);
in
assert wrix.capabilities.profileCatalogue == 1;
assert all (entry: entry == { path = true; }) (attrValues context);
assert builtins.length (builtins.attrNames context) == 9;
assert entries.rust.direct.realization.path == unsafeDiscardStringContext (toString selected);
assert entries.rust.direct.realization.derivation == selected.drvPath;
assert catalogue.drvPath != selectedChanged.drvPath;
assert catalogue.drvPath != unselectedChanged.drvPath;
assert selected.drvPath == unselectedChanged.provisions.rust.direct.drvPath;
assert selected.drvPath != selectedChanged.provisions.rust.direct.drvPath;
assert selected.drvPath != configOnlyChanged.provisions.rust.direct.drvPath;
assert catalogue.drvPath != configOnlyChanged.drvPath;
assert realCatalogue.catalogue.profiles.base.direct.ref == realImages.base.direct.ref;
assert all (entry: entry == { path = true; }) (
  attrValues (getContext (toJSON realCatalogue.catalogue))
);
assert rejects (builtins.removeAttrs matrix.rust.direct [ "profileConfig" ]);
assert rejects (matrix.rust.direct // { agent = "pi"; });
assert rejects (matrix.rust.direct // { source_kind = "archive"; });
assert rejects (matrix.rust.direct // { ref = ""; });
assert rejects (matrix.rust.direct // { source = "/nix/store/unresolved-image"; });
assert rejects (matrix.rust.direct // { profileConfig = "/nix/store/unresolved-config"; });
assert
  !(tryEval (deepSeq (wrix.mkProfileCatalogue { rust.unknown = matrix.rust.direct; }).drvPath true))
  .success;
assert rejects (matrix.rust.direct // { ref = "${matrix.rust.direct.source}/runtime"; });
pkgs.runCommand "profile-catalogue-boundary"
  {
    nativeBuildInputs = [ pkgs.jq ];
    passthru = { inherit catalogue selected realCatalogue; };
  }
  ''
    set -euo pipefail
    for catalogue in '${catalogue}' '${realCatalogue}'; do
      jq -e '.schema == 1 and (.profiles | keys) == ["base", "python", "rust"] and ([.profiles[][]] | length) == 9' "$catalogue"
    done
    jq -e --arg selected '${unsafeDiscardStringContext (toString selected)}' \
      '.profiles.rust.direct.realization.path == $selected' '${catalogue}'
    jq -e 'keys == ["rust"] and (.rust | keys) == ["direct"]' '${selected}'
    configuration=$(jq -r .rust.direct.profile_config '${selected}')
    source=$(jq -r .rust.direct.source '${selected}')
    jq -e --arg source "$source" '.schema == 1 and .agent.kind == "direct" and .image.source == $source' "$configuration"
    [[ "$(< "$source")" == initial ]]
    touch "$out"
  ''
