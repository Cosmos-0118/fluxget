# Open-source research and design decisions

Research performed 2026-10-27 against upstream project documentation. This is a
focused comparison of relevant projects, not an exhaustive survey of every
open-source downloader. No upstream application source was copied into fluxget.

| Project | Relevant capability | Decision |
|---|---|---|
| [aria2](https://github.com/aria2/aria2), GPL-2.0 | Mature cross-platform segmented HTTP, torrents, Metalink, FTP/SFTP, mirrors | Reuse as an optional user-installed process through `fluxget aria2`; benchmark against it. Do not reimplement those protocol stacks. |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Site-specific media extractors and media workflows | Delegate explicitly through `fluxget media`; no custom video-site extractor library. Check upstream distribution/dependency licenses when packaging it. |
| [reqwest](https://docs.rs/reqwest/0.12.22/reqwest/), MIT/Apache-2.0 | Async HTTP, pooling, redirects, proxies, rustls, HTTP/2 | Reuse directly. Pin the tested 0.12.22 API and commit Cargo.lock. No native TLS/OpenSSL deployment dependency. |
| [Tokio](https://tokio.rs/), MIT | Cross-platform async runtime, tasks, cancellation | Reuse for concurrent requests and completion-driven chunk scheduling. |
| [clap](https://github.com/clap-rs/clap), MIT/Apache-2.0 | CLI parsing, help and argument validation | Reuse derive interface. |
| [scraper](https://github.com/rust-scraper/scraper), ISC | HTML parsing and CSS selectors | Reuse instead of inventing an HTML parser or executing page scripts. |
| [indicatif](https://github.com/console-rs/indicatif), MIT | Cross-platform terminal progress | Reuse; no need for a full TUI in the initial CLI. |
| [gosh-dl](https://github.com/goshitsarch-eng/gosh-dl), MIT | Embeddable Rust download engine, queues, SQLite and torrents | Evaluated as a larger all-in-one alternative. Its documented API brings substantial functionality beyond this focused native controller. Keep it a candidate for future queue/torrent integration; not included as a dependency. |
| [Surge](https://github.com/SurgeDM/Surge), MIT | Go TUI downloader and existing browser integration | Evaluated as a complete application alternative; no embedding of its Go runtime into the Rust CLI. Consider using the existing app if a mature TUI is the primary goal. |
| [libcurl multi](https://curl.se/libcurl/c/libcurl-multi.html) | Simultaneous transfers, event-driven integration, extensive protocols | Valid future backend; use installed curl as a baseline now. Adding a second native network stack before measuring the first would complicate deployment. |

## Why there is a small native engine

Transport, TLS, HTML parsing, CLI handling, progress, hashes, atomic temporary
files and locks are existing libraries. Fluxget-specific code is the chunk
controller, strict range validation, conservative identity policy, checkpoint
ordering, and CLI orchestration. The native path makes those policies directly
testable; optional backends cover broad protocol/extractor features without
duplicating them. We have not established that a new native engine outperforms
aria2, curl, or an embedded existing engine in general.

## HTTP rules

[RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html), especially sections 8.8,
13.1.1, 13.1.5 and 14, is the protocol reference. A real one-byte GET verifies
range support. Weak ETags cannot identify byte-for-byte content. Requests use
`If-Match` plus `Range` so a changed representation fails with 412 rather than
being silently merged. Every returned span, total size and body length must
match exactly. `Accept-Encoding: identity` keeps offsets tied to stored bytes.

More workers are not automatically faster. The controller starts conservatively
and explores only within a user cap; synthetic throttled tests demonstrate the
benefit of parallelism, while unrestricted loopback shows synchronization,
checkpoint and hashing overhead. Header-level `Retry-After` and an adaptive
decrease reduce pressure after observed throttling, but they are not a global
origin-wide token bucket. Native aggregate rate limiting is future work.

## Persistence tradeoff

The concept suggested SQLite. A versioned JSON sidecar is sufficient for one
CLI download: atomic replacement, data-first flushes, hashes of complete chunks,
and a per-output OS lock are simpler to inspect and recover. SQLite becomes
more useful for a persistent multi-download queue, history and a browser daemon.

## Licensing and packaging

The repository's own code is MIT. Cargo.lock captures exact transitive versions;
the complete dependency graph can be inspected with `cargo tree --locked`.
Optional backend binaries are neither vendored nor redistributed in releases.
The portable aria2 downloaded for local measurement stays in this task's work
folder, with its upstream license; its ZIP SHA-256 is
`67d015301eef0b612191212d564c5bb0a14b5b9c4796b76454276a4d28d9b288`.

