# Resume and integrity

[Back to README](../README.md) · [Usage](usage.md) · [Installation](installation.md) · [Integrity](integrity.md) · [Development](development.md)

Real byte-range probes establish support; `Accept-Ranges` alone is not trusted.
Segmentation requires a known length and a working range probe. Every chunk
carries the strongest precondition the server offers: `If-Match` for a strong
ETag, otherwise `If-Unmodified-Since` from `Last-Modified`. Responses are also
compared with the probe, including weak ETags, which are never sent. A server
that offers none of these (many CDNs) is still downloaded in parallel, but the
start, middle and end of the file are fetched again and compared with the saved
bytes before the output is published; resuming does the same for saved chunks.
Exact `Content-Range` and length checks apply throughout. Unexpected
compression, changed validators, truncated bodies and invalid ranges cannot be
published as a successful result. The summary's `validator` field says which
guard applied (`etag`, `last-modified`, `weak-etag` or `none`).

Interrupted segmented transfers leave files beside the intended output:

* `name.fluxget.part`: partial data, possibly pre-sized to the final length.
* `name.fluxget.json`: versioned identity and SHA-256 of completed chunks.
* `name.fluxget.lock`: a persistent lock file; ownership is an OS lock, not its existence.
* `name.fluxget.json.tmp`: exists only for an instant while the checkpoint is replaced. A crash
  at that moment can leave it behind; the next save overwrites it.

Data is flushed before an atomic checkpoint replacement. Resume checks remote
URL, size, validators and requested checksum, then re-hashes every saved chunk;
damaged chunks are downloaded again. Power-loss durability depends on the OS,
filesystem and drive honoring flushes; the final output directory publication
has normal filesystem crash semantics. No hardware power-failure test is claimed.

The checkpoint is written again whenever finished chunks reach the disk (about once a second),
and also whenever the transfer stops for any reason: an interruption, a network failure, a full
disk or an unexpected error. A process that is killed outright (`kill -9`, power loss) loses at
most the last second of finished chunks and the chunks that were still in flight, which are
downloaded again. A `.part` file with no checkpoint, left by a run that stopped before its first
save, holds no verified data and is started over automatically. A checkpoint left behind after
the download was published is removed the next time that output is requested.

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
validator relies on the server honoring HTTP semantics, and the spot check used
without one detects a replaced file but is no substitute for `--sha256`.

Headers are never stored in resume state; provide `-H` again to resume private
downloads. Checkpoints store the final URL, including any signed query parameters:
keep them private. Expired signed URLs require a fresh download identity. Custom
headers cannot cross origins via redirects; HTTPS downgrades are blocked. The
native resolver does not log in, execute JavaScript, bypass access controls, or
silently select between multiple files.

