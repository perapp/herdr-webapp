#!/bin/sh
# Private GitHub release installer. Requires authenticated gh, Python 3.9+, and sh.
# gh release download --repo perapp/herdr-webapp --pattern install.sh --output - | sh
# HERDR_WEB_VERSION=x.y.z selects an exact published, non-prerelease version.
set -eu
umask 077
repo=perapp/herdr-webapp
GH_HOST=github.com
GH_PROMPT_DISABLED=1
export GH_HOST GH_PROMPT_DISABLED
fail() { printf 'herdr-webapp: %s\n' "$*" >&2; exit 1; }
for tool in gh python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "requires $tool (install it first)"
done
install_dir=${HERDR_WEB_INSTALL_DIR:-${XDG_DATA_HOME:-"$HOME/.local/share"}/herdr-webapp}
bin_dir=${HERDR_WEB_BIN_DIR:-"$HOME/.local/bin"}
# Reject ambiguous paths, including parent traversal and symlinked ancestors.
python3 - "$install_dir" "$bin_dir" <<'PY'
import pathlib, sys
if sys.version_info < (3, 9):
    raise SystemExit('herdr-webapp: Python 3.9 or newer is required')
for value in sys.argv[1:]:
    p = pathlib.Path(value)
    if not p.is_absolute() or '..' in p.parts or p == pathlib.Path('/') or str(p) != value:
        raise SystemExit('herdr-webapp: install/bin directories must be normalized absolute paths')
    if any(parent.is_symlink() for parent in (p, *p.parents)):
        raise SystemExit('herdr-webapp: install/bin directories must not use symlinked paths')
a, b = map(pathlib.Path, sys.argv[1:])
if a == b or a in b.parents or b in a.parents:
    raise SystemExit('herdr-webapp: install and bin directories must be separate')
