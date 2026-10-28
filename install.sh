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

# ---- Console UI ----------------------------------------------------------
# One line per step. The active step is redrawn in place (flowing wave, shimmering
# progress bar); finished steps keep a single summary line. Output that is not an
# interactive terminal (pipes, CI, FLUXGET_PLAIN=1) gets plain "[n/total]" lines.
# The release bootstrap hands its step count to the payload installer through
# FLUXGET_UI_* so both halves read as one continuous run.
ui_step=0 ui_total=0 ui_child=0 ui_live=0
if [[ "${FLUXGET_UI_STEP:-}" =~ ^[0-9]+$ && "${FLUXGET_UI_TOTAL:-}" =~ ^[0-9]+$ ]]; then
    ui_step=$FLUXGET_UI_STEP ui_total=$FLUXGET_UI_TOTAL ui_child=1
fi
if [[ "${FLUXGET_UI_LIVE:-}" == [01] ]]; then ui_live=$FLUXGET_UI_LIVE
elif [[ -t 1 && -z "${FLUXGET_PLAIN:-}" && -z "${CI:-}" && "${TERM:-dumb}" != dumb ]]; then ui_live=1; fi
if [[ -z "${NO_COLOR:-}" ]]; then
    c_dim=$'\033[90m' c_cy=$'\033[36m' c_wh=$'\033[97m' c_gr=$'\033[32m' c_rd=$'\033[31m' c_b=$'\033[1m' c_off=$'\033[0m'
else c_dim='' c_cy='' c_wh='' c_gr='' c_rd='' c_b='' c_off=''; fi
case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
    *[Uu][Tt][Ff]-8*|*[Uu][Tt][Ff]8*)
        g_full='█' g_empty='░' g_ok='✔' g_fail='✘' g_skip='–' g_dot='   ·   '
        g_wave=(▁ ▂ ▃ ▄ ▅ ▆ ▇ █) ;;
    *)
        g_full='#' g_empty='-' g_ok='+' g_fail='x' g_skip='-' g_dot='   -   '
        g_wave=(_ . - '~' = + '*' '#') ;;
esac
# One period of a sine wave, scaled to the eight block heights.
wave_shape=(4 5 6 7 7 7 6 5 4 2 1 0 0 0 1 2)

ui_now_ms() {
    local t
    t=$(date +%s%N 2>/dev/null || true)
    case "$t" in ''|*N) echo $(($(date +%s) * 1000)) ;; *) echo $((t / 1000000)) ;; esac
}

# Sets REPLY to a size like "3.0 MB".
ui_size() {
    local b=$1 t
    if ((b >= 1048576)); then t=$((b * 10 / 1048576)); REPLY="$((t / 10)).$((t % 10)) MB"
    elif ((b >= 1024)); then REPLY="$(((b + 512) / 1024)) KB"
    else REPLY="$b B"; fi
}

# Sets REPLY to a coloured wave of $2 cells for frame $1.
ui_wave() {
    local f=$1 n=$2 i h col
    REPLY=''
    for ((i = 0; i < n; i++)); do
        h=${wave_shape[$(((f + i * 2) % 16))]}
        if ((h >= 6)); then col=$c_wh; elif ((h >= 3)); then col=$c_cy; else col=$c_cy${c_off:+$'\033[2m'}; fi
        REPLY+="$col${g_wave[$h]}$c_off"
    done
}

# Sets REPLY to a progress bar. $1 = permille done (or -1 if unknown), $2 = cells, $3 = frame.
ui_bar() {
    local pm=$1 n=$2 f=$3 i filled glint d at head
    REPLY=''
    if ((pm >= 0)); then
        filled=$((pm * n / 1000))
        glint=$((f % (filled + 8) - 4))
        for ((i = 0; i < n; i++)); do
            if ((i < filled)); then
                d=$((i - glint)); if ((d < 0)); then d=$((-d)); fi
                if ((d <= 1)); then REPLY+="$c_wh$g_full"; else REPLY+="$c_cy$g_full"; fi
            else REPLY+="$c_dim$g_empty"; fi
        done
    else
        # Total size unknown: a comet bounces along the track instead.
        at=$((f % (2 * (n - 1))))
        if ((at < n)); then head=$at; else head=$((2 * (n - 1) - at)); fi
        for ((i = 0; i < n; i++)); do
            d=$((i - head)); if ((d < 0)); then d=$((-d)); fi
            if ((d == 0)); then REPLY+="$c_wh$g_full"
            elif ((d <= 2)); then REPLY+="$c_cy$g_full"
            else REPLY+="$c_dim$g_empty"; fi
        done
    fi
    REPLY+=$c_off
}

