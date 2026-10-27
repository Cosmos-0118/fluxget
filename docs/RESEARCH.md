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
| [dialoguer](https://docs.rs/dialoguer/0.12.0/dialoguer/struct.Select.html), MIT | Terminal lists, arrow-key handling, paging and cancellation | Reuse for the page download picker rather than building a terminal input system. |
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


## What people download in a browser, and how each is supported

Recorded 2026-10-27 from the public sites themselves (live API responses checked
the same day), not from usage statistics. Browser downloads fall into a few
groups; each has a different obstacle for a command-line downloader.

| Group | Typical sites | Obstacle | Decision |
|---|---|---|---|
| Software releases | GitHub, GitLab, Codeberg, SourceForge, vendor pages | Assets are loaded by script; pages list source archives and checksums beside installers | Use each site's public release API (exact names, sizes, SHA-256 digests); fall back to GitHub's asset fragment when the API rate limit is hit |
| Models and datasets | Hugging Face | Repositories hold many large files; LFS objects are addressed by SHA-256 | Tree API listing; verify LFS SHA-256 automatically |
| Archives and media libraries | archive.org | Items hold originals plus derived files and internal metadata | Metadata API; originals first, internal files hidden |
| Cloud shares | Google Drive/Docs, Dropbox, OneDrive, Pixeldrain | Share pages are not the file | Rewrite to the documented direct form; MEGA is encrypted end to end and is refused with an explanation |
| Torrents | Index pages with magnet links and `.torrent` files, trackers | Different protocol (peers, DHT, piece hashes) | Detect magnet/torrent links in pages; delegate to aria2 instead of reimplementing BitTorrent |
| Video and audio | YouTube, Vimeo, X, Instagram, TikTok, Reddit, Twitch, SoundCloud | Per-site extractors that change weekly | Recognise media page URLs; delegate to yt-dlp |
| Generic files | Direct links, mirrors, "your download will start" pages | Redirects, meta refresh, obfuscated buttons | Static HTML heuristics only |

Not attempted: login-gated sites and JavaScript-rendered listings (torrent search
results, single-page download sites). A native BitTorrent engine was not added:
aria2 is mature, and verifying a from-scratch peer protocol would need swarm
testing that a loopback fixture cannot provide. Digest verification relies on
the publisher: a compromised release API could supply a matching bad digest.

## Measured download speed (2026-10-27, Windows 11)

The VS Code user installer (249 MB) completed in about 11.5 s with curl, the old
fluxget and the new fluxget alike on a steady connection, limited by the link
(about 21 MB/s). Earlier in the session the same CDN briefly throttled single
connections to about 2 MB/s; in that state a 16-worker transfer finished in 14.6 s
while the old adaptive controller needed 38 s. Two engine causes were fixed:
the controller started at two workers and judged throughput from whole completed
chunks, and the coordinator synced and rewrote the checkpoint after every chunk
before starting the next request. On the loopback fixture the new engine
completes an unthrottled 256 MiB transfer in 0.44 s versus 0.90 s. Landing-page
resolution for code.visualstudio.com fell from 0.6-5.9 s to 8 ms, because the
adapter now knows all 25 builds and needs no page fetch. Resolution latency, not
transfer speed, is what the old binary spent extra seconds on.
