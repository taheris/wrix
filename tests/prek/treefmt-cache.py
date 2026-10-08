import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT, FORMATTER = sys.argv[1:]
CLEAN = "# Fixture\n"


class FormatterCache(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wrix-treefmt-cache-")
        self.addCleanup(self.temp.cleanup)
        temp = Path(self.temp.name)
        self.root = temp / "repo"
        self.root.mkdir()
        cache = temp / "unavailable-cache"
        cache.write_text("A file cannot serve as a cache directory.\n")
        self.env = dict(os.environ)
        local_names = subprocess.check_output(
            ["git", "rev-parse", "--local-env-vars"], text=True
        ).splitlines()
        for name in local_names:
            self.env.pop(name, None)
        self.env.update(
            PATH=str(Path(FORMATTER).parent) + os.pathsep + self.env["PATH"],
            PREK_HOME=str(temp / "prek"),
            PREK_COLOR="never",
            XDG_CACHE_HOME=str(cache),
            TREEFMT_NO_CACHE="false",
            TREEFMT_CI="false",
        )
        shutil.copyfile(
            Path(ROOT) / ".pre-commit-config.yaml",
            self.root / ".pre-commit-config.yaml",
        )
        (self.root / "flake.nix").write_text("{}\n")
        self.readme = self.root / "README.md"
        self.readme.write_text(CLEAN)
        for args in (["git", "init", "-q"], ["git", "add", "."]):
            result = self.invoke(args)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def invoke(self, args):
        return subprocess.run(
            args, cwd=self.root, env=self.env, capture_output=True, text=True,
            check=False, timeout=20,
        )

    def hook(self):
        return self.invoke([
            "prek", "run", "treefmt", "--stage", "pre-commit",
            "--files", "README.md", "--no-progress",
        ])

    def test_fixture_prevents_cached_formatter_execution(self):
        result = self.invoke([FORMATTER, "--fail-on-change", "README.md"])
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cache", (result.stdout + result.stderr).lower())

    def test_clean_file_passes_hook_without_cache_access(self):
        result = self.hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.readme.read_text(), CLEAN)

    def test_formatting_change_still_fails_hook_without_cache_access(self):
        self.readme.write_text("#   Fixture\n")
        result = self.hook()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.readme.read_text(), CLEAN)
        rerun = self.hook()
        self.assertEqual(rerun.returncode, 0, rerun.stdout + rerun.stderr)


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0]])
