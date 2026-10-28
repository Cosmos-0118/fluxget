"""Connection-count sweep against a loopback origin with a finite shared link.

Answers "is more connections actually faster, and does auto find the knee?" on
a fixture where each connection is capped and the whole link is capped too.
Randomized order per repetition, median of runs, every output SHA-256 checked.
Loopback only: use it to compare configurations, not to predict internet speed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import random
import statistics
import subprocess
import tempfile
import time

from fixture import payload, start


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--connections", type=int, nargs="+", default=[1, 4, 8, 13, 24])
    parser.add_argument("--mib", type=int, default=96)
    parser.add_argument("--cap", type=float, default=6, help="MiB/s per connection")
    parser.add_argument("--agg", type=float, default=36, help="MiB/s for the whole link")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--output", type=Path, default=Path("bench-results/sweep.json"))
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    binary = str(args.binary.resolve())
    top = max(args.connections)
    configs = {f"fixed-{n}": ["--fixed", "-j", str(n)] for n in args.connections}
    configs[f"auto-max{top}"] = ["-j", str(top)]
    size = args.mib * 1024 * 1024
    expected = hashlib.sha256(payload(size)).hexdigest()
    server = start()
    url = f"{server.url}/file.bin?size={size}&cap={args.cap}&agg={args.agg}"
    records = []
    rng = random.Random(20261028)
    try:
        with tempfile.TemporaryDirectory(prefix="fluxget-sweep-") as folder:
            folder = Path(folder)
            for repeat in range(args.runs):
                order = list(configs)
                rng.shuffle(order)
                for name in order:
                    out = folder / f"{name}-{repeat}.bin"
                    cmd = [binary, url, "--direct", "-o", str(out), "--json", *configs[name]]
                    began = time.perf_counter()
                    result = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
                    elapsed = time.perf_counter() - began
                    if result.returncode:
                        raise RuntimeError(f"{name} failed: {result.stderr} {result.stdout}")
                    with out.open("rb") as handle:
                        if hashlib.file_digest(handle, "sha256").hexdigest() != expected:
                            raise RuntimeError(f"{name}: hash mismatch")
                    engine = json.loads(result.stdout)
                    records.append({"config": name, "run": repeat + 1, "seconds": elapsed, "mib_per_second": args.mib / elapsed, "peak_connections": engine["peak_connections"]})
                    print(f"{name:12} run {repeat + 1}: {args.mib / elapsed:7.2f} MiB/s ({elapsed:.2f}s) peak {engine['peak_connections']}", flush=True)
                    out.unlink()
    finally:
        server.shutdown()
        server.server_close()
    rows = []
    for name in configs:
        speeds = [r["mib_per_second"] for r in records if r["config"] == name]
        peak = max(r["peak_connections"] for r in records if r["config"] == name)
        rows.append({"config": name, "median": statistics.median(speeds), "min": min(speeds), "max": max(speeds), "peak_connections": peak})
    best = max(r["median"] for r in rows if r["config"].startswith("fixed"))
    lines = [
        "# Connection sweep",
        "",
        f"{args.mib} MiB, {args.cap:g} MiB/s per connection, {args.agg:g} MiB/s shared link, {args.runs} runs, randomized order, SHA-256 verified.",
        "",
        "| Config | Median MiB/s | Min–max | % of best fixed | Peak connections |",
        "|---|---:|---:|---:|---:|",
    ]
    for r in rows:
        lines.append(f'| {r["config"]} | {r["median"]:.2f} | {r["min"]:.2f}–{r["max"]:.2f} | {100 * r["median"] / best:.0f}% | {r["peak_connections"]} |')
    text = "\n".join(lines) + "\n"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"args": {k: str(v) for k, v in vars(args).items()}, "summary": rows, "records": records}, indent=2) + "\n", encoding="utf-8")
    args.output.with_suffix(".md").write_text(text, encoding="utf-8")
    print("\n" + text)


if __name__ == "__main__":
    main()
