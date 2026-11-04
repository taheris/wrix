#!/usr/bin/env python3
"""The digest retains JSON string bytes while discarding Go's indentation."""

import hashlib
import json
import os
import runpy
import unittest


digest = runpy.run_path(os.environ["WRIX_CONFIG_DIGEST"])["digest"]


class ConfigDigestTest(unittest.TestCase):
    def test_indentation_is_ignored(self):
        compact = '{"rootfs":{"type":"layers","diff_ids":[]}}'
        self.assertEqual(
            digest(json.dumps(json.loads(compact), indent=4)),
            "sha256:" + hashlib.sha256(compact.encode()).hexdigest(),
        )

    def test_string_whitespace_and_escapes_are_retained(self):
        compact = r'{"Env":["SPACES=two  words","QUOTE=\" ","PATH=a\\b","ESCAPED=\u003c\u0026\u003e","UNICODE=λ"]}'
        pretty = compact.replace('{"Env":', '{\n    "Env": ')
        self.assertEqual(
            digest(pretty), "sha256:" + hashlib.sha256(compact.encode()).hexdigest()
        )

    def test_invalid_json_is_not_hashed(self):
        with self.assertRaises(json.JSONDecodeError):
            digest('{"broken":')


if __name__ == "__main__":
    unittest.main()
