#!/bin/sh
# Install only our native command metadata publisher; preserve other Pi extensions.
set -eu
umask 077
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_file="$root/pi-extension/commands.ts"
agent_dir=${PI_CODING_AGENT_DIR:-"$HOME/.pi/agent"}
case "$agent_dir" in /*) ;; *) printf '%s\n' 'PI_CODING_AGENT_DIR must be an absolute path.' >&2; exit 1;; esac
target="$agent_dir/extensions/herdr-webapp-commands.ts"
if [ ! -f "$source_file" ]; then
  printf '%s\n' 'The command publisher is missing from this checkout.' >&2
  exit 1
fi
mkdir -p -- "$agent_dir/extensions"
if [ -e "$target" ] || [ -L "$target" ]; then
  if [ -L "$target" ] && [ "$(readlink "$target")" = "$source_file" ]; then
    printf '%s\n' 'Native Pi command publisher is already installed. Use /reload in existing Pi sessions.'
    exit 0
  fi
  printf '%s\n' "Refusing to replace an existing extension: $target" >&2
  exit 1
fi
ln -s -- "$source_file" "$target"
printf '%s\n' 'Native Pi command publisher installed. Use /reload in existing Pi sessions.'
