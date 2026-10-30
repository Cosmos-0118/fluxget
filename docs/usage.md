# Usage guide

[Back to README](../README.md) · [Usage](usage.md) · [Installation](installation.md) · [Integrity](integrity.md) · [Development](development.md)

## Download

```sh
fluxget https://example.org/archive.zip
fluxget https://example.org/archive.zip -o downloads/archive.zip
fluxget https://example.org/archive.zip --sha256 <64-hex-digit-hash>
fluxget inspect https://example.org/archive.zip
fluxget resolve https://example.org/downloads
fluxget resume downloads/archive.zip
```

The installer also adds **`fx`**, a two-letter copy of the command, so
`fx https://example.org/archive.zip` works the same. An unrelated program already
named `fx` in the install folder is never replaced.

**Where files go.** Without `-o`, downloads are saved in your **Downloads
folder**. In a terminal, fluxget asks first: *Downloads* is preselected, or choose
*Pick a folder...* to open the system folder picker (Windows file dialog, macOS
Finder, zenity/kdialog on Linux, or a typed path when no dialog is available).
Piped, `--json` and `--quiet` runs skip the question and use Downloads; `-o`
always wins and also skips it. A name already taken becomes `name (1).ext`, like
a browser (use `--force` to replace). Set `FLUXGET_DOWNLOAD_DIR` to change the
default folder.

**Live display.** In a terminal a download shows the file, an overall bar with
speed, ETA and a throughput sparkline, one lane per connection (which chunk each
is fetching and how far along), and a map of every chunk (done, downloading,
waiting). It adapts to the window size and disappears when the download ends;
`--quiet`, `--json` and piped output stay plain.

**Resuming.** `fx resume` with no name lists every paused download (found in the
Downloads folder, the current folder, and the ones fluxget remembers) so you can
pick one with the arrow keys. `fx resume <output>` still resumes a specific file.

`fluxget winget install <package>` runs the Windows Package Manager with the same
arguments, so one tool covers both file downloads and app installs.

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
bounded by maximum workers × chunk size, plus library buffers.

### Network problems and recovery

* **Retries.** Each request is retried four times after the first attempt (`--retries`),
  with exponential backoff from 250 ms to 30 s plus random jitter so connections that failed
  together do not retry together. `408`, `429`, `500`, `502`, `503`, `504` and `520`–`524`
  are retried, and `Retry-After` seconds and HTTP dates are honored. Other HTTP errors
  (`403`, `404`, `412`, a wrong `Content-Range`, a changed file) fail immediately.
* **Stalls.** `--timeout` (default 30) is the time without *any* data after which a
  request counts as stalled and is retried. It is not a limit on total transfer time, so a
  long download that keeps receiving data is never cut off. TCP and HTTP/2 keep-alives also
  detect a dead connection, such as a lost Wi-Fi link, sooner than that.
* **Broken connections.** When a connection drops partway through a chunk and the server
  gave a strong ETag or `Last-Modified`, the next request asks only for the rest of the
  chunk. Without those guards the chunk is fetched again whole, so two versions of a
  file can never be joined. An attempt that delivered 64 KiB or more does not use up the
  retry budget.
* **Network outages.** When no worker has received a byte for a couple of seconds and
  requests keep failing with connection errors or temporary server errors, fluxget stops
  hammering the server. One request probes now and then, with growing pauses of at most 15
  seconds, and the display shows *waiting for the network*. When a probe succeeds the
  download continues at full speed where it left off. `--patience SECONDS` (default 300)
  is how long to wait without any data before giving up; the progress is saved and
  `fluxget resume` continues it. `--patience 0` restores the older behaviour of failing as
  soon as the retries run out. In scripts, one line on stderr says the network is gone and
  one says it is back; `--json` output on stdout stays a single object.
* **Interruption.** Ctrl-C, `SIGTERM` and `SIGHUP` (and on Windows Ctrl-Break, closing the
  console, logoff and shutdown) save the checkpoint and stop. If the process is still busy
  three seconds later, or a second request arrives, it exits with code 130; saved progress
  is never in doubt because the checkpoint is replaced atomically.
* **Single-stream transfers** cannot continue where they stopped, so a broken response
  starts again from zero within the normal retry budget. Only an unreachable network earns
  extra waiting up to `--patience`.

`--json` emits one final JSON result to stdout; errors go to stderr and return
exit code 1 (CLI usage errors return 2). Output includes strategy, SHA-256,
elapsed time, verified resumed bytes, and peak worker target. `transferred_bytes`
counts successfully committed bytes this invocation, excluding retried bytes
and probe traffic; its rate is not a network traffic meter.

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
errors. See [research notes](RESEARCH.md) for alternatives and decisions.


See also [Resume and integrity](integrity.md) for the checkpoint format and verification rules.
