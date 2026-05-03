# pi-mono coding agent vendored for the wrapix sandbox.
#
# The published @mariozechner/pi-coding-agent npm tarball ships a pre-built
# `dist/` (the `prepublishOnly` script runs `tsgo` before publish), so this
# package only resolves and stages dependencies — no JS build runs in Nix.
#
# Spec: specs/loom-agent.md § Pi Runtime Layer
# Audit: see SECURITY-AUDIT.md when bumping the version.
#
# Bumping the pin:
#   1. Update "@mariozechner/pi-coding-agent" version in ./package.json.
#   2. Re-run the install lifecycle audit:
#        cd lib/pi-mono
#        rm package-lock.json
#        npm install --omit=dev --ignore-scripts --package-lock-only
#        node -e 'for (const [p,i] of Object.entries(require("./package-lock.json").packages))
#          if (i.hasInstallScript) console.log(p);'
#      For each new entry, inspect the script source — refuse the bump if any
#      script reaches the network or does anything other than benign metadata
#      processing.
#   3. Set npmDepsHash below to lib.fakeHash, run `nix build .#pi-mono`, copy
#      the suggested hash into npmDepsHash.
{
  lib,
  buildNpmPackage,
  nodejs_22,
  makeWrapper,
}:

buildNpmPackage (_finalAttrs: {
  pname = "pi-mono";
  version = "0.72.1";

  nodejs = nodejs_22;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./package.json
      ./package-lock.json
    ];
  };

  npmDepsHash = "sha256-uC5KwDI39Bg8OKeZ9FNboE6ejyyXLfxyuJjKKuBbwJI=";

  # Tarball ships pre-built dist/cli.js — no JS build step.
  dontNpmBuild = true;

  # Skip npm lifecycle scripts. Audited for 0.72.1:
  #  - protobufjs postinstall:    benign version-scheme warning
  #  - koffi install (cnoke):     prebuild selection; native .node files are
  #                               already bundled and koffi's loader walks
  #                               build/koffi/<triplet>/ at runtime
  #  - @google/genai prepare:     only runs on git installs, not registry
  npmFlags = [ "--ignore-scripts" ];

  nativeBuildInputs = [ makeWrapper ];

  postInstall = ''
    mkdir -p "$out/bin"
    makeWrapper ${nodejs_22}/bin/node "$out/bin/pi" \
      --add-flags "$out/lib/node_modules/pi-mono-launcher/node_modules/@mariozechner/pi-coding-agent/dist/cli.js"
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    "$out/bin/pi" --version >/dev/null
    runHook postInstallCheck
  '';

  meta = {
    description = "pi-mono coding agent (RPC backend for the Loom harness)";
    homepage = "https://github.com/badlogic/pi-mono";
    license = lib.licenses.mit;
    mainProgram = "pi";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
})
