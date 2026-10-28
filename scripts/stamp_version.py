"""Set the crate version in Cargo.toml and Cargo.lock from a release tag such as v1.2.3.

The release workflow runs this before building so the binary's --version, the package
names and the tag always agree, with no manual bump."""
import re
import sys
from pathlib import Path

tag = sys.argv[1] if len(sys.argv) > 1 else ""
match = re.fullmatch(r"v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)", tag)
if not match:
    sys.exit(f"Tag {tag!r} is not a release tag like v1.2.3")
version = match[1]

root = Path(__file__).resolve().parent.parent
manifest = root / "Cargo.toml"
text, count = re.subn(r'(?m)^version = ".*"$', f'version = "{version}"', manifest.read_text(), count=1)
if count != 1:
    sys.exit("No version line found in Cargo.toml")
manifest.write_text(text)

lock = root / "Cargo.lock"
text, count = re.subn(r'(name = "fluxget"\nversion = )".*"', rf'\1"{version}"', lock.read_text(), count=1)
if count != 1:
    sys.exit("fluxget entry not found in Cargo.lock")
lock.write_text(text)
print(f"Stamped fluxget {version}")
