#!/usr/bin/env bash
set -euo pipefail

script_file=${BASH_SOURCE[0]:-}
package_dir=''
if [[ -n "$script_file" && -f "$script_file" ]]; then
    package_dir=$(CDPATH='' cd -- "$(dirname -- "$script_file")" && pwd)
fi
install_root="${HOME}/.local/share/fluxget"
bin_dir="${HOME}/.local/bin"
modify_path=1
release_version=latest
release_base='https://github.com/Cosmos-0118/fluxget/releases'

usage() {
    printf '%s\n' 'Usage: bash install.sh [--prefix DIR] [--no-modify-path] [--version v0.1.0]' \
        'Default: executable in ~/.local/bin; license/docs in ~/.local/share/fluxget.' \
        '--prefix DIR installs to DIR/bin with docs in DIR (useful for isolated installs).'
}

while (($#)); do
    case "$1" in
        --prefix)
            [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
            install_root=$2
            bin_dir="$2/bin"
            shift 2 ;;
        --no-modify-path) modify_path=0; shift ;;
        --version|--release-base-url)
            [[ $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
            if [[ "$1" == --version ]]; then release_version=$2; else release_base=$2; fi
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

case "$(uname -s)" in
    Darwin|Linux) ;;
    *) printf '%s\n' 'Use Install.cmd on Windows. This installer supports macOS and Linux.' >&2; exit 1 ;;
esac

if [[ -z "$package_dir" || (! -f "$package_dir/fluxget" && ! -f "$package_dir/bin/fluxget" && ! -f "$package_dir/target/release/fluxget" && ! -f "$package_dir/Cargo.toml") ]]; then
    case "$(uname -s)" in Darwin) release_os=macos ;; Linux) release_os=linux ;; esac
    case "$(uname -m)" in x86_64|amd64) release_arch=x86_64 ;; arm64|aarch64) release_arch=aarch64 ;;
        *) printf 'Unsupported CPU: %s\n' "$(uname -m)" >&2; exit 1 ;; esac
    case "$release_version" in latest) release_path=latest/download ;;
        v[0-9]*.[0-9]*.[0-9]*)
            [[ "$release_version" != *[!a-zA-Z0-9.-]* ]] || { printf '%s\n' 'Invalid version.' >&2; exit 2; }
            release_path="download/$release_version" ;;
        *) printf '%s\n' 'Version must be latest or a tag such as v0.1.0.' >&2; exit 2 ;; esac
    case "$release_base" in https://*|http://127.0.0.1:*|http://localhost:*) ;;
        *) printf '%s\n' 'Release URL requires HTTPS (loopback HTTP is allowed for tests).' >&2; exit 2 ;; esac
    asset="fluxget-$release_os-$release_arch.tar.gz"
    release_url="${release_base%/}/$release_path/$asset"
    download_dir=$(mktemp -d "${TMPDIR:-/tmp}/fluxget-install.XXXXXX")
    trap 'rm -rf -- "$download_dir"' EXIT
    printf 'Fetching %s %s %s release...\n' "$release_version" "$release_os" "$release_arch"
    curl --fail --silent --show-error --location --retry 3 "$release_url" -o "$download_dir/$asset"
    curl --fail --silent --show-error --location --retry 3 "$release_url.sha256" -o "$download_dir/$asset.sha256"
    expected=$(awk -v asset="$asset" 'length($1) == 64 && $2 == asset { print $1 }' "$download_dir/$asset.sha256")
    if command -v shasum >/dev/null 2>&1; then actual=$(shasum -a 256 "$download_dir/$asset" | awk '{print $1}')
    else actual=$(sha256sum "$download_dir/$asset" | awk '{print $1}'); fi
    [[ "$expected" == "$actual" ]] || { printf '%s\n' 'Release archive SHA-256 mismatch; existing installation preserved.' >&2; exit 1; }
    mkdir "$download_dir/package"
    tar -xzf "$download_dir/$asset" -C "$download_dir/package"
    options=(--prefix "$install_root")
    # Default Unix bin is ~/.local/bin; preserve it when handing off to the payload.
    if [[ "$bin_dir" == "$HOME/.local/bin" ]]; then options=(); fi
    if ((!modify_path)); then options+=(--no-modify-path); fi
    bash "$download_dir/package/install.sh" "${options[@]}"
    exit 0