ui_header() {
    ((!ui_child)) || return 0
    if ((!ui_live)); then printf 'fluxget installer (%s)\n' "$1"; return 0; fi
    ui_wave 0 7
    printf '\n   %s   %sfluxget%s   %sinstaller%s\n\n   %s%s%s\n\n\n' "$REPLY" "$c_b$c_wh" "$c_off" "$c_dim" "$c_off" "$c_dim" "$1" "$c_off"
}

# ui_line ok|skip|fail LABEL DETAIL
ui_line() {
    local mark col
    ui_step=$((ui_step + 1))
    if ((!ui_live)); then printf '[%s/%s] %s  %s\n' "$ui_step" "$ui_total" "$2" "$3"; return 0; fi
    case "$1" in
        ok) mark=$g_ok col=$c_gr ;;
        skip) mark=$g_skip col=$c_dim ;;
        *) mark=$g_fail col=$c_rd ;;
    esac
    printf '\r\033[2K  %s%-3s%s   %s%s%s      %s%-13s%s   %s%s%s\n' "$c_dim" "$ui_step/$ui_total" "$c_off" "$col" "$mark" "$c_off" "$c_wh" "$2" "$c_off" "$c_dim" "$3" "$c_off"
}

ui_finish() {
    if ((!ui_live)); then printf 'Installed %s to %s\n' "$1" "$2"; return 0; fi
    printf '\n  %s%s%s   %sInstalled %s%s\n      %s%s%s\n\n' "$c_gr" "$g_ok" "$c_off" "$c_b$c_wh" "$1" "$c_off" "$c_dim" "$2" "$c_off"
}

