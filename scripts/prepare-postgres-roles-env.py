#!/usr/bin/env python3
"""Add independent PostgreSQL role passwords to an existing Histae env file."""
import argparse
import os
from pathlib import Path
import re
import secrets
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("env_file", nargs="?", type=Path, default=Path(".env"))
    args = parser.parse_args()
    path = args.env_file.absolute()
    if path.is_symlink() or not path.is_file():
        parser.error("the env file must exist and must not be a symbolic link")
    source = path.read_text(encoding="utf-8")
    values = {}
    for line in source.splitlines():
        match = re.fullmatch(r"\s*(POSTGRES_(?:ADMIN|MIGRATE|APP)_PASSWORD)\s*=\s*(.*?)\s*", line)
        if match:
            name, value = match.groups()
            if name in values:
                parser.error("a PostgreSQL role password is defined more than once")
            value = value.strip("\"'")
            if value and not value.startswith("REPLACE_") and not re.fullmatch("[a-f0-9]{64}", value):
                parser.error("role passwords must contain 64 lowercase hexadecimal characters")
            values[name] = value if re.fullmatch("[a-f0-9]{64}", value) else secrets.token_hex(32)
    for role in ("ADMIN", "MIGRATE", "APP"):
        values.setdefault(f"POSTGRES_{role}_PASSWORD", secrets.token_hex(32))
    if len(set(values.values())) != 3:
        parser.error("the three PostgreSQL role passwords must be different")
    replacements = dict(values, POSTGRES_USER="histae_app", POSTGRES_PASSWORD="${POSTGRES_APP_PASSWORD}")
    seen = set()
    output = []
    roles_inserted = False
    for line in source.splitlines():
        match = re.match(r"\s*([A-Z_]+)\s*=", line)
        if match and match.group(1) in replacements:
            name = match.group(1)
            if name in seen:
                parser.error("a PostgreSQL setting is defined more than once")
            seen.add(name)
            if name in values:
                continue
            if not roles_inserted:
                # dotenvy expands references in file order. Define APP_PASSWORD
                # before POSTGRES_PASSWORD=${POSTGRES_APP_PASSWORD} for native Rust.
                output.extend(f"{key}={value}" for key, value in values.items())
                roles_inserted = True
            output.append(f"{name}={replacements[name]}")
        else:
            output.append(line)
    if not roles_inserted:
        output.extend(f"{name}={value}" for name, value in values.items())
    output.extend(f"{name}={value}" for name, value in replacements.items()
                  if name not in seen and name not in values)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".env.postgres-tmp-", delete=False) as writer:
            temporary = Path(writer.name)
            os.fchmod(writer.fileno(), 0o600)
            writer.write("\n".join(output) + "\n")
            writer.flush()
            os.fsync(writer.fileno())
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    print("PostgreSQL role settings prepared; passwords were not displayed.")
    print("An existing development database must be converted before recreating its containers.")


if __name__ == "__main__":
    main()