fi

source_binary=''
for candidate in "$package_dir/fluxget" "$package_dir/bin/fluxget" "$package_dir/target/release/fluxget"; do
    if [[ -f "$candidate" ]]; then source_binary=$candidate; break; fi
done
if [[ -z "$source_binary" ]]; then
    [[ -f "$package_dir/Cargo.toml" ]] || { printf '%s\n' 'No executable in package; download the release for your OS and CPU.' >&2; exit 1; }
    cargo_bin=$(command -v cargo || true)
    if [[ -z "$cargo_bin" && -x "$HOME/.cargo/bin/cargo" ]]; then cargo_bin="$HOME/.cargo/bin/cargo"; fi
    [[ -n "$cargo_bin" ]] || { printf '%s\n' 'Source installation needs Rust 1.91+. Use a prebuilt release for installation without a compiler.' >&2; exit 1; }
    printf '%s\n' 'Building fluxget from source (first installation only)...'
    "$cargo_bin" build --release --locked --manifest-path "$package_dir/Cargo.toml" --target-dir "$package_dir/target"
    source_binary="$package_dir/target/release/fluxget"
fi

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
    else sha256sum "$1" | awk '{print $1}'; fi
}

if [[ -f "$package_dir/SHA256SUMS" ]]; then
    expected=$(awk 'length($1) == 64 && $2 == "fluxget" { print $1 }' "$package_dir/SHA256SUMS")
    actual=$(sha256_file "$source_binary")
    [[ "$expected" == "$actual" ]] || { printf '%s\n' 'Package SHA-256 mismatch; installation stopped.' >&2; exit 1; }
fi

# Finder/Archive Utility and third-party ZIP tools can disagree on executable bits.
chmod u+x "$source_binary"
version=$("$source_binary" --version)
[[ "$version" == fluxget\ [0-9]* ]] || { printf '%s\n' 'Executable version check failed; installation stopped.' >&2; exit 1; }
mkdir -p -- "$bin_dir" "$install_root"
temporary_binary=$(mktemp "$bin_dir/.fluxget.XXXXXX")
trap 'rm -f -- "$temporary_binary"' EXIT
cp -- "$source_binary" "$temporary_binary"
chmod 755 "$temporary_binary"
[[ "$(sha256_file "$temporary_binary")" == "$(sha256_file "$source_binary")" ]] || { printf '%s\n' 'Copied executable checksum failed.' >&2; exit 1; }
if [[ -f "$bin_dir/fluxget" ]] && [[ "$(sha256_file "$bin_dir/fluxget")" == "$(sha256_file "$source_binary")" ]]; then
    printf '%s is already installed; refreshing PATH and documentation.\n' "$version"
else
    mv -f -- "$temporary_binary" "$bin_dir/fluxget"
fi
for name in README.md LICENSE THIRD_PARTY_NOTICES.txt; do
    if [[ -f "$package_dir/$name" && ! "$package_dir/$name" -ef "$install_root/$name" ]]; then
        cp -- "$package_dir/$name" "$install_root/$name"
    fi
done

if ((modify_path)); then
    # %q emits literal shell-safe paths, including spaces and metacharacters.
    printf -v quoted_bin '%q' "$bin_dir"
    path_line="export PATH=${quoted_bin}:\"\$PATH\" # fluxget installer"
    profiles=("$HOME/.profile")
    login_shell=${SHELL:-/bin/sh}
    case "${login_shell##*/}" in
        zsh) profiles+=("${ZDOTDIR:-$HOME}/.zprofile" "${ZDOTDIR:-$HOME}/.zshrc") ;;
        bash) profiles+=("$HOME/.bash_profile" "$HOME/.bashrc") ;;
    esac
    for profile in "${profiles[@]}"; do
        mkdir -p -- "$(dirname -- "$profile")"
        if ! grep -Fqx -- "$path_line" "$profile" 2>/dev/null; then
            printf '\n%s\n' "$path_line" >> "$profile"
        fi
    done
fi
printf 'Installed %s to %s/fluxget\n' "$version" "$bin_dir"
if ((modify_path)); then printf '%s\n' 'Open a new terminal and run: fluxget --help'
else printf '%s\n' 'Shell profiles were left unchanged (--no-modify-path).'; fi