# ui_download URL DEST LABEL: curl with a live progress line. The size comes from the response
# headers curl itself dumps, so no extra request is made.
ui_download() {
    local url=$1 dest=$2 label=$3 name hdr err pid status=0 total='' size=0 pm cells cols tail
    local t0 now frame=0 last_t last_size speed=0 inst wave_text shown percent detail left bar_text
    name=${dest##*/}
    if ((!ui_live)); then
        curl --fail --silent --show-error --location --retry 3 "$url" -o "$dest" || return 1
        ui_size "$(wc -c <"$dest")"
        ui_line ok "${label%ing}ed" "$name$g_dot$REPLY"
        return 0
    fi
    hdr="$dest.headers" err="$dest.err"
    curl --fail --silent --show-error --location --retry 3 -D "$hdr" "$url" -o "$dest" 2>"$err" &
    pid=$!
    trap 'kill "$pid" 2>/dev/null; printf "\033[?25h\n"; exit 130' INT TERM
    # Two rows are reserved and redrawn in place: the bar, then the numbers under it.
    printf '\033[?25l\n\n'
    t0=$(ui_now_ms) last_t=$t0 last_size=0
    while kill -0 "$pid" 2>/dev/null; do
        now=$(ui_now_ms)
        if [[ -z "$total" && -s "$hdr" ]]; then
            total=$(awk '/^HTTP\//{ok=($2==200); len=""} ok && tolower($1)=="content-length:"{len=$2+0} END{if(len>0)print len}' "$hdr")
        fi
        size=$(wc -c <"$dest" 2>/dev/null || echo 0); size=$((size + 0))
        if ((now - last_t >= 500)); then
            inst=$(((size - last_size) * 1000 / (now - last_t)))
            if ((speed == 0)); then speed=$inst; else speed=$(((speed * 6 + inst * 4) / 10)); fi
            last_t=$now last_size=$size
        fi
        cols=${COLUMNS:-$(tput cols 2>/dev/null || echo 80)}
        pm=-1 percent='' left=''
        ui_size "$size"; detail=$REPLY
        if [[ -n "$total" ]]; then
            if ((size > total)); then size=$total; fi
            pm=$((size * 1000 / total))
            percent=$(printf '%5s%%' "$((pm / 10)).$((pm % 10))")
            ui_size "$size"; shown=$REPLY; ui_size "$total"
            detail="$shown of $REPLY"
        fi
        if ((speed > 0)); then ui_size "$speed"; detail+="     $REPLY/s"; fi
        if [[ -n "$total" ]] && ((speed > 1)); then
            left=$(((total - size) / speed + 1))
            if ((left >= 60)); then detail+="     ETA $((left / 60))m $(printf '%02d' $((left % 60)))s"; else detail+="     ETA ${left}s"; fi
        fi
        cells=$((cols - 1 - 31 - 3 - 6))
        if ((cells > 30)); then cells=30; fi
        frame=$(((now - t0) / 70))
        ui_wave "$frame" 4; wave_text=$REPLY
        bar_text=''
        if ((cells >= 8)); then ui_bar "$pm" "$cells" "$frame"; bar_text="$REPLY   "; fi
        printf '\033[2A\r\033[2K  %s%-3s%s   %s   %s%-13s%s   %s%s%s%s\n\r\033[2K               %s%s%s\n' \
            "$c_dim" "$((ui_step + 1))/$ui_total" "$c_off" "$wave_text" "$c_wh" "$label" "$c_off" \
            "$bar_text" "$c_wh" "$percent" "$c_off" "$c_dim" "$detail" "$c_off"
        sleep 0.06
    done
    wait "$pid" || status=$?
    # Wipe the two rows and leave the cursor on the first, ready for the finished-step line.
    printf '\033[2A\r\033[2K\n\033[2K\033[1A\r\033[?25h'
    trap - INT TERM
    if ((status != 0)); then
        ui_line fail 'Download failed' "$name"
        cat "$err" >&2
        rm -f -- "$hdr" "$err"
        return 1
    fi
    now=$(ui_now_ms)
    size=$(wc -c <"$dest"); size=$((size + 0))
    rm -f -- "$hdr" "$err"
    t0=$((now - t0)); if ((t0 < 100)); then t0=100; fi
    ui_size "$size"; shown=$REPLY
    ui_size $((size * 1000 / t0))
    ui_line ok "${label%ing}ed" "$name$g_dot$shown in $((t0 / 1000)).$(((t0 % 1000) / 100))s ($REPLY/s)"
}
# --------------------------------------------------------------------------

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
    ui_total=6
    if [[ "$release_version" == latest ]]; then release_name='latest release'; else release_name="release $release_version"; fi
    case "$release_os" in macos) os_name=macOS ;; *) os_name=Linux ;; esac
    ui_header "$os_name $release_arch$g_dot$release_name"
    ui_download "$release_url" "$download_dir/$asset" Downloading
    curl --fail --silent --show-error --location --retry 3 "$release_url.sha256" -o "$download_dir/$asset.sha256"
    expected=$(awk -v asset="$asset" 'length($1) == 64 && $2 == asset { print $1 }' "$download_dir/$asset.sha256")
    if command -v shasum >/dev/null 2>&1; then actual=$(shasum -a 256 "$download_dir/$asset" | awk '{print $1}')
    else actual=$(sha256sum "$download_dir/$asset" | awk '{print $1}'); fi
    [[ "$expected" == "$actual" ]] || { printf '%s\n' 'Release archive SHA-256 mismatch; existing installation preserved.' >&2; exit 1; }
    ui_line ok Verified 'SHA-256 matches the published checksum'
    mkdir "$download_dir/package"
    tar -xzf "$download_dir/$asset" -C "$download_dir/package"
    ui_line ok Unpacked 'release package extracted'
    export FLUXGET_UI_STEP=$ui_step FLUXGET_UI_TOTAL=$ui_total FLUXGET_UI_LIVE=$ui_live
    options=(--prefix "$install_root")
    # Default Unix bin is ~/.local/bin; preserve it when handing off to the payload.
    if [[ "$bin_dir" == "$HOME/.local/bin" ]]; then options=(); fi
    if ((!modify_path)); then options+=(--no-modify-path); fi
    bash "$download_dir/package/install.sh" ${options[@]+"${options[@]}"}
    exit 0
fi

source_binary=''
for candidate in "$package_dir/fluxget" "$package_dir/bin/fluxget" "$package_dir/target/release/fluxget"; do
    if [[ -f "$candidate" ]]; then source_binary=$candidate; break; fi
done
if ((!ui_child)); then
    if [[ -n "$source_binary" ]]; then ui_total=3; else ui_total=4; fi
    ui_header 'installing from this package'
