#!/usr/bin/env python3
"""Convert the existing Rust development database without deleting its data."""
import argparse
import json
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", type=Path, default=Path(".env"))
    parser.add_argument("--container", default="histae-rust-dev-postgres-1")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if not re.fullmatch(r"histae-rust-dev-postgres-[0-9]+", args.container):
        parser.error("only a Rust development PostgreSQL container is accepted")

    def docker(*arguments, input=None):
        result = subprocess.run(["docker", *arguments], input=input, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if result.returncode:
            # Config and SQL errors may contain interpolated passwords.
            code = re.search(r"(?m)^(?:ERROR|FATAL):\s+([0-9A-Z]{5})\s*$", result.stderr)
            if code:
                parser.error(f"PostgreSQL conversion failed (SQLSTATE {code.group(1)}); transaction rolled back")
            parser.error("Docker/SQL failed; no diagnostic containing secrets was displayed")
        return result.stdout

    metadata = json.loads(docker("inspect", args.container))[0]
    labels = metadata["Config"].get("Labels") or {}
    environment = dict(entry.split("=", 1) for entry in metadata["Config"]["Env"] if "=" in entry)
    if labels.get("com.docker.compose.project") != "histae-rust-dev" \
            or labels.get("com.docker.compose.service") != "postgres" \
            or environment.get("POSTGRES_DB") != "histae-dev" \
            or not metadata["State"]["Running"]:
        parser.error("the running Rust development database could not be verified")
    old_user = environment.get("POSTGRES_USER", "")
    if not re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_]*", old_user):
        parser.error("invalid original PostgreSQL administrator")
    model = json.loads(docker("compose", "--env-file", str(args.env_file.resolve()),
                              "-f", str(root / "compose.yaml"), "-f", str(root / "compose.dev.yaml"),
                              "config", "--format", "json"))
    pg_env = model["services"]["postgres"]["environment"]
    passwords = {
        "admin_password": pg_env["POSTGRES_PASSWORD"],
        "migrate_password": pg_env["HISTAE_BOOTSTRAP_MIGRATE_PASSWORD"],
        "app_password": pg_env["HISTAE_BOOTSTRAP_APP_PASSWORD"],
    }
    if any(not re.fullmatch("[a-f0-9]{64}", value) for value in passwords.values()) \
            or len(set(passwords.values())) != 3:
        parser.error("three distinct 64-character hexadecimal passwords are required")
    if pg_env["POSTGRES_DB"] != "histae-dev":
        parser.error("the env file does not select histae-dev")
    sql = "\\set bootstrap_mode convert-dev\n"
    sql += "".join(f"\\set {name} '{value}'\n" for name, value in passwords.items())
    sql += (root / "db/000_bootstrap_postgres.sql").read_text()
    docker("exec", "-i", args.container, "psql", "-X", "-q", "--set", "ON_ERROR_STOP=1",
           "--set", "VERBOSITY=sqlstate",
           "-U", old_user, "-d", "histae-dev", input=sql)
    print("Development PostgreSQL roles converted; application data was preserved.")
    print("Recreate the development containers with the same env file to use the new roles.")


if __name__ == "__main__":
    main()
