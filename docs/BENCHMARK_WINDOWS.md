# Local benchmark

Host: Windows-11-10.0.26300-SP0; 12 logical CPUs. Python 3.12.10.

HTTP/1.1 loopback; deterministic payload; randomized order each repetition; one untimed warm-up per tool; end-to-end wall time includes startup, probes, disk writes, and internal fluxget hash; external hash for all tools outside timing; per-connection cap is synthetic, not aggregate bandwidth; no TLS or internet variability. Files are memory-cache friendly. Fluxget flushes checkpoints per chunk; curl/aria2 durability policies differ.

| Scenario | Tool | Median MiB/s | Min–max MiB/s | Runs |
|---|---|---:|---:|---:|
| loopback-unthrottled | fluxget-auto | 267.82 | 257.51–274.57 | 3 |
| loopback-unthrottled | fluxget-fixed8 | 331.99 | 330.48–343.66 | 3 |
| loopback-unthrottled | fluxget-single | 261.30 | 194.26–275.60 | 3 |
| loopback-unthrottled | curl | 1380.19 | 1337.19–1398.15 | 3 |
| loopback-unthrottled | aria2-8 | 531.86 | 514.25–550.34 | 3 |
| per-connection-cap | fluxget-auto | 10.53 | 10.41–10.53 | 3 |
| per-connection-cap | fluxget-fixed8 | 30.17 | 30.15–30.21 | 3 |
| per-connection-cap | fluxget-single | 3.98 | 3.96–3.98 | 3 |
| per-connection-cap | curl | 4.00 | 4.00–4.00 | 3 |
| per-connection-cap | aria2-8 | 30.92 | 30.77–30.94 | 3 |
| no-range-support | fluxget-auto | 3.97 | 3.97–3.97 | 3 |
| no-range-support | fluxget-fixed8 | 3.97 | 3.97–3.98 | 3 |
| no-range-support | fluxget-single | 3.97 | 3.97–3.98 | 3 |
| no-range-support | curl | 4.00 | 4.00–4.00 | 3 |
| no-range-support | aria2-8 | 3.97 | 3.97–3.97 | 3 |

Every output passed SHA-256 verification. These measurements do not establish superiority on real internet connections. Mac/Linux execution must be verified by CI or on those machines.

Exact commands, versions, timing, hashes, and adaptive worker counts are in the companion JSON.
