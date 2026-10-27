"""Black-box tests against a real HTTP socket and the built CLI. Python stdlib."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

from fixture import payload, start

PARSER = argparse.ArgumentParser()
PARSER.add_argument("--binary", type=Path, required=True)
ARGS, REMAINING = PARSER.parse_known_args()
BINARY = ARGS.binary.resolve()


class Downloads(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fluxget-test-")
        self.addCleanup(self.temp.cleanup)
        self.output = Path(self.temp.name) / "result.bin"
        self.server.version = 1

    def call(self, *args, success=True):
        result = subprocess.run([str(BINARY), *map(str, args)], capture_output=True, text=True, timeout=40)
        self.assertEqual(result.returncode == 0, success, result.stderr + result.stdout)
        return result

    def get(self, route="/file.bin", *flags, success=True):
        return self.call(self.server.url + route, "-o", self.output, "--direct", "--json", *flags, success=success)

    def verify(self, size=2 * 1024 * 1024):
        self.assertEqual(hashlib.sha256(self.output.read_bytes()).hexdigest(), hashlib.sha256(payload(size)).hexdigest())

    def test_parallel_and_sha256(self):
        expected = hashlib.sha256(payload(2 * 1024 * 1024)).hexdigest()
        result = json.loads(self.get("/file.bin", "--sha256", expected, "--chunk-kib", "64").stdout)
        self.assertEqual(result["strategy"], "adaptive-ranges")
        self.verify()
        self.assertFalse(Path(str(self.output) + ".fluxget.json").exists())

    def test_redirect(self):
        self.get("/redirect")
        self.verify()

    def test_no_range_fallback(self):
        result = json.loads(self.get("/norange").stdout)
        self.assertEqual(result["strategy"], "single-stream")
        self.verify()

    def test_missing_validator_fallback(self):
        result = json.loads(self.get("/noetag").stdout)
        self.assertEqual(result["strategy"], "single-stream")
        self.verify()

    def test_weak_validator_fallback(self):
        self.assertEqual(json.loads(self.get("/weak").stdout)["strategy"], "single-stream")
        self.verify()

    def test_unknown_size_stream(self):
        self.get("/unknown")
        self.verify()

    def test_empty_file(self):
        self.get("/file.bin?size=0")
        self.assertEqual(self.output.read_bytes(), b"")

    def test_wrong_range_rejected(self):
        self.get("/badrange", "--chunk-kib", "64", success=False)
        self.assertFalse(self.output.exists())

    def test_dropped_body_retried(self):
        self.get("/truncate", "--chunk-kib", "1024")
        self.verify()

    def test_permanent_truncation_rejected(self):
        self.get("/always-truncate", "--retries", "0", success=False)
        self.assertFalse(self.output.exists())

    def test_retry_after(self):
        began = time.perf_counter()
        result = json.loads(self.get("/rate?size=2097280", "--chunk-kib", "1024").stdout)
        self.assertGreater(time.perf_counter() - began, 1.0)
        self.assertGreater(result["concurrency_changes"], 0)
        self.verify(2097280)

    def test_checksum_failure_keeps_partial(self):
        self.get("/file.bin", "--sha256", "0" * 64, success=False)
        self.assertFalse(self.output.exists())
        self.assertTrue(Path(str(self.output) + ".fluxget.part").exists())

    def test_overwrite_protection_and_force(self):
        self.output.write_bytes(b"keep")
        self.get(success=False)
        self.assertEqual(self.output.read_bytes(), b"keep")
        self.get("/file.bin", "--force")
        self.verify()

    def test_encoded_response_rejected(self):
        self.get("/encoded", success=False)
        self.assertFalse(self.output.exists())

    def test_page_discovery_and_selection(self):
        self.call("get", self.server.url + "/page", "-o", self.output, "--json")
        self.verify()
        candidates = json.loads(self.call("resolve", self.server.url + "/ambiguous").stdout)
        self.assertEqual(len(candidates), 2)
        self.call("get", self.server.url + "/ambiguous", "-o", self.output.with_suffix(".other"), success=False)

    def test_inspect(self):
        info = json.loads(self.call("inspect", self.server.url + "/file.bin").stdout)
        self.assertTrue(info["ranges"])
        self.assertEqual(info["size"], 2 * 1024 * 1024)

    def interrupt(self):
        url = self.server.url + "/file.bin?size=8388608&cap=2"
        process = subprocess.Popen([str(BINARY), url, "-o", str(self.output), "--direct", "--chunk-kib", "256", "--json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        cp_path = Path(str(self.output) + ".fluxget.json")
        deadline = time.monotonic() + 15
        try:
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    stdout, stderr = process.communicate()
                    self.fail(f"Downloader exited before interruption: {stdout!r} {stderr!r}")
                if cp_path.exists():
                    cp = json.loads(cp_path.read_text())
                    if sum(c is not None for c in cp["chunks"]) >= 3:
                        break
                time.sleep(0.02)
            else:
                self.fail("No checkpoint committed within 15 seconds")
        finally:
            process.kill()
            process.communicate(timeout=5)
        return cp_path

    def test_kill_resume_and_corruption_repair(self):
        cp_path = self.interrupt()
        cp = json.loads(cp_path.read_text())
        index = next(i for i, h in enumerate(cp["chunks"]) if h)
        with Path(str(self.output) + ".fluxget.part").open("r+b") as file:
            file.seek(index * cp["chunk_size"])
            file.write(b"corrupt")
        result = json.loads(self.call("resume", self.output, "--json").stdout)
        self.assertGreater(result["resumed_bytes"], 0)
        self.assertLess(result["resumed_bytes"], 8 * 1024 * 1024)
        self.verify(8 * 1024 * 1024)

    def test_changed_remote_refuses_resume(self):
        self.interrupt()
        self.server.version = 2
        self.call("resume", self.output, "--json", success=False)
        self.assertFalse(self.output.exists())

    def test_exclusive_output_lock(self):
        # Keep the first process alive until a competing process attempts the same output.
        process = subprocess.Popen([str(BINARY), self.server.url + "/file.bin?size=8388608&cap=1", "-o", str(self.output), "--direct", "--json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 10
            while not Path(str(self.output) + ".fluxget.json").exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            result = self.call("resume", self.output, "--json", success=False)
            self.assertIn("owns this output", result.stderr)
        finally:
            process.kill()
            process.communicate(timeout=5)

    def test_sensitive_header_redirect_blocked(self):
        self.get("/crossredirect", "-H", "X-Api-Key: private", success=False)

    def test_bad_cli_input(self):
        self.get("/file.bin", "-j", "0", success=False)
        self.get("/file.bin", "--sha256", "short", success=False)
        self.get("/file.bin", "-H", "Range: bytes=0-1", success=False)


if __name__ == "__main__":
    unittest.main(argv=[__file__, *REMAINING], verbosity=2)
