import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


root = Path(sys.argv[1]).resolve()
env = dict(os.environ)
for name in subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines():
    env.pop(name, None)


def nix(*args, **overrides):
    result = subprocess.run(["nix", *args], cwd=root, env=dict(env, **overrides),
                            text=True, capture_output=True, check=False)
    if result.returncode:
        sys.stderr.write(result.stderr)
        raise SystemExit(result.returncode)
    return json.loads(result.stdout)


def derivations(source):
    return nix("eval", "--impure", "--json", "--expr", '''
      let
        source = builtins.getEnv "WRIX_SOURCE_PROBE";
        flake = builtins.getFlake ("path:" + source);
        system = builtins.currentSystem;
        pkgs = import flake.inputs.nixpkgs { inherit system; config.allowUnfree = true; };
        rust = import (source + "/lib/services/rust.nix") {
          inherit pkgs;
          rustProfile = flake.legacyPackages.${system}.lib.profiles.rust;
        };
      in {
        launcher = flake.packages.${system}.wrix.drvPath;
        clippy = flake.legacyPackages.${system}.ciChecks.wrix-rust-clippy.drvPath;
        nextest = flake.legacyPackages.${system}.ciChecks.wrix-rust-nextest.drvPath;
        dependencies = rust.cargoArtifacts.drvPath;
      }
    ''', WRIX_SOURCE_PROBE=str(source))


def perturb(path):
    path.chmod(path.stat().st_mode | 0o200)
    with path.open("a") as handle:
        handle.write("\n// Source-isolation probe.\n")


metadata = nix("flake", "metadata", "--json", "--no-write-lock-file", "git+file://" + str(root))
with tempfile.TemporaryDirectory(prefix="wrix-source-isolation-") as temporary:
    source = Path(temporary).resolve() / "source"
    shutil.copytree(metadata["path"], source, symlinks=True)
    original = derivations(source)
    perturb(source / "crates/wrix-cli/tests/arguments.rs")
    integration = derivations(source)
    assert original["launcher"] == integration["launcher"], "integration-test edit rebuilt the packaged launcher"
    assert original["dependencies"] == integration["dependencies"], "integration-test edit rebuilt Cargo dependencies"
    for check in ("clippy", "nextest"):
        assert original[check] != integration[check], f"integration-test edit did not invalidate {check}"
    perturb(source / "crates/wrix-cli/src/main.rs")
    production = derivations(source)
    assert integration["launcher"] != production["launcher"], "production edit failed to invalidate the launcher"
    assert integration["dependencies"] == production["dependencies"], "production edit rebuilt Cargo dependencies"
    for check in ("clippy", "nextest"):
        assert integration[check] != production[check], f"production edit did not invalidate {check}"
    print(json.dumps({"original": original, "integration_edit": integration, "production_edit": production}))
