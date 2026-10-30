# Development

[Back to README](../README.md) · [Usage](usage.md) · [Installation](installation.md) · [Integrity](integrity.md) · [Development](development.md)

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
claimed. See [the recorded Windows results](BENCHMARK_WINDOWS.md).

