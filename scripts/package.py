"""Package the native release binary, README, and license; no external backends."""
import hashlib
from pathlib import Path
import platform
import zipfile

root = Path(__file__).resolve().parents[1]
suffix = ".exe" if platform.system() == "Windows" else ""
binary = root / "target" / "release" / ("fluxget" + suffix)
destination = root / "dist"
destination.mkdir(exist_ok=True)
archive = destination / f"fluxget-{platform.system().lower()}-{platform.machine().lower()}.zip"
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as out:
    out.write(binary, binary.name)
    for name in ["README.md", "LICENSE", "THIRD_PARTY_NOTICES.txt", "docs/RESEARCH.md", "docs/BENCHMARK_WINDOWS.md", "docs/BENCHMARK_WINDOWS.json", "docs/VALIDATION_WINDOWS.md"]:
        if (root / name).exists():
            out.write(root / name, name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
print(archive)
