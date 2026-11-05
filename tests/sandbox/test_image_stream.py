"""Bounded regressions for the real dockerTools threaded layer pipe."""

import hashlib
import io
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest

sys.dont_write_bytecode = True


def child(source, mode):
    namespace = runpy.run_path(source, run_name="docker_stream_test")
    layer = namespace["add_layer_dir"]
    globals_ = layer.__globals__
    archive = globals_["archive_paths_to"]
    entered = threading.Event()
    calls = 0

    def injected(output, *args):
        nonlocal calls
        calls += 1
        if calls == 2 and mode in ("producer", "consumer"):
            print("entered injected " + mode, file=sys.stderr, flush=True)
        if calls == 2 and mode == "producer":
            raise OSError("injected producer failure")
        if calls == 2 and mode == "consumer":
            output.write(b"x" * 4096)
            output.flush()
            entered.set()
            output.write(b"x" * (4 * 1024 * 1024))
            return
        archive(output, *args)
        if calls == 2 and mode == "late-producer":
            raise OSError("injected late producer failure")

    globals_["archive_paths_to"] = injected

    class FailedConsumer:
        def addfile(self, _info, _reader):
            if not entered.wait(1):
                raise RuntimeError("producer did not enter")
            # Let the producer block holding the buffered writer's lock.
            time.sleep(0.05)
            raise OSError("injected consumer failure")

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "payload").write_bytes(b"payload" * 1024)
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w") as tar:
            consumer = FailedConsumer() if mode == "consumer" else tar
            info = layer(consumer, [str(root)], str(root), 1, 0, 0, "root", "root")
        if mode != "success":
            raise AssertionError("a failed producer/consumer was accepted")
        print(info.checksum, hashlib.sha256(output.getvalue()).hexdigest())


class ImageStreamTests(unittest.TestCase):
    def run_child(self, source, mode, timeout=3):
        return subprocess.run(
            [sys.executable, __file__, "--child", source, mode],
            capture_output=True,
            text=True,
            timeout=timeout,
        )

    def test_original_producer_failure_reproduces_hang(self):
        with self.assertRaises(subprocess.TimeoutExpired) as caught:
            self.run_child(UPSTREAM, "producer", timeout=1)
        self.assertIn(b"entered injected producer", caught.exception.stderr or b"")

    def test_original_consumer_failure_reproduces_hang(self):
        with self.assertRaises(subprocess.TimeoutExpired) as caught:
            self.run_child(UPSTREAM, "consumer", timeout=1)
        self.assertIn(b"entered injected consumer", caught.exception.stderr or b"")

    def test_producer_failure_is_nonzero_without_hanging(self):
        result = self.run_child(PATCHED, "producer")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("injected producer failure", result.stderr)

    def test_consumer_failure_is_nonzero_without_hanging(self):
        result = self.run_child(PATCHED, "consumer")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("injected consumer failure", result.stderr)

    def test_late_producer_failure_is_not_accepted(self):
        result = self.run_child(PATCHED, "late-producer")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("injected late producer failure", result.stderr)

    def test_successful_layer_bytes_and_checksums_are_unchanged(self):
        original = runpy.run_path(UPSTREAM, run_name="original_stream")
        patched = runpy.run_path(PATCHED, run_name="patched_stream")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "payload").write_bytes(b"unchanged payload\n" * 1024)
            (root / "empty").touch()
            (root / "link").symlink_to("payload")
            os.link(root / "payload", root / "hardlink")
            observed = []
            for module in (original, patched):
                output = io.BytesIO()
                with tarfile.open(fileobj=output, mode="w") as tar:
                    info = module["add_layer_dir"](
                        tar, [str(root)], str(root), 1, 0, 0, "root", "root"
                    )
                observed.append((info.size, info.checksum, output.getvalue()))
            self.assertEqual(observed[0], observed[1])
            self.assertGreater(observed[0][0], 0)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--child":
        child(sys.argv[2], sys.argv[3])
    else:
        UPSTREAM, PATCHED = sys.argv[1:3]
        unittest.main(argv=[sys.argv[0]])