fi
if [[ -z "$source_binary" ]]; then
    [[ -f "$package_dir/Cargo.toml" ]] || { printf '%s\n' 'No executable in package; download the release for your OS and CPU.' >&2; exit 1; }
    cargo_bin=$(command -v cargo || true)
    if [[ -z "$cargo_bin" && -x "$HOME/.cargo/bin/cargo" ]]; then cargo_bin="$HOME/.cargo/bin/cargo"; fi
    [[ -n "$cargo_bin" ]] || { printf '%s\n' 'Source installation needs Rust 1.91+. Use a prebuilt release for installation without a compiler.' >&2; exit 1; }
    printf '%s\n' 'Building fluxget from source (first installation only)...'
    "$cargo_bin" build --release --locked --manifest-path "$package_dir/Cargo.toml" --target-dir "$package_dir/target"
    source_binary="$package_dir/target/release/fluxget"
    ui_line ok Built 'compiled from source'
fi

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
    else sha256sum "$1" | awk '{print $1}'; fi
}

checked_detail='version test passed'
if [[ -f "$package_dir/SHA256SUMS" ]]; then
    checked_detail='checksum and version test passed'
    expected=$(awk 'length($1) == 64 && $2 == "fluxget" { print $1 }' "$package_dir/SHA256SUMS")
    actual=$(sha256_file "$source_binary")
    [[ "$expected" == "$actual" ]] || { printf '%s\n' 'Package SHA-256 mismatch; installation stopped.' >&2; exit 1; }
fi

# Finder/Archive Utility and third-party ZIP tools can disagree on executable bits.
chmod u+x "$source_binary"
version=$("$source_binary" --version)
[[ "$version" == fluxget\ [0-9]* ]] || { printf '%s\n' 'Executable version check failed; installation stopped.' >&2; exit 1; }
ui_line ok Checked "$version$g_dot$checked_detail"
mkdir -p -- "$bin_dir" "$install_root"
temporary_binary=$(mktemp "$bin_dir/.fluxget.XXXXXX")
trap 'rm -f -- "$temporary_binary"' EXIT
cp -- "$source_binary" "$temporary_binary"
chmod 755 "$temporary_binary"
[[ "$(sha256_file "$temporary_binary")" == "$(sha256_file "$source_binary")" ]] || { printf '%s\n' 'Copied executable checksum failed.' >&2; exit 1; }
if [[ -f "$bin_dir/fluxget" ]] && [[ "$(sha256_file "$bin_dir/fluxget")" == "$(sha256_file "$source_binary")" ]]; then
    already_installed=1
else
    already_installed=0
    mv -f -- "$temporary_binary" "$bin_dir/fluxget"
fi
for name in README.md LICENSE THIRD_PARTY_NOTICES.txt; do
    if [[ -f "$package_dir/$name" && ! "$package_dir/$name" -ef "$install_root/$name" ]]; then
        cp -- "$package_dir/$name" "$install_root/$name"
    fi
done
# fx is a short link to fluxget. Leave an unrelated fx alone.
short_ok=0
if [[ -L "$bin_dir/fx" && "$(readlink "$bin_dir/fx")" == fluxget ]] || [[ ! -e "$bin_dir/fx" && ! -L "$bin_dir/fx" ]]; then
    ln -sfn fluxget "$bin_dir/fx" && short_ok=1
fi
if ((short_ok)); then installed_detail='fluxget and fx in place, with documentation'
else installed_detail='executable and documentation in place'; fi
if ((already_installed)); then ui_line skip Installed "$version is already installed; refreshing PATH and documentation."
else ui_line ok Installed "$installed_detail"; fi

path_changed=0
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
            path_changed=1
        fi
    done
    if ((path_changed)); then ui_line ok PATH 'added to your shell profiles'
    else ui_line skip PATH 'already set in your shell profiles'; fi
else
    ui_line skip PATH 'left unchanged (--no-modify-path)'
fi
ui_finish "$version" "$bin_dir/fluxget"
ui_indent=''
if ((ui_live)); then ui_indent='  '; fi
if ((modify_path)); then printf '%s%s\n' "$ui_indent" 'Open a new terminal and run: fx --help   (or fluxget --help)'
else printf '%s%s\n' "$ui_indent" 'Shell profiles were left unchanged (--no-modify-path).'; fi
if ((ui_live)); then printf '\n'; fi
