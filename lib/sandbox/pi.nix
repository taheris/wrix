{ pkgs }:
let
  # Remove this pin and its build overrides once nixpkgs provides Pi >= 1.0.0; keep the auth patch below.
  pi = pkgs.pi-coding-agent.overrideAttrs (
    finalAttrs: old: {
      version = "1.0.0";
      src = pkgs.fetchFromGitHub {
        owner = "earendil-works";
        repo = "pi";
        tag = "v${finalAttrs.version}";
        hash = "sha256-CGznIVHXG6gr2F8vzHcR/v4P9xJgZHeMTt/CJ/kB78o=";
      };
      npmDepsHash = "sha256-ndEvWdB6sa5nNNtabk2OMZKUFG9x3op185deZHxFnXk=";
      npmDeps = pkgs.fetchNpmDeps {
        name = "pi-coding-agent-${finalAttrs.version}-npm-deps";
        inherit (finalAttrs) src;
        hash = finalAttrs.npmDepsHash;
      };
      modelData = pkgs.fetchurl {
        url = "https://registry.npmjs.org/@earendil-works/pi-ai/-/pi-ai-${finalAttrs.version}.tgz";
        hash = "sha256-85uZwpuFmPF1sQhA5dKoGYPnwM5crk19+DoQB0R9LCs=";
      };
      buildPhase = ''
        runHook preBuild
        npm run build:offline
        runHook postBuild
      '';
      postInstall = old.postInstall + ''
        for ws in codemode mcp; do
          cp -r "packages/$ws" "$out/lib/node_modules/pi-monorepo/node_modules/@earendil-works/pi-$ws"
        done
      '';
    }
  );
in
# Pi's sync readers and async refreshes must share a target and stale timeout.
pi.overrideAttrs (old: {
  postPatch = (old.postPatch or "") + ''
    substituteInPlace packages/coding-agent/src/core/auth-storage.ts \
      --replace-fail 'lockfile.lockSync(path, { realpath: false })' \
        'lockfile.lockSync(path, { realpath: true, stale: 30_000 })' \
      --replace-fail 'realpath: false' 'realpath: true'
  '';
})
