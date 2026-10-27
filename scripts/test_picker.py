"""Real arrow-key terminal tests on Unix; Windows is also exercised manually."""
import argparse
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

parser = argparse.ArgumentParser()
parser.add_argument("--binary", required=True, type=Path)
args, remaining = parser.parse_known_args()
BINARY = args.binary.resolve()


@unittest.skipIf(os.name == "nt", "PTY tests run on macOS/Linux; Windows ConPTY is checked locally")
class Picker(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.origin = subprocess.Popen([sys.executable, str(Path(__file__).with_name("fixture.py"))], stdout=subprocess.PIPE, text=True)
        cls.url = cls.origin.stdout.readline().strip()

    @classmethod
    def tearDownClass(cls):
        cls.origin.terminate()
        cls.origin.wait(timeout=5)

    def interact(self, keys):
        import pty
        with tempfile.TemporaryDirectory(prefix="fluxget-picker-") as folder:
            output = Path(folder) / "selected.bin"
            child, terminal = pty.fork()
            if child == 0:
                os.environ["TERM"] = "xterm-256color"
                os.execv(str(BINARY), [str(BINARY), self.url + "/homepage", "--output", str(output), "--quiet"])
            transcript = bytearray()
            sent = False
            status = None
            deadline = time.monotonic() + 20
            try:
                while time.monotonic() < deadline:
                    if select.select([terminal], [], [], 0.05)[0]:
                        try:
                            data = os.read(terminal, 65536)
                        except OSError:
                            data = b""
                        transcript.extend(data)
                    if not sent and b"Other file" in transcript:
                        os.write(terminal, keys)
                        sent = True
                    done, result = os.waitpid(child, os.WNOHANG)
                    if done:
                        status = os.waitstatus_to_exitcode(result)
                        break
                self.assertTrue(sent, transcript.decode(errors="replace"))
                self.assertEqual(status, 0, transcript.decode(errors="replace"))
                return bytes(transcript), output.read_bytes() if output.exists() else None
            finally:
                if status is None:
                    os.kill(child, signal.SIGKILL)
                    os.waitpid(child, 0)
                os.close(terminal)

    def test_down_enter_downloads_second_option(self):
        transcript, data = self.interact(b"\x1b[B\r")
        self.assertIn(b"other.zip?size=2048", transcript)
        self.assertEqual(data, bytes(range(256)) * 8)

    def test_escape_cancels_without_output(self):
        _, data = self.interact(b"\x1b")
        self.assertIsNone(data)


if __name__ == "__main__":
    unittest.main(argv=[__file__, *remaining], verbosity=2)
