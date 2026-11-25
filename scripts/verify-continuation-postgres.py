#!/usr/bin/env python3
"""Run continuation and related PostgreSQL suites against a disposable local instance."""
import argparse
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
TESTS = (
    "mobile_identity", "phone_blocklist_postgres",
    "complimentary_premium_postgres", "web_privacy_postgres",
    "play_review_postgres", "matches_postgres", "matches_inactivity_postgres", "moderation_postgres", "privacy_rights_postgres", "account_erasure_postgres",
    "administration_reports_postgres", "admin_metrics_postgres", "query_performance_postgres",
    "maintenance_outbox_postgres", "notifications_postgres", "postgres_locks",
    "discovery_postgres", "profiles_postgres", "catalog_postgres", "otp_delivery", "localization_postgres", "store_billing_postgres",
)
# These fixtures create their own disposable databases; application-role suites
# must continue to run separately with histae_app.
ADMIN_TESTS = ("sms_safety_postgres", "outbox_postgres")
# This target also contains a separate S3 integration test.
POSTGRES_FILTERS = {"media_integration": "postgres_protocol_preserves_replay_replacement_and_consumption"}


class VerificationError(RuntimeError):
    """A verification failure whose message contains no credentials or SQL."""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-image", default="histae-api-rust:local",
                        help="Locally built API image containing the current migrator")
    parser.add_argument("--postgres-image", default="histae-postgres:18.7-postgis3.7.0")
    parser.add_argument("--test", action="append", choices=TESTS + ADMIN_TESTS + tuple(POSTGRES_FILTERS))
    parser.add_argument("--native-migrator", type=Path, help="Use a freshly built local db-migrate binary over the published loopback port")
    parser.add_argument("--native-phone-sync", type=Path, help="Optional live public-feed smoke after isolated tests; never sends SMS")
    parser.add_argument("--sudo-docker", action="store_true", help="Use sudo for Docker only, keep Cargo unprivileged")
    args = parser.parse_args()
    docker_command = ["sudo", "docker"] if args.sudo_docker else ["docker"]
    name = f"histae-continuation-postgres-test-{os.getpid()}"
    created = False
    os.umask(0o077)

    def docker(*arguments, input_text=None, failure_message=None):
        result = subprocess.run([*docker_command, *arguments], input=input_text,
                                text=True, capture_output=True, cwd=ROOT)
        if result.returncode:
            # Never print bootstrap credentials, SQL or container environment.
            codes = sorted(set(re.findall(r"\berror_code=([a-z][a-z0-9_]{0,63})\b",
                                          result.stdout + result.stderr)))
            details = f"exit {result.returncode}"
            if codes:
                details += f"; error_code={','.join(codes)}"
            message = failure_message or f"Docker verification step failed: {arguments[0]}"
            raise VerificationError(f"{message} ({details})")
        return result.stdout.strip()

    # Fail before creating a database or attempting an implicit registry pull.
    docker("info", "--format", "{{.ServerVersion}}",
           failure_message="Docker is not accessible; check the daemon and permissions, or use --sudo-docker")
    docker("image", "inspect", args.postgres_image, "--format", "{{.Id}}",
           failure_message=f"Required local PostgreSQL image is unavailable: {args.postgres_image}")
    if args.native_migrator:
        if not args.native_migrator.is_file() or not os.access(args.native_migrator, os.X_OK):
            raise VerificationError("Native migrator is missing or not executable; run cargo build --locked --bin db-migrate")
    else:
        docker("image", "inspect", args.api_image, "--format", "{{.Id}}",
               failure_message=f"Required local API image is unavailable: {args.api_image}. "
                               "Build the current API image and select it with --api-image, or run "
                               "cargo build --locked --bin db-migrate and use --native-migrator target/debug/db-migrate")

    if args.native_phone_sync and (not args.native_phone_sync.is_file() or not os.access(args.native_phone_sync, os.X_OK)):
        raise VerificationError("Native phone synchronizer is missing or not executable; build phone-blocklist-sync first")

    with tempfile.TemporaryDirectory(prefix="histae-continuation-") as temporary:
        directory = Path(temporary)
        admin_password, migrate_password, app_password, sync_password = [secrets.token_hex(32) for _ in range(4)]
        postgres = dict(POSTGRES_DB="histae-dev", POSTGRES_USER="histae_admin",
                        POSTGRES_PASSWORD=admin_password,
                        POSTGRES_INITDB_ARGS="--auth-host=scram-sha-256",
                        HISTAE_BOOTSTRAP_MIGRATE_PASSWORD=migrate_password,
                        HISTAE_BOOTSTRAP_APP_PASSWORD=app_password, HISTAE_BOOTSTRAP_PHONE_SYNC_PASSWORD=sync_password)
        runtime = dict(GOOGLE_PLAY_REVIEW_ENABLED="false", ENV="development", POSTGRES_HOST="127.0.0.1", POSTGRES_PORT="5432",
                       POSTGRES_DB="histae-dev", POSTGRES_USER="histae_migrate",
                       POSTGRES_PASSWORD=migrate_password, POSTGRES_SSLMODE="disable",
                       POSTGRES_POOL_MAX="4", JWT_SECRET=secrets.token_hex(32),
                       PHONE_ENCRYPTION_KEY=secrets.token_hex(32), PHONE_HASH_KEY=secrets.token_hex(32),
                       RATE_LIMIT_STORE="memory", SMS_PROVIDER="disabled",
                       APP_STORE_ENABLED="false", GOOGLE_PLAY_ENABLED="false",
                       PUSH_PROVIDER="disabled",
                       ADVERTISING_PROVIDER="disabled", METRICS_ENABLED="false",
                       TERMS_OF_SERVICE_VERSION="continuation-test-v1", PRIVACY_POLICY_VERSION="continuation-test-v1",
                       ADMIN_WEBAUTHN_ORIGIN="http://localhost:5173", ADMIN_WEBAUTHN_RP_ID="localhost")
        for file, values in [("postgres.env", postgres), ("migrate.env", runtime)]:
            path = directory / file
            path.write_text("".join(f"{key}={value}\n" for key, value in values.items()))
            path.chmod(0o600)
        try:
            print("Starting isolated PostgreSQL for continuation tests…", flush=True)
            docker("run", "--pull", "never", "-d", "--name", name, "--network", "bridge",
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
            port = docker("port", name, "5432/tcp").split(":")[-1]
            def migrate():
                if args.native_migrator:
                    environment = os.environ.copy()
                    environment.update(runtime, POSTGRES_PORT=port)
                    result = subprocess.run([str(args.native_migrator.resolve())], cwd=ROOT, env=environment)
                    if result.returncode:
                        raise VerificationError("Native migration verification failed")
                else:
                    docker("run", "--pull", "never", "--rm", "--network", f"container:{name}",
                           "--env-file", str(directory / "migrate.env"), "--user", "1000:1000",
                           "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
                           "--entrypoint", "/app/bin/db-migrate", args.api_image,
                           failure_message=f"Migration with local API image {args.api_image} failed")
            migrate()

            def query(statement):
                return docker("exec", "-i", name, "sh", "-ec",
                    'export PGPASSWORD="$HISTAE_BOOTSTRAP_APP_PASSWORD"; exec psql -X -qAt -v ON_ERROR_STOP=1 -h 127.0.0.1 -U histae_app -d histae-dev',
                    input_text=statement)

            runtime.update(POSTGRES_PORT=port, POSTGRES_USER="histae_app", POSTGRES_PASSWORD=app_password)
            environment = os.environ.copy()
            environment.update(runtime)
            environment.update(HISTAE_PHONE_BLOCKLIST_TEST_ISOLATED="true", PHONE_SYNC_TEST_PASSWORD=sync_password)
            environment.setdefault("CARGO_BUILD_JOBS", "2")
            selected = args.test or (TESTS + ADMIN_TESTS + tuple(POSTGRES_FILTERS))
            for administrator_fixture in (False, True):
                targets = [target for target in selected if target not in POSTGRES_FILTERS and (target in ADMIN_TESTS) == administrator_fixture]
                if not targets:
                    continue
                command = ["cargo", "test", "--locked", "--features", "postgres-integration"]
                for target in targets:
                    command.extend(["--test", target])
                command.extend(["--", "--test-threads=1"])
                test_environment = environment.copy()
                if administrator_fixture:
                    print("Running disposable database-creation fixtures with the temporary test administrator.", flush=True)
                    test_environment.update(POSTGRES_USER="histae_admin", POSTGRES_PASSWORD=admin_password)
                result = subprocess.run(command, cwd=ROOT, env=test_environment)
                if result.returncode:
                    raise VerificationError("PostgreSQL continuation regression failed")
            for target, test_filter in POSTGRES_FILTERS.items():
                if target not in selected:
                    continue
                result = subprocess.run(
                    ["cargo", "test", "--locked", "--features", "postgres-integration",
                     "--test", target, test_filter, "--", "--test-threads=1"],
                    cwd=ROOT, env=environment)
                if result.returncode:
                    raise VerificationError("PostgreSQL media regression failed")
            if args.native_phone_sync:
                sync_environment = environment.copy()
                sync_environment.update(POSTGRES_USER="histae_phone_sync", POSTGRES_PASSWORD=sync_password)
                completed = subprocess.run([str(args.native_phone_sync.resolve())], cwd=ROOT, env=sync_environment)
                if completed.returncode:
                    raise VerificationError("Live phone feed smoke failed; no SMS provider was contacted")
                assert query("SELECT count(*)=2 AND bool_and(last_error IS NULL AND last_success_at IS NOT NULL) FROM phone_blocklist_source;") == "t"
                print("Live imports: " + query("SELECT string_agg(source||':'||imported_count, ', ' ORDER BY source) FROM phone_blocklist_source;"), flush=True)
                print("Both live public phone feeds imported with restricted credentials; no SMS sent.", flush=True)
            print("Isolated continuation PostgreSQL verification passed.", flush=True)
        finally:
            if created:
                docker("rm", "-f", name)
            print("Temporary PostgreSQL, fixtures and credentials removed.", flush=True)


if __name__ == "__main__":
    try:
        main()
    except VerificationError as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
