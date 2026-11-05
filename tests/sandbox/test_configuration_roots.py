"""Configuration-root resolution, independent of Nix evaluator/store warmth."""

import importlib.util
from pathlib import Path
import sys
import unittest

module_path = Path(sys.argv.pop(1)) if len(sys.argv) > 1 else (
    Path(__file__).resolve().parents[2] / "lib/sandbox/configuration-roots.py"
)
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("configuration_roots", module_path)
roots = importlib.util.module_from_spec(spec)
spec.loader.exec_module(roots)

A = "/nix/store/" + "a" * 32 + "-configuration.js"
B = "/nix/store/" + "b" * 32 + "-tool?native-name"


class ConfigurationRoots(unittest.TestCase):
    def test_exact_sorted_unique_roots(self):
        self.assertEqual(roots.resolve([B, A, A], [B, A]), [A, B])

    def test_annotation_uses_native_closure_name(self):
        self.assertEqual(roots.resolve([A + "?annotation"], [A]), [A])

    def test_question_mark_in_native_name_is_preserved(self):
        self.assertEqual(roots.resolve([B], [B]), [B])

    def test_absent_literal_is_not_implicit_authority(self):
        with self.assertRaisesRegex(ValueError, "outside its input closure"):
            roots.resolve([A], [B])

    def test_no_closure_or_data_paths_are_silently_selected(self):
        self.assertEqual(roots.resolve([], [A, B]), [])

    def test_malformed_tokens_and_graphs_fail(self):
        for candidates, closure in [([A + "/../peer"], [A]), ([A], ["/tmp/peer"]), ({}, [A]), ([None], [A])]:
            with self.subTest(candidates=candidates, closure=closure), self.assertRaises(ValueError):
                roots.resolve(candidates, closure)


if __name__ == "__main__":
    unittest.main()
