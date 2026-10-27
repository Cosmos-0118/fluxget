# fluxget

An adaptive, integrity-first download accelerator for Windows, macOS, and Linux.
Native Rust HTTP(S) engine; optional **aria2** and **yt-dlp** commands reuse mature
open-source implementations for torrents, Metalink, FTP/SFTP and media websites.

This is version **0.2.0**. Release packages are built and tested on Windows,
macOS (Apple Silicon and Intel), and Linux, for x86_64 and aarch64 where available.

## Install once, update with the same command

No Rust, Python, admin account, or compiler is required for prebuilt releases.
The installer automatically selects your OS and CPU, downloads the latest
release, checks its SHA-256, verifies the executable, and installs it for your
user account. Open a new terminal after installation and run `fluxget --help`.

**Windows — paste into PowerShell:**

```powershell
irm https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.ps1 | iex
```

**macOS or Linux — paste into Terminal:**

```sh
curl -fsSL https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.sh | bash
```

Prefer clicking? Download the matching ZIP from
[Releases](https://github.com/Cosmos-0118/fluxget/releases/latest), extract it,
then double-click **Install.cmd** on Windows or **Install.command** on macOS.
If macOS blocks the unsigned installer, use the Terminal command above.
Linux packages also include `install.sh`. Packages contain license notices and
both an archive checksum and an internal executable checksum. Checksums detect
corruption; they rely on the trusted GitHub release as the source.

**Already installed? Run the same command again to update.** An identical binary
is left in place; a changed binary replaces the existing installation only after
verification. A failed download, checksum or version check leaves the old binary
intact. PATH entries are not duplicated. Downloads and resume checkpoints are
preserved; updating does not uninstall your data or create another installation.
An explicitly selected older release can also replace the installed version.

Default locations: `%LOCALAPPDATA%\Programs\fluxget\bin` on Windows and
`~/.local/bin` on macOS/Linux. Windows updates the user PATH; Unix updates shell
profiles for bash/zsh. Restart your terminal after installation or updates.
Installation from an extracted archive uses that archive's version; the online
command always fetches the latest published release.

For a custom directory or a pinned version:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.ps1))) -Version v0.1.0 -InstallDir "$env:LOCALAPPDATA\Programs\fluxget"
```

```sh
curl -fsSL https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.sh | bash -s -- --version v0.1.0 --prefix "$HOME/apps/fluxget"
```

Extracted-package options: `powershell -ExecutionPolicy Bypass -File install.ps1
-InstallDir C:\Tools\fluxget -NoModifyPath` or `bash install.sh --prefix
"$HOME/apps/fluxget" --no-modify-path`. The no-modify option leaves PATH/profile
settings unchanged. Running an installer from a source checkout uses its local
binary, or builds it if the prerequisites below are available.

## Build and install

Install [Rust 1.91 or newer](https://rustup.rs). Windows also needs the Visual
Studio C++ Build Tools; macOS needs Xcode command line tools. Linux needs a C
compiler and standard build tools. TLS uses rustls, so OpenSSL is not required.

```sh
cargo install --path . --locked
fluxget --help
fluxget doctor
```

Or build a portable executable with `cargo build --release --locked`. The binary
is `target/release/fluxget` (`fluxget.exe` on Windows). Run it directly or put it
on PATH. Python 3.10+ is needed only for the test/benchmark scripts.

## Download

```sh
fluxget https://example.org/archive.zip
fluxget https://example.org/archive.zip -o downloads/archive.zip
fluxget https://example.org/archive.zip --sha256 <64-hex-digit-hash>
fluxget inspect https://example.org/archive.zip
fluxget resolve https://example.org/downloads
fluxget resume downloads/archive.zip
```

Quote URLs containing `&` or shell special characters. `get URL` and bare `URL`
are equivalent. A page with multiple downloads opens an **arrow-key picker**:
use **Up/Down**, **Enter** to download, or **Esc** to cancel. A likely OS/CPU match
is highlighted; you can choose another platform. One candidate is selected
automatically unless you add `--pick`. `--direct` saves the actual HTTP response,
including HTML when desired.

```sh
fluxget https://code.visualstudio.com/
fluxget https://example.org/downloads --pick
fluxget resolve https://code.visualstudio.com/
fluxget https://example.org/downloads --select 2 --json
```

The resolver detects file links, download attributes, query-string filenames,
download URL attributes, `<video>`/`<audio>` sources, social-preview video tags,
base64-obfuscated download buttons and meta-refresh redirects. It follows up to
three same-origin download indexes one level below the starting page (fetched
concurrently), with a 2 MiB/page and 200-candidate limit. Candidates keep page
order. Page scripts are never run. A pasted `github.com/o/r` without a scheme
works (HTTPS, or HTTP for localhost).

## Supported sources

The sites people most often download from have dedicated support. Listings use
the sites' documented public APIs, so no scraping or login is involved, and the
published SHA-256 is verified automatically whenever the site provides one.

| Source | What you can paste | Result |
|---|---|---|
| **GitHub** (and GitHub Enterprise via `FLUXGET_GITHUB_URL`) | repo, `/releases`, `/releases/latest`, `/releases/tag/T`, `/blob/...`, `/tree/REF` | release assets with **verified digests**, source archives, raw file |
| **GitLab**, **Codeberg/Gitea/Forgejo** | `/-/releases`, `/releases`, `/releases/tag/T`, repo root on gitlab.com/codeberg.org | release assets and source archives |
| **Hugging Face** | model/dataset page, `/tree/REV/dir`, `/blob/...` | every file; LFS files carry **verified SHA-256** |
| **SourceForge** | project page | best release per platform, **verified SHA-256** |
| **archive.org** | `/details/ID` | original files first, then derivatives |
| **Google Drive / Docs**, **Dropbox**, **OneDrive**, **Pixeldrain** | public file share links | direct download; Docs/Sheets/Slides offer DOCX/XLSX/PPTX/PDF |
| **Torrent sites and files** | magnet links, `.torrent` URLs or files, pages with either | downloaded with aria2 (below) |
| **Video and audio pages** | YouTube, Vimeo, X/Twitter, Instagram, TikTok, Reddit, Twitch, SoundCloud, Bandcamp, and similar | downloaded with yt-dlp (below) |
| **VS Code** | `code.visualstudio.com` | all 25 official builds, listed instantly |

Not possible natively: MEGA (end-to-end encrypted; use MEGAcmd), Google Drive
folders, private repositories or files behind a login (`-H 'Authorization: ...'`
helps for token-based sites). OneDrive support covers public single-file shares
and relies on Microsoft's shares API.

GitHub's unauthenticated API allows 60 requests an hour per address; set
`GITHUB_TOKEN` (or `GH_TOKEN`) for 5000. When the limit is hit fluxget falls
back to the release page itself (still no login, but without digests). Other
optional tokens: `GITLAB_TOKEN`, `GITEA_TOKEN`, `HF_TOKEN` (gated models).

```sh
fluxget github.com/BurntSushi/ripgrep              # pick a release asset
fluxget https://github.com/o/r/releases/latest --select 3 --json
fluxget https://huggingface.co/org/model            # pick a model file, hash-verified
fluxget 'magnet:?xt=urn:btih:...' -o ~/torrents     # torrent via aria2
fluxget https://tracker.example/torrent/123         # page with magnet/.torrent links
fluxget https://www.youtube.com/watch?v=ID -o ~/videos
```

### Torrents and media

BitTorrent and video sites are delegated to [aria2](https://aria2.github.io/) and
[yt-dlp](https://github.com/yt-dlp/yt-dlp), which must be installed (`fluxget
doctor` shows what is found). Fluxget detects magnet links and `.torrent` files
(by link, file name or `application/x-bittorrent` content type), fetches `.torrent`
files itself with your proxy and headers, rejects HTML posing as a torrent, and
hands over a clean argument vector. For these two kinds `-o` names a *directory*,
torrents stop seeding when complete (`--seed-minutes N` to keep seeding), YouTube
URLs download one item, not the playlist, and with `--json` the tool's output
goes to stderr so stdout is a single JSON result. Piece hashing, peers, resume
and merging streams belong to those tools; `--direct` saves a `.torrent` file itself.
Custom `-H` headers are not forwarded to them, and BitTorrent peer traffic does
not use `--proxy`.

For scripts, `resolve` returns JSON with stable 1-based ordering. Use `--select N`
to choose a candidate. Non-interactive input and `--json` never open a picker;
ambiguous pages return the numbered choices and an actionable error. A download
landing page can be resolved further after selection, up to four page levels.

```sh
fluxget https://example.org/file.iso -j 16
fluxget https://example.org/file.iso --fixed -j 8
fluxget https://example.org/file.iso --single-stream
fluxget https://example.org/private.zip -H 'Authorization: Bearer TOKEN'
fluxget https://example.org/file.iso --proxy socks5h://127.0.0.1:1080
fluxget https://example.org/file.iso --json
```

Default settings: maximum sixteen requests; adaptive mode starts at four,
measures bytes as they arrive every 500 ms, multiplies workers by 1.5 while
throughput improves by more than 10%, rolls back after two flat samples and
re-probes ten samples later, and halves its target after observed throttling.
Already-running requests finish when the target falls. `--fixed` disables
throughput exploration; rate-limit responses can still reduce concurrency.
This controller is a heuristic, not a proof of the optimal connection count.
Small files can finish before it learns much. HTTP/2 multiplexes requests, so
worker count is not necessarily TCP connection count. Replacement requests start
before the previous chunk is written, and chunk data is synced and checkpointed
about once a second instead of after every chunk, so a slow disk does not idle
the connections; the checkpoint still only lists synced chunks.

There is **no download-speed cap** in fluxget. Sixteen is the default worker
ceiling, configurable with `-j` up to 32, for example `fluxget URL -j 32`.
Files advertise byte-range support, not an optimal number of workers. The
number of chunks bounds useful tasks; bandwidth, server limits, latency, CPU
and disk determine whether extra workers improve throughput. A worker-per-chunk
policy can open thousands of requests without adding bandwidth. The controller
therefore measures performance within the chosen ceiling. Native aggregate
bandwidth limiting is not implemented.

`--chunk-kib` defaults to 2048 (allowed 64–16384). Application body buffers are
bounded by maximum workers × chunk size, plus library buffers. Network retries
default to four after the first attempt; `--timeout` defaults to 60 seconds per
request. Change it for slow large single-stream downloads. `429`, `502`, `503`,
`504` and connection failures retry with exponential backoff; `Retry-After`
seconds and HTTP dates are honored. Truncated range bodies replay their chunk.
Other HTTP errors fail immediately. Body retries and header retries have separate
budgets, so one chunk can make up to `(retries + 1)^2` attempts in the worst case.

`--json` emits one final JSON result to stdout; errors go to stderr and return
exit code 1 (CLI usage errors return 2). Output includes strategy, SHA-256,
elapsed time, verified resumed bytes, and peak worker target. `transferred_bytes`
counts successfully committed bytes this invocation, excluding retried bytes
and probe traffic; its rate is not a network traffic meter.

## Resume and integrity

Real byte-range probes establish support; `Accept-Ranges` alone is not trusted.
Safe segmentation requires a known length and a **strong ETag**. Servers without
these receive a single stream. Each chunk uses `If-Match` and exact `Content-Range`
and length checks. Unexpected compression, changed validators, truncated bodies
and invalid ranges cannot be published as a successful result.

Interrupted segmented transfers leave files beside the intended output:

* `name.fluxget.part`: partial data, possibly pre-sized to the final length.
* `name.fluxget.json`: versioned identity and SHA-256 of completed chunks.
* `name.fluxget.lock`: a persistent lock file; ownership is an OS lock, not its existence.

Data is flushed before an atomic checkpoint replacement. Resume checks remote
URL, size, strong ETag and requested checksum, then re-hashes every saved chunk;
damaged chunks are downloaded again. Power-loss durability depends on the OS,
filesystem and drive honoring flushes; the final output directory publication
has normal filesystem crash semantics. No hardware power-failure test is claimed.

Single-stream transfers restart from zero after interruption. For those, repeat
the original URL command. `resume <output>` requires a segmented checkpoint.
On changed remote identity, move or delete **both** partial and checkpoint files
and start again. A requested hash mismatch retains the partial/checkpoint and
does not publish the output. Repeating the same expected hash cannot repair a
server consistently serving incorrect content; obtain the correct URL/hash.

The destination appears only after final SHA-256 calculation and optional
expected-hash verification. Existing outputs are protected unless `--force`
is set; replacement happens only when the new data verifies. Without an expected
hash, the printed SHA-256 is a fingerprint, not publisher authentication. A
strong ETag relies on the server honoring HTTP semantics.

Headers are never stored in resume state; provide `-H` again to resume private
downloads. Checkpoints store the final URL, including any signed query parameters:
keep them private. Expired signed URLs require a fresh download identity. Custom
headers cannot cross origins via redirects; HTTPS downgrades are blocked. The
native resolver does not log in, execute JavaScript, bypass access controls, or
silently select between multiple files.

## Reuse existing tools

Install [aria2](https://aria2.github.io/) and/or
[yt-dlp](https://github.com/yt-dlp/yt-dlp) independently and add them to PATH.
`fluxget doctor` reports their availability. All trailing arguments pass directly
to the tool as process arguments; no shell command is constructed.

```sh
fluxget aria2 -x8 -s8 -k1M https://example.org/file.iso
fluxget aria2 https://example.org/image.meta4
fluxget aria2 'magnet:?xt=urn:btih:...'
fluxget media --no-playlist 'https://supported-media-site.example/video'
```

These are explicit delegation commands. Fluxget does not claim its own adaptive
scheduler or checkpoint format applies to external tools. aria2 owns its own
resume, mirror, checksum and torrent semantics. yt-dlp may need ffmpeg or other
dependencies for selected sites. Backend exit failures propagate as fluxget
errors. See [research notes](docs/RESEARCH.md) for alternatives and decisions.

## Test and benchmark locally

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
python scripts/integration.py --binary target/release/fluxget
python scripts/test_sources.py --binary target/release/fluxget
python scripts/test_picker.py --binary target/release/fluxget
python scripts/benchmark.py --binary target/release/fluxget --runs 3
python scripts/package.py
python scripts/test_installers.py
```

