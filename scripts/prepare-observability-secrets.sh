#!/usr/bin/env bash
set -euo pipefail

if (( $# > 1 )); then
  printf 'Usage: %s [env-file]\n' "$0" >&2
  exit 2
fi
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
python3 - "${1:-${SCRIPT_DIR}/../.env}" <<'PYSECRETS'
from pathlib import Path
import os
import re
import secrets
import sys
import tempfile

env = Path(sys.argv[1]).absolute()
if env.is_symlink() or not env.is_file():
    raise SystemExit('The env file must exist and must not be a symbolic link.')
owner = env.stat()
source = env.read_text(encoding='utf-8')
pattern = r'^[ \t]*(?:export[ \t]+)?METRICS_TOKEN[ \t]*=[ \t]*(.*)$'
rows = re.findall(pattern, source, re.M)
if len(rows) > 1:
    raise SystemExit('Duplicate METRICS_TOKEN in env file.')
value = rows[0].strip() if rows else ''
if value.startswith(('"', "'")):
    match = re.fullmatch(r"(['\"])(.*?)\1[ \t]*(?:#.*)?", value)
    if not match:
        raise SystemExit('Invalid METRICS_TOKEN quoting.')
    token = match.group(2)
else:
    token = re.split(r'[ \t]+#|^#', value, maxsplit=1)[0].strip()
if token and not re.fullmatch(r'[A-Za-z0-9._~+/=-]{32,}', token):
    raise SystemExit('Set a dedicated literal METRICS_TOKEN of at least 32 characters, without interpolation.')

def write_private(path, contents, mode):
    if path.is_symlink():
        raise SystemExit('Monitoring files must not be symbolic links.')
    if path.is_file() and path.read_text(encoding='utf-8') == contents:
        path.chmod(mode)
        if os.geteuid() == 0:
            os.chown(path, owner.st_uid, owner.st_gid)
        return
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix='.monitoring-')
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as writer:
            writer.write(contents)
            writer.flush()
            os.fsync(writer.fileno())
            os.fchmod(writer.fileno(), mode)
            if os.geteuid() == 0:
                os.fchown(writer.fileno(), owner.st_uid, owner.st_gid)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)

directory = env.parent / '.secrets'
if directory.is_symlink():
    raise SystemExit('The secrets directory must not be a symbolic link.')
directory.mkdir(exist_ok=True)
directory.chmod(0o700)
if os.geteuid() == 0:
    os.chown(directory, owner.st_uid, owner.st_gid)
if not token:
    token = secrets.token_hex(32)
    source = re.sub(pattern, lambda _: 'METRICS_TOKEN=' + token, source, flags=re.M) if rows else source.rstrip('\n') + '\nMETRICS_TOKEN=' + token + '\n'
    write_private(env, source, 0o600)
env.chmod(0o600)

# The private parent protects host access. Service UIDs must read Docker's bind-mounted secrets.
write_private(directory / 'histae_metrics_token', token, 0o444)
password = directory / 'histae_grafana_admin_password'
if password.is_symlink():
    raise SystemExit('Monitoring files must not be symbolic links.')
if password.exists():
    if len(password.read_text(encoding='utf-8').strip()) < 32:
        raise SystemExit('The existing Grafana bootstrap password must contain at least 32 characters.')
    password.chmod(0o444)
else:
    write_private(password, secrets.token_hex(32), 0o444)
print('Monitoring secrets prepared; no credentials were displayed.')
PYSECRETS
