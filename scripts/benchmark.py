"""Reproducible local end-to-end benchmark; checks every output's SHA-256.

Includes process startup, probing, writing and fluxget's internal verification.
An additional harness hash validates all tools outside the timed region. Results
describe this HTTP/1.1 loopback fixture, never general internet performance.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import tempfile
import time

from fixture import payload, start


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--aria2", default=shutil.which("aria2c"))
    parser.add_argument("--curl", default=shutil.which("curl.exe") or shutil.which("curl"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--output", type=Path, default=Path("bench-results/local.json"))
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    binary = str(args.binary.resolve())
    tools = ["fluxget-auto", "fluxget-fixed8", "fluxget-single"]
    if args.curl:
        tools.append("curl")
    if args.aria2:
        tools.append("aria2-8")
    scenarios = [
        {"name": "loopback-unthrottled", "path": "/file.bin", "mib": 64, "cap_mib_per_connection": 0},
        {"name": "per-connection-cap", "path": "/file.bin", "mib": 32, "cap_mib_per_connection": 4},
        {"name": "no-range-support", "path": "/norange", "mib": 8, "cap_mib_per_connection": 4},
    ]

    def command(tool, url, output):
        if tool.startswith("fluxget"):
            flags = {"fluxget-auto": [], "fluxget-fixed8": ["--fixed", "-j", "8"], "fluxget-single": ["--single-stream"]}[tool]
            return [binary, url, "--direct", "-o", str(output), "--json", *flags]
        if tool == "curl":
            return [args.curl, "--fail", "--silent", "--show-error", "--location", "--noproxy", "*", "-o", str(output), url]
        return [args.aria2, "--no-conf=true", "--console-log-level=error", "--summary-interval=0", "--file-allocation=none", "--enable-color=false", "--all-proxy=", "-x8", "-s8", "-k1M", "--allow-overwrite=false", "--auto-file-renaming=false", "-d", str(output.parent), "-o", output.name, url]

    server = start()
    records = []
    rng = random.Random(20261027)
    try:
        with tempfile.TemporaryDirectory(prefix="fluxget-bench-") as folder:
            folder = Path(folder)
            # Untimed warm-up: process loading, TLS provider and filesystem caches.
            for i, tool in enumerate(tools):
                subprocess.run(command(tool, server.url + "/file.bin?size=1048576", folder / f"warmup-{i}.bin"), capture_output=True, check=True, timeout=60)
            for scenario in scenarios:
                size = scenario["mib"] * 1024 * 1024
                expected = hashlib.sha256(payload(size)).hexdigest()
                url = f'{server.url}{scenario["path"]}?size={size}&cap={scenario["cap_mib_per_connection"]}'
                for repeat in range(args.runs):
                    order = tools.copy()
                    rng.shuffle(order)
                    for tool in order:
                        output = folder / f'{scenario["name"]}-{repeat}-{tool}.bin'
                        cmd = command(tool, url, output)
                        began = time.perf_counter()
                        result = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
                        elapsed = time.perf_counter() - began
                        if result.returncode:
                            raise RuntimeError(f"{tool} failed: {result.stderr} {result.stdout}")
                        with output.open("rb") as downloaded:
                            digest = hashlib.file_digest(downloaded, "sha256").hexdigest()
                        if digest != expected:
                            raise RuntimeError(f"{tool}: hash mismatch")
                        record = {"scenario": scenario["name"], "tool": tool, "run": repeat + 1, "bytes": size, "seconds": elapsed, "mib_per_second": scenario["mib"] / elapsed, "sha256": digest, "verified": True, "command": cmd}
                        if tool.startswith("fluxget"):
                            record["engine"] = json.loads(result.stdout)
                        records.append(record)
                        print(f'{scenario["name"]:24} {tool:16} run {repeat+1}: {record["mib_per_second"]:8.2f} MiB/s ({elapsed:.3f}s), SHA-256 OK', flush=True)
                        output.unlink()
    finally:
        server.shutdown()
        server.server_close()
    groups = []
    for scenario in scenarios:
        for tool in tools:
            rows = [r for r in records if r["scenario"] == scenario["name"] and r["tool"] == tool]
            groups.append({"scenario": scenario["name"], "tool": tool, "median_mib_per_second": statistics.median(r["mib_per_second"] for r in rows), "min_mib_per_second": min(r["mib_per_second"] for r in rows), "max_mib_per_second": max(r["mib_per_second"] for r in rows), "runs": len(rows)})
    versions = {}
    for name, executable in [("fluxget", binary), ("curl", args.curl), ("aria2", args.aria2)]:
        if executable:
            versions[name] = subprocess.run([executable, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    data = {"timestamp_utc": datetime.now(timezone.utc).isoformat(), "platform": platform.platform(), "processor": platform.processor(), "logical_cpus": os.cpu_count(), "python": platform.python_version(), "versions": versions, "seed": 20261027, "runs": args.runs, "scenarios": scenarios, "methodology": "HTTP/1.1 loopback; deterministic payload; randomized order each repetition; one untimed warm-up per tool; end-to-end wall time includes startup, probes, disk writes, and internal fluxget hash; external hash for all tools outside timing; per-connection cap is synthetic, not aggregate bandwidth; no TLS or internet variability. Files are memory-cache friendly. Fluxget flushes checkpoints per chunk; curl/aria2 durability policies differ.", "summary": groups, "records": records}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    markdown = ["# Local benchmark", "", f'Host: {data["platform"]}; {data["logical_cpus"]} logical CPUs. Python {data["python"]}.', "", data["methodology"], "", "| Scenario | Tool | Median MiB/s | Min–max MiB/s | Runs |", "|---|---|---:|---:|---:|"]
    for row in groups:
        markdown.append(f'| {row["scenario"]} | {row["tool"]} | {row["median_mib_per_second"]:.2f} | {row["min_mib_per_second"]:.2f}–{row["max_mib_per_second"]:.2f} | {row["runs"]} |')
    markdown.extend(["", "Every output passed SHA-256 verification. These measurements do not establish superiority on real internet connections. Mac/Linux execution must be verified by CI or on those machines.", "", "Exact commands, versions, timing, hashes, and adaptive worker counts are in the companion JSON.", ""])
    args.output.with_suffix(".md").write_text("\n".join(markdown), encoding="utf-8")
    print(f"Saved {args.output} and {args.output.with_suffix('.md')}", flush=True)


if __name__ == "__main__":
    main()
