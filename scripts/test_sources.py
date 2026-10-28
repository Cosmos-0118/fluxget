"""Black-box tests for site support: release APIs, torrents, media pages, share links.

A loopback server imitates GitHub Enterprise Server, GitLab, Gitea, torrent index
pages and download landing pages. aria2c and yt-dlp are replaced by a recording
stub (scripts/stub_backend.rs, built with rustc) so delegation is verified
without peers or a media site. Python stdlib only.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from urllib.parse import urlsplit

from fixture import Handler, Origin, payload

PARSER = argparse.ArgumentParser()
PARSER.add_argument("--binary", type=Path, required=True)
ARGS, REMAINING = PARSER.parse_known_args()
BINARY = ARGS.binary.resolve()
EXE = ".exe" if os.name == "nt" else ""
DEFAULT = payload(2 * 1024 * 1024)
DEFAULT_SHA = hashlib.sha256(DEFAULT).hexdigest()
HASH = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a"
MAGNET = f"magnet:?xt=urn:btih:{HASH}&dn=Test+Linux+ISO&tr=udp%3A%2F%2Ftracker.invalid%3A1337"
TORRENT = b"d8:announce22:http://tracker.invalid4:infod6:lengthi5e4:name5:a.bin12:piece lengthi16384e6:pieces20:" + bytes(20) + b"ee"


class SiteHandler(Handler):
    def reply(self, body, kind="text/html", status=200, headers=()):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        for key, value in headers:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def json(self, value, status=200):
        self.reply(json.dumps(value), "application/json", status)

    def release(self, tag, assets):
        base = self.server.url
        return {
            "tag_name": tag,
            "prerelease": False,
            "assets": [
                {"name": name, "size": len(DEFAULT), "state": "uploaded", "digest": digest,
                 "browser_download_url": f"{base}/acme/tool/releases/download/{tag}/{name}"}
                for name, digest in assets
            ],
        }

    def serve(self):
        path = urlsplit(self.path).path
        base = self.server.url
        if path == "/api/v3/repos/acme/tool/releases/latest":
            return self.json(self.release("v1.0.0", [
                ("tool-windows-x64.zip", f"sha256:{DEFAULT_SHA}"),
                ("tool-bad-digest.zip", "sha256:" + "0" * 64),
                ("tool-linux-x64.tar.gz", ""),
            ]))
        if path == "/api/v3/repos/acme/tool/releases":
            return self.json([self.release("v1.1.0", [("tool-new.zip", "")]),
                              {**self.release("v1.0.0", [("tool-old.zip", "")]), "draft": True},
                              self.release("v0.9.0", [("tool-older.zip", "")])])
        if path == "/api/v3/repos/acme/tool/releases/tags/v0.9.0":
            return self.json(self.release("v0.9.0", [("tool-older.zip", "")]))
        if path == "/api/v3/repos/acme/limited/releases/latest":
            return self.reply("rate limited", "text/plain", 403)
        if path == "/acme/limited/releases/latest":
            return self.reply("", status=302, headers=[("Location", "/acme/limited/releases/tag/v2.0")])
        if path == "/acme/limited/releases/tag/v2.0":
            return self.reply("<html>release page without assets</html>")
        if path == "/acme/limited/releases/expanded_assets/v2.0":
            return self.reply('<a href="/acme/limited/releases/download/v2.0/limited-linux">limited-linux</a>'
                              '<a href="/acme/limited/archive/refs/tags/v2.0.zip">Source code (zip)</a>')
        if path == "/api/v3/repos/acme/empty/releases/latest":
            return self.json({"message": "Not Found"}, 404)
        if path == "/api/v3/repos/acme/empty/releases":
            return self.json([])
        if path == "/api/v3/repos/acme/empty":
            return self.json({"default_branch": "trunk"})
        if path.startswith("/api/v3/"):
            return self.json({"message": "Not Found"}, 404)
        if path == "/api/v4/projects/group%2Fproj/releases":
            return self.json([{"tag_name": "v7", "assets": {"links": [
                {"name": "proj-setup.exe", "url": f"{base}/dl/proj-setup.exe",
                 "direct_asset_url": f"{base}/dl/proj-setup.exe"}], "sources": []}}])
        if path == "/api/v1/repos/o/r/releases":
            return self.json([{"tag_name": "v3", "assets": [
                {"name": "r.deb", "size": 10, "browser_download_url": f"{base}/dl/r.deb"}]}])
        if path == "/torrents":
            return self.reply(
                f'<a href="{MAGNET.replace("&", "&amp;")}"><i></i></a>'
                '<a href="/files/ubuntu.torrent">ubuntu.torrent</a>'
                '<a href="/dl/get?id=1">Download torrent</a>')
        if path in ("/files/ubuntu.torrent", "/dl/get"):
            return self.reply(TORRENT, "application/x-bittorrent")
        if path == "/login.torrent":
            return self.reply("<html>please log in</html>", "application/x-bittorrent")
        if path == "/refresh":
            return self.reply('<meta http-equiv="refresh" content="1; url=/file.bin?size=4096">Starting download')
        if path == "/video":
            return self.reply('<video src="/clip.mp4"></video><video><source src="/live/stream.m3u8"></video>')
        if path == "/acme/tool/raw/main/README.md":
            return self.reply(b"# readme\n", "text/plain")
        if path.startswith(("/acme/", "/dl/")):
            return super().serve()
        return super().serve()


class SiteServer(Origin):
    def __init__(self):
        super().__init__()
        self.RequestHandlerClass = SiteHandler


def build_stub(folder):
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo" / "bin" / f"rustc{EXE}")
    source = Path(__file__).with_name("stub_backend.rs")
    aria2 = Path(folder) / f"aria2c{EXE}"
    subprocess.run([rustc, "-O", str(source), "-o", str(aria2)], check=True, capture_output=True)
    shutil.copy(aria2, Path(folder) / f"yt-dlp{EXE}")


class Sources(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import threading
        cls.server = SiteServer()
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.tools = tempfile.TemporaryDirectory(prefix="fluxget-stubs-")
        build_stub(cls.tools.name)
        cls.empty = tempfile.TemporaryDirectory(prefix="fluxget-nopath-")

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.tools.cleanup()
        cls.empty.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fluxget-sources-")
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.log = self.folder / "stub.log"
        self.output = self.folder / "result.bin"

    def run_cli(self, *args, success=True, path=None, **env):
        full = {**os.environ, "FLUXGET_GITHUB_URL": self.server.url, "STUB_LOG": str(self.log),
                "PATH": path if path is not None else self.tools.name, **{k: str(v) for k, v in env.items()}}
        full.pop("GITHUB_TOKEN", None)
        full.pop("GH_TOKEN", None)
        result = subprocess.run([str(BINARY), *map(str, args)], capture_output=True, text=True, env=full, timeout=60)
        self.assertEqual(result.returncode == 0, success, result.stderr + result.stdout)
        return result

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def resolve(self, url):
        return json.loads(self.run_cli("resolve", url).stdout)

    def verify(self, expected=DEFAULT, path=None):
        self.assertEqual((path or self.output).read_bytes(), expected)

    # ---- GitHub-style release pages -------------------------------------------------
    def test_github_latest_release_lists_assets_with_digests(self):
        for url in (f"{self.server.url}/acme/tool", f"{self.server.url}/acme/tool/releases/latest"):
            found = self.resolve(url)
            names = [c["label"] for c in found]
            self.assertTrue(names[0].startswith("tool-windows-x64.zip (2.0 MiB)"), names)
            self.assertEqual(found[0]["sha256"], DEFAULT_SHA)
            self.assertNotIn("sha256", found[2])
            self.assertTrue(any(n.startswith("Source code (zip)") for n in names))

    def test_published_digest_is_verified_during_download(self):
        self.run_cli(f"{self.server.url}/acme/tool", "--select", 1, "-o", self.output, "--json")
        self.verify()

    def test_wrong_published_digest_blocks_publication(self):
        result = self.run_cli(f"{self.server.url}/acme/tool", "--select", 2, "-o", self.output, "--json", success=False)
        self.assertIn("SHA-256 mismatch", result.stderr)
        self.assertFalse(self.output.exists())

    def test_explicit_sha256_flag_wins_over_published_digest(self):
        self.run_cli(f"{self.server.url}/acme/tool", "--select", 2, "-o", self.output, "--json", "--sha256", DEFAULT_SHA)
        self.verify()

    def test_release_list_tag_and_unstable_labels(self):
        found = self.resolve(f"{self.server.url}/acme/tool/releases")
        labels = [c["label"] for c in found]
        self.assertTrue(any(l.startswith("[v1.1.0] tool-new.zip") for l in labels))
        self.assertTrue(any(l.startswith("[v0.9.0] tool-older.zip") for l in labels))
        self.assertFalse(any("v1.0.0" in l for l in labels), "drafts must be hidden")
        tagged = self.resolve(f"{self.server.url}/acme/tool/releases/tag/v0.9.0")
        self.assertTrue(tagged[0]["label"].startswith("tool-older.zip"))

    def test_rate_limited_api_falls_back_to_release_page(self):
        found = self.resolve(f"{self.server.url}/acme/limited/releases/latest")
        self.assertEqual([c["label"] for c in found][0], "limited-linux")
        result = self.run_cli(f"{self.server.url}/acme/limited", "--select", 1, "-o", self.output, "--json")
        self.assertIn("GITHUB_TOKEN", result.stderr)
        self.verify()

    def test_repository_without_releases_offers_source_archive(self):
        found = self.resolve(f"{self.server.url}/acme/empty")
        self.assertTrue(found[0]["url"].endswith("/acme/empty/archive/refs/heads/trunk.zip"))

    def test_blob_page_downloads_raw_file(self):
        self.run_cli(f"{self.server.url}/acme/tool/blob/main/README.md", "-o", self.output, "--json")
        self.assertEqual(self.output.read_bytes(), b"# readme\n")

    def test_gitlab_and_gitea_releases(self):
        port = self.server.server_port
        gitlab = self.resolve(f"http://localhost:{port}/group/proj/-/releases")
        self.assertEqual(gitlab[0]["label"], "[v7] proj-setup.exe")
        gitea = self.resolve(f"http://localhost:{port}/o/r/releases")
        self.assertEqual(gitea[0]["label"], "[v3] r.deb (10 B)")
        self.run_cli(f"http://localhost:{port}/group/proj/-/releases", "-o", self.output, "--json")
        self.verify()

    # ---- torrents ---------------------------------------------------------------------
    def test_magnet_link_is_downloaded_by_aria2(self):
        target = self.folder / "torrents"
        result = self.run_cli(MAGNET, "-o", target, "--json")
        done = json.loads(result.stdout)
        self.assertEqual((done["backend"], done["kind"], done["status"]), ("aria2", "torrent", "completed"))
        self.assertEqual(done["source"], f"magnet:?xt=urn:btih:{HASH}")
        [call] = self.calls()
        self.assertEqual(call["args"][-1], MAGNET)
        self.assertEqual(call["args"][:2], ["--dir", str(target)])
        self.assertIn("--seed-time=0", call["args"])
        self.assertNotIn("stub backend ran", result.stdout, "backend output must stay off stdout in --json mode")

    def test_seed_time_is_configurable(self):
        self.run_cli(MAGNET, "--seed-minutes", 5, "--quiet")
        self.assertIn("--seed-time=5", self.calls()[0]["args"])

    def test_torrent_page_lists_magnet_and_torrent_files(self):
        found = self.resolve(f"{self.server.url}/torrents")
        self.assertEqual([c["kind"] for c in found], ["torrent", "torrent", "http"][:len(found)])
        self.assertEqual(found[0]["label"], "Test Linux ISO")
        failure = self.run_cli(f"{self.server.url}/torrents", "--json", success=False)
        self.assertIn("[torrent]", failure.stderr)
        self.assertIn(f"magnet:?xt=urn:btih:{HASH}", failure.stderr)
        self.assertNotIn("tracker.invalid", failure.stderr, "magnet trackers should not flood the list")

    def test_torrent_file_is_fetched_natively_then_given_to_aria2(self):
        self.run_cli(f"{self.server.url}/files/ubuntu.torrent", "-o", self.folder / "out", "--quiet")
        [call] = self.calls()
        local = call["args"][-1]
        self.assertTrue(local.endswith(".torrent"))
        self.assertNotIn("http", local)
        self.assertTrue(call["files"][local].startswith("d8:announce"))

    def test_torrent_detected_by_content_type_not_suffix(self):
        self.run_cli(f"{self.server.url}/dl/get?id=1", "--quiet")
        self.assertTrue(self.calls()[0]["args"][-1].endswith(".torrent"))

    def test_local_torrent_file(self):
        local = self.folder / "local.torrent"
        local.write_bytes(TORRENT)
        self.run_cli(local, "--quiet")
        self.assertEqual(self.calls()[0]["args"][-1], str(local))

    def test_login_page_posing_as_torrent_is_rejected(self):
        result = self.run_cli(f"{self.server.url}/login.torrent", "--quiet", success=False)
        self.assertIn("not a valid .torrent", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_direct_flag_saves_the_torrent_file_itself(self):
        self.run_cli(f"{self.server.url}/files/ubuntu.torrent", "--direct", "-o", self.output, "--json")
        self.assertEqual(self.output.read_bytes(), TORRENT)
        self.assertEqual(self.calls(), [])

    def test_missing_aria2_and_failing_backend_are_reported(self):
        result = self.run_cli(MAGNET, "--quiet", success=False, path=self.empty.name)
        self.assertIn("aria2", result.stderr)
        self.assertIn("fluxget doctor", result.stderr)
        result = self.run_cli(MAGNET, "--quiet", success=False, STUB_EXIT=3)
        self.assertIn("exited with", result.stderr)

    # ---- media pages ------------------------------------------------------------------
    def test_media_page_goes_to_ytdlp_for_one_item(self):
        url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLsomething"
        done = json.loads(self.run_cli(url, "-o", self.folder / "videos", "--json").stdout)
        self.assertEqual((done["backend"], done["kind"]), ("yt-dlp", "media"))
        # fluxget also probes aria2c with --version to decide whether to hand plain files to it
        [call] = [c for c in self.calls() if c["args"] != ["--version"]]
        self.assertIn("--no-playlist", call["args"])
        self.assertEqual(call["args"][call["args"].index("--concurrent-fragments") + 1], "16")
        self.assertEqual(call["args"][call["args"].index("--downloader") + 1], "http,https:aria2c")
        self.assertEqual(call["args"][call["args"].index("--paths") + 1], str(self.folder / "videos"))
        self.assertEqual(call["args"][-2:], ["--", url])

    def test_page_with_video_and_stream_playlist(self):
        found = self.resolve(f"{self.server.url}/video")
        self.assertEqual([(c["url"].rsplit("/", 1)[1], c["kind"]) for c in found],
                         [("clip.mp4", "http"), ("stream.m3u8", "media")])

    # ---- share links and landing pages --------------------------------------------------
    def test_cloud_share_links_resolve_without_network(self):
        dropbox = self.resolve("https://www.dropbox.com/scl/fi/abc/f.zip?rlkey=k&dl=0")
        self.assertEqual(dropbox[0]["url"], "https://www.dropbox.com/scl/fi/abc/f.zip?rlkey=k&dl=1")
        drive = self.resolve("https://drive.google.com/file/d/1AbC_dEf/view?usp=sharing")
        self.assertEqual(drive[0]["url"], "https://drive.usercontent.google.com/download?id=1AbC_dEf&export=download&confirm=t")
        blocked = self.run_cli("https://mega.nz/file/abc#key", success=False)
        self.assertIn("end-to-end encrypted", blocked.stderr)

    def test_meta_refresh_landing_page_downloads_the_target(self):
        self.run_cli(f"{self.server.url}/refresh", "-o", self.output, "--json")
        self.verify(payload(4096))

    # ---- command-line behaviour ---------------------------------------------------------------
    def test_scheme_is_optional(self):
        self.run_cli(f"127.0.0.1:{self.server.server_port}/file.bin?size=4096", "-o", self.output, "--json")
        self.verify(payload(4096))

    def test_resolved_file_is_probed_only_once(self):
        route = "/file.bin?size=4096&probe=once"
        self.run_cli(self.server.url + route, "-o", self.output, "--json")
        probes = [n for (path, span), n in self.server.hits.items() if path == route and span == "bytes=0-0"]
        self.assertEqual(probes, [1])


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], *REMAINING], verbosity=2)
