#!/bin/sh
# Served at /run or /main/run. The server substitutes the canonical channel URL
# into __HERDR_RELAY_ORIGIN__ (origin or origin/main, no shell syntax).
set -eu
umask 077
origin='__HERDR_RELAY_ORIGIN__'
case "$origin" in
    https://*) ;;
    *) printf '%s\n' 'Relay launcher requires an HTTPS relay origin.' >&2; exit 1 ;;
esac
# Defense in depth: the substituted origin is data, never evaluated shell code.
case "$origin" in
    *[!a-zA-Z0-9.:/\[\]-]*|https://|https://*/) printf '%s\n' 'Invalid relay endpoint.' >&2; exit 1 ;;
esac
authority=${origin#https://}
case "$authority" in
    */main) authority=${authority%/main} ;;
esac
case "$authority" in
    ''|*/*) printf '%s\n' 'Invalid relay endpoint path.' >&2; exit 1 ;;
esac
command -v python3 >/dev/null 2>&1 || { printf '%s\n' 'Python 3.9 or newer is required.' >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required.' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/herdr-relay.XXXXXXXX")
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
# No redirects; an incomplete/error response never reaches Python.
curl --fail --silent --show-error --proto '=https' --max-time 60 --max-filesize 1048576 \
    --output "$work/launcher.py" "$origin/client/launcher.py"
# With curl|sh stdin is the script pipe. Restore an interactive terminal when available.
# Python remains a child so the shell can always remove its temporary bootstrap.
if [ -r /dev/tty ] && ( : </dev/tty ) 2>/dev/null; then
    python3 "$work/launcher.py" --relay-url "$origin" </dev/tty &
else
    python3 "$work/launcher.py" --relay-url "$origin" </dev/null &
fi
child=$!
# Forward signals only to our own child, and wait for it to reap its connector.
trap 'kill -HUP "$child" 2>/dev/null || :; wait "$child" || :; exit 129' HUP
trap 'kill -INT "$child" 2>/dev/null || :; wait "$child" || :; exit 130' INT
trap 'kill -TERM "$child" 2>/dev/null || :; wait "$child" || :; exit 143' TERM
set +e
wait "$child"
status=$?
exit "$status"
