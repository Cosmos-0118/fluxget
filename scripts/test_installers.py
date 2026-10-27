"""Exercise installers in temporary directories, including updates and failures."""
import functools
import hashlib
import http.server
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import threading
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
WINDOWS = os.name == "nt"


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


class Installers(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        patterns = ["fluxget-windows-*.zip"] if WINDOWS else ["fluxget-*.tar.gz"]
        archives = [p for pattern in patterns for p in (ROOT / "dist").glob(pattern)]
        if not archives:
            raise RuntimeError("Build the native binary and run scripts/package.py first")
        cls.archive = max(archives, key=lambda p: p.stat().st_mtime_ns)
        cls.binary_name = "fluxget.exe" if WINDOWS else "fluxget"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fluxget-install-test-")
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.package = self.folder / "package with spaces"
        self.package.mkdir()
        if WINDOWS:
            with zipfile.ZipFile(self.archive) as archive:
                archive.extractall(self.package)
        else:
            with tarfile.open(self.archive) as archive:
                archive.extractall(self.package, filter="data")
        self.install_root = self.folder / "installed version with spaces"
        self.target = self.install_root / "bin" / self.binary_name
        self.env = os.environ.copy()
        self.env["HOME"] = str(self.folder / "test home")
        self.env["SHELL"] = "/bin/zsh"
        Path(self.env["HOME"]).mkdir()

    def install(self, package=None, success=True, remote=None):
        package = package or self.package
        if WINDOWS:
            command = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(package / "install.ps1"), "-InstallDir", str(self.install_root), "-NoModifyPath"]
            if remote:
                command.extend(["-ReleaseBaseUrl", remote])
        else:
            command = ["bash", str(package / "install.sh"), "--prefix", str(self.install_root)]
            if remote:
                command.extend(["--release-base-url", remote])
        result = subprocess.run(command, env=self.env, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_fresh_install_and_run(self):
        self.install()
        result = subprocess.run([str(self.target), "--version"], capture_output=True, text=True, check=True)
        self.assertTrue(result.stdout.startswith("fluxget "))
        self.assertTrue((self.install_root / "LICENSE").is_file())

    def test_reinstall_identical_binary_is_noop(self):
        self.install()
        before = self.target.stat().st_mtime_ns
        self.install()
        self.assertEqual(before, self.target.stat().st_mtime_ns)
        if not WINDOWS:
            for profile in [".profile", ".zprofile", ".zshrc"]:
                self.assertEqual((Path(self.env["HOME"]) / profile).read_text().count("# fluxget installer"), 1)

    def test_update_replaces_existing_binary_and_keeps_downloads(self):
        self.target.parent.mkdir(parents=True)
        self.target.write_bytes(b"previous installed version")
        download = self.install_root / "keep.iso"
        checkpoint = self.install_root / "keep.iso.fluxget.json"
        download.write_bytes(b"downloaded content")
        checkpoint.write_text("saved progress")
        self.install()
        self.assertEqual(self.target.read_bytes(), (self.package / self.binary_name).read_bytes())
        self.assertEqual(download.read_bytes(), b"downloaded content")
        self.assertEqual(checkpoint.read_text(), "saved progress")

    def test_bad_checksum_preserves_existing_installation(self):
        self.target.parent.mkdir(parents=True)
        self.target.write_bytes(b"keep existing install")
        with (self.package / self.binary_name).open("ab") as binary:
            binary.write(b"corrupted")
        self.install(success=False)
        self.assertEqual(self.target.read_bytes(), b"keep existing install")

    def host(self, corrupt_checksum=False, missing=False):
        web = self.folder / "web"
        assets = web / "latest" / "download"
        assets.mkdir(parents=True)
        if not missing:
            shutil.copy2(self.archive, assets / self.archive.name)
            checksum = "0" * 64 if corrupt_checksum else hashlib.sha256(self.archive.read_bytes()).hexdigest()
            (assets / (self.archive.name + ".sha256")).write_text(f"{checksum}  {self.archive.name}\n", encoding="ascii")
        handler = functools.partial(QuietHandler, directory=str(web))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        bootstrap = self.folder / "bootstrap only"
        bootstrap.mkdir()
        name = "install.ps1" if WINDOWS else "install.sh"
        shutil.copy2(ROOT / name, bootstrap / name)
        return bootstrap, f"http://127.0.0.1:{server.server_port}"

    def test_download_latest_release_and_install(self):
        package, remote = self.host()
        self.install(package, remote=remote)
        self.assertTrue(self.target.is_file())

    def test_bad_release_archive_checksum_keeps_old_install(self):
        self.target.parent.mkdir(parents=True)
        self.target.write_bytes(b"old working installation")
        package, remote = self.host(corrupt_checksum=True)
        self.install(package, remote=remote, success=False)
        self.assertEqual(self.target.read_bytes(), b"old working installation")

    def test_unavailable_release_keeps_old_install(self):
        self.target.parent.mkdir(parents=True)
        self.target.write_bytes(b"old working installation")
        package, remote = self.host(missing=True)
        self.install(package, remote=remote, success=False)
        self.assertEqual(self.target.read_bytes(), b"old working installation")

    @unittest.skipUnless(WINDOWS, "Windows execution policy check")
    def test_pasted_installer_under_restricted_policy(self):
        package, remote = self.host()
        env = self.env.copy()
        env.update(FLUXGET_TEST_SCRIPT=str(package / "install.ps1"), FLUXGET_TEST_PREFIX=str(self.install_root), FLUXGET_TEST_RELEASE_URL=remote)
        code = '& ([scriptblock]::Create([IO.File]::ReadAllText($env:FLUXGET_TEST_SCRIPT))) -InstallDir $env:FLUXGET_TEST_PREFIX -NoModifyPath -ReleaseBaseUrl $env:FLUXGET_TEST_RELEASE_URL'
        result = subprocess.run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Restricted", "-Command", code], env=env, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.target.is_file())

    @unittest.skipIf(WINDOWS, "Unix shell profile check")
    def test_shell_path_works_after_install(self):
        self.install()
        profile = Path(self.env["HOME"]) / ".zprofile"
        result = subprocess.run(["bash", "-c", 'source "$1"; command -v fluxget', "bash", str(profile)], env=self.env, capture_output=True, text=True, check=True)
        self.assertEqual(Path(result.stdout.strip()), self.target)


if __name__ == "__main__":
    unittest.main(verbosity=2)
