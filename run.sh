#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
if [[ ! -x target/release/herdr-webapp ]] || [[ "${HERDR_WEB_REBUILD:-0}" == 1 ]]; then
    make release
fi
exec ./target/release/herdr-webapp "$@"
