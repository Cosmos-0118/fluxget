# Windows validation — 2026-10-27

Local environment: Windows 11 x86_64, 12 logical CPUs, Rust/Cargo 1.91.1,
Python 3.12.10, Visual Studio 18 Community C++ toolchain.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed with no warnings |
| `cargo test --locked` | 5 passed |
| Optimized release build | Passed |
| Real HTTP integration suite | 21 passed; repeated successfully against the executable in the final Developer repository |
| Local benchmark | 45 timed trials, 3 repetitions × 5 configurations × 3 scenarios; all output hashes matched |
| Real public HTTPS smoke check | 1,187,554 bytes from RFC Editor, native TLS download matched independently downloaded curl output |
| Optional aria2 process adapter | `fluxget aria2 --version` successfully executed the official portable aria2 1.37.0 |
| macOS/Linux runtime checks | CI configured; not executed in this Windows session |

The HTTP integration suite exercises:

* Parallel range transfers and expected SHA-256 verification.
* Redirects, empty files, unknown lengths, and servers ignoring Range.
* Missing/weak validators falling back to single streams.
* Invalid range headers, truncated bodies and retry exhaustion.
* HTTP 429 backoff honoring `Retry-After`, with controller reduction.
* Hard process termination, resume and repair of deliberately corrupted chunks.
* Refusal to resume changed remote content; exclusive per-output OS locking.
* Refusal to publish a checksum mismatch or silently overwrite completed output.
* Explicit force replacement after verification.
* HTML candidate discovery and ambiguous-page refusal.
* Encoded-body refusal, CLI validation and custom-header redirect containment.

The real HTTPS smoke URL was
`https://www.rfc-editor.org/rfc/rfc9110.html`. Its observed SHA-256 was
`d431760660ea44e130f6e919dab216df2d0b3a490567a98089267523368fe1e5`.
This is a recorded observation, not a promise that an upstream page never changes.

See `BENCHMARK_WINDOWS.md` and `BENCHMARK_WINDOWS.json` for measured performance,
raw repetitions, exact commands, versions, hashes and methodology. The loopback
fixture's accept backlog is 128 to accommodate all configured workers without
artificial connection-backlog stalls.

Known limits: no hardware power-failure testing; no native verified multi-mirror
striping, browser extension, authenticated JavaScript extraction, aggregate rate
limiting or batch queue; no claim to be faster than curl/aria2 in every scenario.
