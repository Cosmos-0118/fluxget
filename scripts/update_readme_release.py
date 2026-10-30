"""Refresh the release block in README.md from the checksums published with a release.

The release workflow runs this after the packages are built: it reads the `*.sha256`
files that scripts/package.py writes next to every archive and rewrites the text
between the release markers with the new version, download links, SHA-256 values
and matching verify commands. Running it twice for the same release changes nothing."""
import argparse
import re
import sys
from pathlib import Path

START = "<!-- release-info:start -->"
END = "<!-- release-info:end -->"
PLATFORMS = {"windows": "Windows", "macos": "macOS", "linux": "Linux"}
ARCHES = {"x86_64": "x86_64 (Intel/AMD)", "aarch64": "aarch64 (ARM)"}


def read_checksums(directory):
    entries = []
    for path in sorted(directory.glob("*.sha256")):
        match = re.fullmatch(r"([0-9a-fA-F]{64}) [ *](.+)", path.read_text(encoding="ascii").strip())
        if match:
            entries.append((match[2], match[1].lower()))
    return entries


def sort_key(entry):
    match = re.fullmatch(r"fluxget-([a-z]+)-([a-z0-9_]+)\.(zip|tar\.gz)", entry[0])
    if not match:
        return (9, 9, entry[0])
    order = list(PLATFORMS).index(match[1]) if match[1] in PLATFORMS else 8
    return (order, list(ARCHES).index(match[2]) if match[2] in ARCHES else 9, match[3])


def label(name):
    match = re.fullmatch(r"fluxget-([a-z]+)-([a-z0-9_]+)\.(zip|tar\.gz)", name)
    if not match:
        return name
    return f"{PLATFORMS.get(match[1], match[1])} {ARCHES.get(match[2], match[2])}"


def build_block(version, repo, entries):
    tag = "v" + version
    base = f"https://github.com/{repo}/releases/download/{tag}"
    lines = [
        f"- **Latest release:** [fluxget {version}](https://github.com/{repo}/releases/tag/{tag})",
        "- **Downloads and SHA-256 checksums:**",
        "",
        "  | Platform | File | SHA-256 |",
        "  | --- | --- | --- |",
    ]
    for name, digest in entries:
        lines.append(f"  | {label(name)} | [`{name}`]({base}/{name}) | `{digest}` |")
    lines += [
        "",
        "  Every archive also has a `.sha256` file on the release page.",
        "",
        "- **Verify locally** (run it in the folder you downloaded to):",
        "",
    ]
    unix = next((e for e in entries if e[0].endswith(".tar.gz")), None)
    windows = next((e for e in entries if e[0].endswith(".zip") and "windows" in e[0]), None)
    if unix:
        lines += ["  ```sh", f"  echo '{unix[1]}  {unix[0]}' | shasum -a 256 -c", "  ```", ""]
    if windows:
        lines += [
            "  On Windows, PowerShell prints `True` when the file is genuine:",
            "",
            "  ```powershell",
            f"  (Get-FileHash .\\{windows[0]} -Algorithm SHA256).Hash -eq '{windows[1]}'",
            "  ```",
        ]
    return "\n".join(lines).rstrip("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="release tag or version, e.g. v1.2.3")
    parser.add_argument("--dist", type=Path, default=Path("dist"), help="folder holding the *.sha256 files")
    parser.add_argument("--readme", type=Path, default=Path(__file__).resolve().parent.parent / "README.md")
    parser.add_argument("--repo", default="Cosmos-0118/fluxget")
    args = parser.parse_args()

    version = args.version.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        sys.exit(f"{args.version!r} is not a release version like v1.2.3")
    entries = sorted(read_checksums(args.dist), key=sort_key)
    if not entries:
        sys.exit(f"No *.sha256 files found in {args.dist}")

    # newline="" keeps the file's existing line endings untouched.
    text = args.readme.read_text(encoding="utf-8", newline="")
    start, end = text.find(START), text.find(END)
    if start < 0 or end < start:
        sys.exit(f"README is missing the {START} / {END} markers")
    eol = "\r\n" if "\r\n" in text else "\n"
    block = build_block(version, args.repo, entries).replace("\n", eol)
    updated = text[:start] + START + eol + block + eol + text[end:]
    if updated == text:
        print(f"README already up to date for v{version}.")
        return
    args.readme.write_text(updated, encoding="utf-8", newline="")
    print(f"Updated {args.readme} for v{version}.")


if __name__ == "__main__":
    main()