PY
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin/arm64|Darwin/aarch64) target=aarch64-apple-darwin ;;
  Darwin/*) fail 'only Apple Silicon macOS is supported; Intel Macs are not supported' ;;
  *) fail 'supported platforms: Linux GNU x86_64/aarch64 and Apple Silicon macOS (no Windows)' ;;
esac
if [ "$(uname -s)" = Linux ]; then
  # GNU builds require glibc; do not pretend musl/Alpine is supported.
  getconf GNU_LIBC_VERSION >/dev/null 2>&1 || fail 'Linux builds require glibc (Ubuntu 22.04 baseline); musl is not supported'
fi
requested=${HERDR_WEB_VERSION:-}
if [ -n "$requested" ]; then
  python3 - "$requested" <<'PY'
import re, sys
if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', sys.argv[1]):
    raise SystemExit('herdr-webapp: HERDR_WEB_VERSION must be exact stable x.y.z (without v)')
PY
fi
parent=$(dirname -- "$install_dir")
mkdir -p -- "$parent"
# The backend requires an owned, non-group/world-writable parent as well.
python3 - "$parent" <<'PY'
import os, pathlib, sys
p = pathlib.Path(sys.argv[1])
s = p.stat()
if s.st_uid != os.getuid() or s.st_mode & 0o022:
    raise SystemExit('herdr-webapp: installation parent must be owned by you and not group/world writable')
PY
lock="${install_dir}.update-lock"
stage=
locked=false
swapped=false
committed=false
link_stage=
cleanup() {
  status=$?
  trap - 0 HUP INT TERM
  if [ "$swapped" = true ] && [ "$committed" != true ]; then
    rm -rf -- "$install_dir"
    if [ -d "$stage/backup" ]; then
      if ! mv -- "$stage/backup" "$install_dir"; then
        printf 'herdr-webapp: rollback needs manual recovery from %s/backup\n' "$stage" >&2
        stage=
        status=1
      fi
    fi
  fi
  if [ -n "$link_stage" ]; then rm -rf -- "$link_stage"; fi
  if [ -n "$stage" ]; then rm -rf -- "$stage"; fi
  if [ "$locked" = true ]; then rmdir -- "$lock" 2>/dev/null || true; fi
  exit "$status"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
mkdir -- "$lock" 2>/dev/null || fail 'another installer/updater owns the update lock; wait for it to finish (do not delete an active lock)'
locked=true
# The updater uses this same lock and sibling PID record. Never signal a core or terminal.
python3 - "${install_dir}.pid" <<'PY'
import errno, os, pathlib, sys
p = pathlib.Path(sys.argv[1])
if p.is_symlink():
    raise SystemExit('herdr-webapp: refusing symlinked PID record')
if p.exists():
    text = p.read_text().strip()
    if not text.isascii() or not text.isdigit() or int(text) <= 0:
        raise SystemExit('herdr-webapp: invalid PID record; stop the web server and inspect it first')
    try:
        os.kill(int(text), 0)
    except ProcessLookupError:
        pass
    except PermissionError:
        raise SystemExit('herdr-webapp: recorded server may be running; use its UI updater or stop only the web server first')
    else:
        raise SystemExit('herdr-webapp: server is running; use its UI updater or stop only the web server first')
PY
# A receipt is ownership, not authorization to overwrite arbitrary user files.
python3 - "$install_dir" "$bin_dir/herdr-webapp" "$target" <<'PY'
import json, pathlib, re, sys
root, link = map(pathlib.Path, sys.argv[1:3])
if root.is_symlink():
    raise SystemExit('herdr-webapp: refusing symlinked installation')
if root.exists():
    receipt = root / 'install.json'
    try:
        if not root.is_dir() or receipt.is_symlink() or not receipt.is_file():
            raise ValueError()
        data = json.loads(receipt.read_text())
        if (set(data) != {'schemaVersion', 'repository', 'target', 'version'}
            or type(data['schemaVersion']) is not int or data['schemaVersion'] != 1
            or data['repository'] != 'perapp/herdr-webapp' or data['target'] != sys.argv[3]
            or not isinstance(data['version'], str)
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', data['version'])):
            raise ValueError()
    except (OSError, ValueError, TypeError):
        raise SystemExit('herdr-webapp: refusing unmanaged installation (valid install.json required)')
if link.is_symlink():
    if str(link.readlink()) != str(root / 'herdr-webapp'):
        raise SystemExit('herdr-webapp: refusing to overwrite unmanaged CLI symlink')
elif link.exists():
    raise SystemExit('herdr-webapp: refusing to overwrite unmanaged CLI file/directory')
PY
stage=$(mktemp -d "$parent/.herdr-webapp-install.XXXXXXXX")
mkdir -- "$stage/download" "$stage/payload"
# gh keeps OAuth credentials in its own store; never serialize them or signed URLs.
# Suppress remote error bodies: they can contain private URLs.
gh auth status --hostname github.com >/dev/null 2>&1 || fail 'authenticate GitHub CLI first: gh auth login --hostname github.com'
endpoint="repos/$repo/releases/latest"
if [ -n "$requested" ]; then endpoint="repos/$repo/releases/tags/v$requested"; fi
if ! gh api --hostname github.com "$endpoint" --jq 'if (.draft == false and .prerelease == false) then .tag_name else empty end' >"$stage/tag" 2>/dev/null; then
  fail 'cannot read published release metadata; check gh authentication and repository access'
fi
tag=$(tr -d '\r\n' < "$stage/tag")
version=${tag#v}
python3 - "$tag" "$requested" <<'PY'
import re, sys
if not re.fullmatch(r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', sys.argv[1]):
    raise SystemExit('herdr-webapp: no stable published release found')
if sys.argv[2] and sys.argv[1] != 'v' + sys.argv[2]:
    raise SystemExit('herdr-webapp: release version mismatch')
PY
asset="herdr-webapp-v$version-$target.tar.gz"
if ! gh release download "$tag" --repo "$repo" --pattern "$asset" --pattern SHA256SUMS --dir "$stage/download" >/dev/null 2>&1; then
  fail 'release download failed; check gh authentication, repository access, and available platform assets'
fi
# Verify exact bytes, then validate ALL members before extracting any. No links,
# devices, duplicate paths, absolute paths, traversal, or unrecognized payloads.
python3 - "$stage" "$asset" "$target" "$version" <<'PY'
import hashlib, json, pathlib, re, shutil, sys, tarfile
stage = pathlib.Path(sys.argv[1])
asset, target, version = sys.argv[2:]
archive = stage / 'download' / asset
try:
    lines = (stage / 'download/SHA256SUMS').read_text().splitlines()
    checksums = {}
    for line in lines:
        match = re.fullmatch(r'([a-fA-F0-9]{64}) [ *]([^/\\\s]+)', line)
        if not match or match[2] in checksums:
            raise ValueError('invalid checksum manifest')
        checksums[match[2]] = match[1].lower()
    digest = hashlib.sha256()
    with archive.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            digest.update(chunk)
    if digest.hexdigest() != checksums.get(asset):
        raise ValueError('checksum mismatch')
    required = {'herdr-webapp', 'pi-extension/commands.ts', 'scripts/install-pi-extension.sh'}
    with tarfile.open(archive, 'r:gz') as tar:
        members = tar.getmembers()
        names = [m.name for m in members]
        if (len(names) != len(set(names)) or not required.issubset(names)
            or not set(names).issubset(required | {'LICENSE'})
            or any(not m.isfile() or m.size > 512 * 1024 * 1024 for m in members)):
            raise ValueError('unsafe or incomplete archive')
        for member in members:
            path = stage / 'payload' / member.name
            path.parent.mkdir(parents=True, exist_ok=True)
            with tar.extractfile(member) as src, path.open('wb') as dst:
                shutil.copyfileobj(src, dst)
            path.chmod(0o755 if member.name in {'herdr-webapp', 'scripts/install-pi-extension.sh'} else 0o644)
    receipt = {'schemaVersion': 1, 'repository': 'perapp/herdr-webapp', 'target': target, 'version': version}
    (stage / 'payload/install.json').write_text(json.dumps(receipt, separators=(',', ':')) + '\n')
except (OSError, ValueError, tarfile.TarError) as e:
    raise SystemExit('herdr-webapp: invalid release download: ' + str(e))
PY
# Run only verified bytes; this also rejects an incompatible CPU/glibc baseline.
if ! actual=$("$stage/payload/herdr-webapp" --version 2>/dev/null); then
  fail 'downloaded binary cannot run on this platform (Linux requires glibc 2.35 or newer)'
fi
[ "$actual" = "herdr-webapp $version" ] || fail 'downloaded binary version does not match release'
mkdir -p -- "$bin_dir"
link_stage=$(mktemp -d "$bin_dir/.herdr-webapp-link.XXXXXXXX")
ln -s -- "$install_dir/herdr-webapp" "$link_stage/herdr-webapp"
# Prepare rollback before either rename. The existing installation never gets
# unpacked into or modified in place. Both directory renames stay on one device.
if [ -d "$install_dir" ]; then mv -- "$install_dir" "$stage/backup"; fi
swapped=true
mv -- "$stage/payload" "$install_dir"
# Python os.replace avoids mv following a destination symlink-to-directory.
python3 - "$link_stage/herdr-webapp" "$bin_dir/herdr-webapp" <<'PY'
import os, sys
os.replace(sys.argv[1], sys.argv[2])
PY
committed=true
printf 'Installed herdr-webapp %s (%s). Run: %s/herdr-webapp\n' "$version" "$target" "$bin_dir"
printf '%s\n' 'Pi command metadata is optional: run the included scripts/install-pi-extension.sh only if wanted.'
case ":$PATH:" in *":$bin_dir:"*) ;; *) printf 'Add %s to PATH if needed.\n' "$bin_dir" ;; esac
