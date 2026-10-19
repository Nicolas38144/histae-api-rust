#!/usr/bin/env python3
"""Run continuation and related PostgreSQL suites against a disposable local instance."""
import argparse
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
TESTS = (
    "matches_postgres", "privacy_rights_postgres", "account_erasure_postgres",
    "administration_reports_postgres", "billing_postgres",
    "maintenance_outbox_postgres", "notifications_postgres",
    "discovery_postgres", "profiles_postgres",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-image", default="histae-api:local")
    parser.add_argument("--postgres-image", default="histae-postgres:18.6-postgis3.6.4")
    parser.add_argument("--test", action="append", choices=TESTS)
    parser.add_argument("--sudo-docker", action="store_true", help="Use sudo for Docker only, keep Cargo unprivileged")
    args = parser.parse_args()
    docker_command = ["sudo", "docker"] if args.sudo_docker else ["docker"]
    name = f"histae-continuation-postgres-test-{os.getpid()}"
    created = False
    os.umask(0o077)

    def docker(*arguments, input_text=None):
        result = subprocess.run([*docker_command, *arguments], input=input_text,
                                text=True, capture_output=True, cwd=ROOT)
        if result.returncode:
            # Never print bootstrap credentials, SQL or container environment.
            raise RuntimeError(f"Docker verification step failed: {arguments[0]}")
        return result.stdout.strip()

    with tempfile.TemporaryDirectory(prefix="histae-continuation-") as temporary:
        directory = Path(temporary)
        admin_password, migrate_password, app_password = [secrets.token_hex(32) for _ in range(3)]
        postgres = dict(POSTGRES_DB="histae-dev", POSTGRES_USER="histae_admin",
                        POSTGRES_PASSWORD=admin_password,
                        POSTGRES_INITDB_ARGS="--auth-host=scram-sha-256",
                        HISTAE_BOOTSTRAP_MIGRATE_PASSWORD=migrate_password,
                        HISTAE_BOOTSTRAP_APP_PASSWORD=app_password)
        runtime = dict(ENV="development", POSTGRES_HOST="127.0.0.1", POSTGRES_PORT="5432",
                       POSTGRES_DB="histae-dev", POSTGRES_USER="histae_migrate",
                       POSTGRES_PASSWORD=migrate_password, POSTGRES_SSLMODE="disable",
                       POSTGRES_POOL_MAX="4", JWT_SECRET=secrets.token_hex(32),
                       PHONE_ENCRYPTION_KEY=secrets.token_hex(32), PHONE_HASH_KEY=secrets.token_hex(32),
                       RATE_LIMIT_STORE="memory", SMS_PROVIDER="disabled", BILLING_PROVIDER="disabled",
                       PUSH_PROVIDER="disabled", PHOTO_MODERATION_PROVIDER="disabled",
                       ADVERTISING_PROVIDER="disabled", METRICS_ENABLED="false",
                       TERMS_OF_SERVICE_VERSION="continuation-test-v1", PRIVACY_POLICY_VERSION="continuation-test-v1",
                       ADMIN_WEBAUTHN_ORIGIN="http://localhost:5173", ADMIN_WEBAUTHN_RP_ID="localhost")
        for file, values in [("postgres.env", postgres), ("migrate.env", runtime)]:
            path = directory / file
            path.write_text("".join(f"{key}={value}\n" for key, value in values.items()))
            path.chmod(0o600)
        try:
            print("Starting isolated PostgreSQL for continuation tests…", flush=True)
            docker("run", "-d", "--name", name, "--network", "bridge",
                   "--publish", "127.0.0.1::5432", "--memory", "1g", "--cpus", "2",
                   "--env-file", str(directory / "postgres.env"),
                   "--tmpfs", "/var/lib/postgresql:rw,size=512m",
                   "--mount", f"type=bind,src={ROOT / 'db'},dst=/etc/histae/db,readonly",
                   "--mount", f"type=bind,src={ROOT / 'docker/postgres/init-roles.sh'},dst=/docker-entrypoint-initdb.d/001_roles.sh,readonly",
                   args.postgres_image, "postgres", "-c", "shared_preload_libraries=pg_stat_statements")
            created = True
            for _ in range(90):
                ready = subprocess.run([*docker_command, "exec", name, "pg_isready", "-h", "127.0.0.1",
                                        "-U", "histae_admin", "-d", "histae-dev"], capture_output=True)
                if ready.returncode == 0:
                    break
                time.sleep(1)
            else:
                raise RuntimeError("Isolated PostgreSQL readiness timed out")
            # Use the shipped binary and the limited migrator, with TCP/SCRAM.
            for _ in range(2):
                docker("run", "--rm", "--network", f"container:{name}",
                       "--env-file", str(directory / "migrate.env"), "--user", "1000:1000",
                       "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
                       "--entrypoint", "/app/bin/db-migrate", args.api_image)
            def query(statement, role="histae_app"):
                return docker("exec", "-i", name, "sh", "-ec",
                    'case "$1" in histae_app) export PGPASSWORD="$HISTAE_BOOTSTRAP_APP_PASSWORD" ;; histae_migrate) export PGPASSWORD="$HISTAE_BOOTSTRAP_MIGRATE_PASSWORD" ;; *) exit 2 ;; esac; exec psql -X -qAt -v ON_ERROR_STOP=1 -h 127.0.0.1 -U "$1" -d histae-dev',
                    "sh", role, input_text=statement)

            # Simulate the pre-credit/pre-pause schema, then prove the upgrade and
            # that a repeated migration preserves choices made under the new rules.
            first, second, match_id = [str(uuid.uuid4()) for _ in range(3)]
            query(f"INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted) VALUES ('{first}','legacy-credit-first',''::bytea),('{second}','legacy-credit-second',''::bytea); "
                  f"INSERT INTO match_init(id,user1_id,user2_id,status,expires_at,continuation_initiator_id) VALUES ('{match_id}',LEAST('{first}'::uuid,'{second}'::uuid),GREATEST('{first}'::uuid,'{second}'::uuid),'awaiting_continuation',clock_timestamp()+INTERVAL '24 hours','{first}'); "
                  f"INSERT INTO match_state(match_id,user_id,continued) VALUES ('{match_id}','{first}',true),('{match_id}','{second}',false); "
                  f"INSERT INTO continuation_usage(user_id,week_start,used_count,free_used_count) VALUES ('{first}',date_trunc('week',clock_timestamp() AT TIME ZONE 'UTC')::date,2,2);")
            assert query(f"SELECT discovery_paused FROM user_account WHERE user_id='{first}';") == "f"
            query("DROP TABLE continuation_reservation,advertising_reward_intent,continuation_wallet; ALTER TABLE continuation_usage DROP COLUMN free_used_count; ALTER TABLE user_account DROP COLUMN discovery_paused;", "histae_migrate")
            def migrate():
                docker("run", "--rm", "--network", f"container:{name}",
                       "--env-file", str(directory / "migrate.env"), "--user", "1000:1000",
                       "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
                       "--entrypoint", "/app/bin/db-migrate", args.api_image)
            migrate()
            assert query(f"SELECT discovery_paused FROM user_account WHERE user_id='{first}';") == "f"
            assert query(f"SELECT used_count||':'||free_used_count FROM continuation_usage WHERE user_id='{first}';") == "2:2"
            assert query(f"SELECT count(*) FROM match_state WHERE match_id='{match_id}' AND continued;") == "0"
            assert query(f"SELECT continuation_initiator_id IS NULL FROM match_init WHERE id='{match_id}';") == "t"
            query(f"UPDATE match_state SET continued=true WHERE match_id='{match_id}' AND user_id='{first}'; INSERT INTO continuation_reservation(match_id,user_id,source,week_start) VALUES ('{match_id}','{first}','free',date_trunc('week',clock_timestamp() AT TIME ZONE 'UTC')::date);")
            query(f"UPDATE user_account SET discovery_paused=true WHERE user_id='{first}';")
            migrate()
            assert query(f"SELECT discovery_paused FROM user_account WHERE user_id='{first}';") == "t"
            assert query(f"SELECT count(*) FROM match_state WHERE match_id='{match_id}' AND continued;") == "1"
            assert query(f"SELECT count(*) FROM continuation_reservation WHERE match_id='{match_id}';") == "1"
            assert query(f"SELECT used_count||':'||free_used_count FROM continuation_usage WHERE user_id='{first}';") == "2:2"
            query(f"DELETE FROM user_account WHERE user_id IN ('{first}','{second}');")
            print("Legacy quota/pause migrations and repeated upgrade passed.", flush=True)
            port = docker("port", name, "5432/tcp").split(":")[-1]
            runtime.update(POSTGRES_PORT=port, POSTGRES_USER="histae_app", POSTGRES_PASSWORD=app_password)
            environment = os.environ.copy()
            environment.update(runtime)
            environment.setdefault("CARGO_BUILD_JOBS", "2")
            command = ["cargo", "test", "--locked", "--features", "postgres-integration"]
            for target in args.test or TESTS:
                command.extend(["--test", target])
            command.extend(["--", "--test-threads=1"])
            result = subprocess.run(command, cwd=ROOT, env=environment)
            if result.returncode:
                raise RuntimeError("PostgreSQL continuation regression failed")
            print("Isolated continuation PostgreSQL verification passed.", flush=True)
        finally:
            if created:
                docker("rm", "-f", name)
            print("Temporary PostgreSQL, fixtures and credentials removed.", flush=True)


if __name__ == "__main__":
    main()
