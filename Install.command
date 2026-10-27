#!/usr/bin/env bash
set -euo pipefail
package_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
status=0
bash "$package_dir/install.sh" "$@" || status=$?
if [[ -t 0 ]]; then read -r -p 'Press Return to close this window...' _ || true; fi
exit "$status"