`test_sources.py` needs `rustc` (it builds a recording stand-in for aria2c and
yt-dlp) and runs GitHub-, GitLab- and Gitea-style APIs, torrent pages and landing
pages on loopback; no external tools or internet are required. Append `.exe` to
the binary path on Windows. `--aria2 /path/to/aria2c` adds the
aria2 comparison if it is not on PATH. The benchmark automatically includes curl
when available. It writes JSON with exact commands and raw repetitions, plus a
Markdown table, into `bench-results/`. It uses a loopback fixture, deterministic
content, randomized trial order, an untimed warm-up and SHA-256 verification of
every result. Cases cover unrestricted transfers, synthetic per-connection caps,
and servers ignoring ranges. Temporary payloads are removed automatically.

Only the test server applies the synthetic per-connection cap. The downloader
itself does not impose that cap; the unrestricted scenario has no rate limit.

The measured outcome depends on file size, request latency, bandwidth policy,
disk, checksum work, and checkpoint flush overhead. Loopback is not an internet
speed test. Curl and aria2 use different durability and hash policies; the
end-to-end comparison exposes those practical costs. No universal speedup is
claimed. See [the recorded Windows results](docs/BENCHMARK_WINDOWS.md).

## Scope and next work

Working now: native HTTP(S), redirects, public static HTML candidates, bounded
dynamic chunk scheduling, adaptive/fixed workers, validated resume, integrity,
progress/JSON output, proxies/headers, arrow-key download selection, release and
file-listing adapters (GitHub, GitLab, Gitea, Hugging Face, SourceForge,
archive.org), cloud share links, automatic torrent/magnet and media-page
delegation, and a VS Code page adapter.

Future work: independent mirror identity proofs and native mirror striping,
browser extension/native messaging, batch scheduling, aggregate rate limiting,
cookie jars, login-gated sites, JavaScript-rendered listings (torrent search
results, single-page download sites), a native BitTorrent engine, a richer terminal interface, and broader
real-WAN benchmarks. Browser integration is deliberately a later milestone.

MIT licensed; see [LICENSE](LICENSE). Upstream libraries retain their licenses.

