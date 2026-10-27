"""Collect dependency license texts from locally fetched Cargo packages.

Run after cargo fetch. The result is intentionally inclusive: Cargo metadata also
lists dependencies for other supported targets and optional features.
"""
import argparse
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--cargo", default="cargo")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output([args.cargo, "metadata", "--locked", "--format-version", "1"], cwd=root))
sections = ["FLUXGET THIRD-PARTY LICENSE NOTICES", "Generated from Cargo.lock and downloaded dependency source packages.", "Includes target-specific and optional dependencies; inclusion does not imply all code is in every binary.", ""]
missing = []
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if package["source"] is None:
        continue
    directory = Path(package["manifest_path"]).parent
    sections.extend(["=" * 78, f'{package["name"]} {package["version"]}', f'License expression: {package.get("license")}', f'Repository: {package.get("repository")}', ""])
    files = [p for p in directory.iterdir() if p.is_file() and p.name.lower().startswith(("license", "copying", "copyright", "notice"))]
    for name in ("licenses", "LICENSES"):
        if (directory / name).is_dir():
            files.extend(p for p in (directory / name).rglob("*") if p.is_file())
    if package.get("license_file"):
        explicit = directory / package["license_file"]
        if explicit.is_file() and explicit not in files:
            files.append(explicit)
    if not files:
        missing.append(package["name"])
        sections.append("No standalone license file in the published package; see upstream repository and license expression above.")
    for file in sorted(set(files)):
        sections.extend([f"--- {file.relative_to(directory).as_posix()} ---", file.read_text(encoding="utf-8", errors="replace"), ""])
(root / "THIRD_PARTY_NOTICES.txt").write_text("\n".join(sections), encoding="utf-8")
print(f"Generated notices for {len(metadata['packages'])-1} packages; no standalone text: {', '.join(missing) or 'none'}")
