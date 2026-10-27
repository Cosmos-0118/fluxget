"""Build native release archives with checksum-verified, repeatable installers."""
import argparse
import hashlib
import io
from pathlib import Path
import platform
import tarfile
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    system = {"Windows": "windows", "Darwin": "macos", "Linux": "linux"}[platform.system()]
    arch = {"AMD64": "x86_64", "x86_64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64", "ARM64": "aarch64"}[platform.machine()]
    binary_name = "fluxget.exe" if system == "windows" else "fluxget"
    binary = args.binary or root / "target" / "release" / binary_name
    binary_data = binary.read_bytes()
    checksums = f"{hashlib.sha256(binary_data).hexdigest()}  {binary_name}\n".encode("ascii")
    entries = [(binary_name, binary_data, 0o755), ("SHA256SUMS", checksums, 0o644)]
    for name in ["README.md", "LICENSE", "THIRD_PARTY_NOTICES.txt", "install.ps1", "Install.cmd", "install.sh", "Install.command", "docs/RESEARCH.md", "docs/BENCHMARK_WINDOWS.md", "docs/BENCHMARK_WINDOWS.json", "docs/VALIDATION_WINDOWS.md"]:
        if (root / name).exists():
            entries.append((name, (root / name).read_bytes(), 0o755 if name.endswith((".sh", ".command")) else 0o644))
    destination = root / "dist"
    destination.mkdir(exist_ok=True)
    stem = f"fluxget-{system}-{arch}"
    archive = destination / f"{stem}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as out:
        for name, data, mode in entries:
            entry = zipfile.ZipInfo(name)
            entry.create_system = 3
            entry.external_attr = (0o100000 | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            out.writestr(entry, data)
    archives = [archive]
    if system != "windows":
        archive = destination / f"{stem}.tar.gz"
        with tarfile.open(archive, "w:gz") as out:
            for name, data, mode in entries:
                entry = tarfile.TarInfo(name)
                entry.size, entry.mode = len(data), mode
                out.addfile(entry, io.BytesIO(data))
        archives.append(archive)
    for archive in archives:
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
        print(archive)


if __name__ == "__main__":
    main()
