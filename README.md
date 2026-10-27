# fluxget

An adaptive, integrity-first download accelerator for Windows, macOS, and Linux.
Native Rust HTTP(S) engine; optional **aria2** and **yt-dlp** commands reuse mature
open-source implementations for torrents, Metalink, FTP/SFTP and media websites.

This is version **0.1.0**. Release packages are built and tested on Windows,
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
are equivalent. Only a single unambiguous HTML download candidate is followed;
use `resolve` to list alternatives and choose one explicitly. `--direct` saves
the actual HTTP response, including HTML when desired.

```sh
fluxget https://example.org/file.iso -j 16
fluxget https://example.org/file.iso --fixed -j 8
fluxget https://example.org/file.iso --single-stream
fluxget https://example.org/private.zip -H 'Authorization: Bearer TOKEN'
fluxget https://example.org/file.iso --proxy socks5h://127.0.0.1:1080
fluxget https://example.org/file.iso --json
```

Default settings: maximum eight requests; adaptive mode starts at two, samples
completed-chunk throughput every 750 ms, explores by adding one worker, rolls
back when the gain is below 5%, and halves its target after observed throttling.
Already-running requests finish when the target falls. `--fixed` disables
throughput exploration; rate-limit responses can still reduce concurrency.
This initial controller is a heuristic, not a proof of the optimal connection
count. Small files can finish before it learns much. HTTP/2 multiplexes requests,
so worker count is not necessarily TCP connection count.

There is **no download-speed cap** in fluxget. Eight is the default worker
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
python scripts/benchmark.py --binary target/release/fluxget --runs 3
python scripts/package.py
python scripts/test_installers.py
```

Append `.exe` to the binary path on Windows. `--aria2 /path/to/aria2c` adds the
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
progress/JSON output, proxies/headers and optional backend delegation.

Future work: independent mirror identity proofs and native mirror striping,
browser extension/native messaging, batch scheduling, aggregate rate limiting,
cookie jars, more resolver adapters, a richer terminal interface, and broader
real-WAN benchmarks. Browser integration is deliberately a later milestone.

MIT licensed; see [LICENSE](LICENSE). Upstream libraries retain their licenses.

