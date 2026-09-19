# Pi's sync readers and async refreshes must share a target and stale timeout.
{ pkgs }:
pkgs.pi-coding-agent.overrideAttrs (old: {
  postPatch = (old.postPatch or "") + ''
    substituteInPlace packages/coding-agent/src/core/auth-storage.ts \
      --replace-fail 'lockfile.lockSync(path, { realpath: false })' \
        'lockfile.lockSync(path, { realpath: true, stale: 30_000 })' \
      --replace-fail 'realpath: false' 'realpath: true'
  '';
})
