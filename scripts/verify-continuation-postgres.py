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
import uuid

ROOT = Path(__file__).resolve().parents[1]
TESTS = (
    "matches_postgres", "moderation_postgres", "privacy_rights_postgres", "account_erasure_postgres",
    "administration_reports_postgres", "billing_postgres",
    "maintenance_outbox_postgres", "notifications_postgres",
    "discovery_postgres", "profiles_postgres", "catalog_postgres", "otp_delivery", "localization_postgres", "store_billing_postgres",
)
# These fixtures create their own disposable databases; application-role suites
# must continue to run separately with histae_app.
ADMIN_TESTS = ("sms_safety_postgres",)
# This target also contains a separate S3 integration test.
POSTGRES_FILTERS = {"media_integration": "postgres_protocol_preserves_replay_replacement_and_consumption"}


class VerificationError(RuntimeError):
    """A verification failure whose message contains no credentials or SQL."""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-image", default="histae-api:local",
                        help="Locally built API image containing the current migrator")
    parser.add_argument("--postgres-image", default="histae-postgres:18.6-postgis3.6.4")
    parser.add_argument("--test", action="append", choices=TESTS + ADMIN_TESTS + tuple(POSTGRES_FILTERS))
    parser.add_argument("--native-migrator", type=Path, help="Use a freshly built local db-migrate binary over the published loopback port")
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
                       APP_STORE_ENABLED="false", GOOGLE_PLAY_ENABLED="false", WEB_BILLING_ENABLED="false",
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
            for _ in range(2):
                migrate()
            def query(statement, role="histae_app"):
                return docker("exec", "-i", name, "sh", "-ec",
                    'case "$1" in histae_app) export PGPASSWORD="$HISTAE_BOOTSTRAP_APP_PASSWORD" ;; histae_migrate) export PGPASSWORD="$HISTAE_BOOTSTRAP_MIGRATE_PASSWORD" ;; *) exit 2 ;; esac; exec psql -X -qAt -v ON_ERROR_STOP=1 -h 127.0.0.1 -U "$1" -d histae-dev',
                    "sh", role, input_text=statement)

            # Simulate the pre-store/pre-credit/pre-pause schema, then prove the upgrade and
            # that a repeated migration preserves choices made under the new rules.
            first, second, match_id = [str(uuid.uuid4()) for _ in range(3)]
            query(f"INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted) VALUES ('{first}','legacy-credit-first',''::bytea),('{second}','legacy-credit-second',''::bytea); "
                  f"INSERT INTO match_init(id,user1_id,user2_id,status,expires_at,continuation_initiator_id) VALUES ('{match_id}',LEAST('{first}'::uuid,'{second}'::uuid),GREATEST('{first}'::uuid,'{second}'::uuid),'awaiting_continuation',clock_timestamp()+INTERVAL '24 hours','{first}'); "
                  f"INSERT INTO match_state(match_id,user_id,continued) VALUES ('{match_id}','{first}',true),('{match_id}','{second}',false); "
                  f"INSERT INTO continuation_usage(user_id,week_start,used_count,free_used_count) VALUES ('{first}',date_trunc('week',clock_timestamp() AT TIME ZONE 'UTC')::date,2,2);")
            assert query("SELECT NOT EXISTS (SELECT 1 FROM trait CROSS JOIN (VALUES('en'),('es'),('it')) langs(language) WHERE NOT EXISTS (SELECT 1 FROM trait_translation t WHERE t.trait_id=trait.id AND t.language=langs.language)) AND NOT EXISTS (SELECT 1 FROM profile_question CROSS JOIN (VALUES('en'),('es'),('it')) langs(language) WHERE NOT EXISTS (SELECT 1 FROM profile_question_translation t WHERE t.question_id=profile_question.id AND t.language=langs.language));") == "t"
            assert query(f"SELECT discovery_paused FROM user_account WHERE user_id='{first}';") == "f"
            query(f"INSERT INTO user_subscription(user_id,plan,provider,provider_subscription_id,status) VALUES ('{first}','free','stripe','sub_LegacyStoreMigration','canceled') ON CONFLICT(user_id) DO UPDATE SET plan='free',provider='stripe',provider_subscription_id='sub_LegacyStoreMigration',status='canceled';")
            query(f"INSERT INTO user_presence(user_id,latitude,longitude,is_location_fresh) VALUES ('{first}',48.856613,2.352222,true);")
            # Recreate the retired analyser columns and decisions on this disposable database.
            photo_ids = [str(uuid.uuid4()) for _ in range(5)]
            query("ALTER TABLE content_moderation_case ADD COLUMN face_count smallint, ADD COLUMN sharpness_score double precision, ADD COLUMN nsfw_score double precision, ADD COLUMN face_detectable boolean, ADD COLUMN sharp_enough boolean, ADD COLUMN content_allowed boolean; ALTER TABLE content_moderation_case DROP CONSTRAINT chk_content_moderation_reason_codes;", "histae_migrate")
            query(f"INSERT INTO user_profile(user_id,firstname,birthdate) VALUES ('{first}','Photo migration',DATE '1990-01-01'),('{second}','Photo migration',DATE '1990-01-01');")
            for index, photo_id in enumerate(photo_ids):
                owner = second if index == 1 else first
                status = 'ready' if index < 2 else 'deleting'
                query(f"INSERT INTO user_photo(id,user_id,object_key,status,mime_type,size_bytes,width,height,sha256) VALUES ('{photo_id}','{owner}','profile-photos/{owner}/{photo_id}.webp','{status}','image/webp',12,2,2,decode(repeat('07',32),'hex'));")
                if index == 1:  # A legacy ready photo without moderation metadata.
                    continue
                status = {0: 'pending', 2: 'rejected', 3: 'approved', 4: 'pending'}[index]
                reviewed = "NULL" if index == 0 else "clock_timestamp()"
                query(f"INSERT INTO content_moderation_case(user_id,content_type,photo_id,status,reason_codes,policy_version,reviewed_at,review_reason,face_count,sharpness_score,nsfw_score) VALUES ('{owner}','photo','{photo_id}','{status}',ARRAY['blurry'],'local_vision_v1',{reviewed},'Human decision kept',1,40,0.1);")
            query("ALTER TABLE user_presence DROP COLUMN location_changed_at;", "histae_migrate")
            query("DROP VIEW effective_user_subscription; DROP FUNCTION effective_subscription_at(uuid,timestamptz); DROP TABLE billing_store_subscription,billing_store_notification,billing_store_finance,billing_store_account,web_billing_challenge,web_billing_session;", "histae_migrate")
            query("DROP TABLE continuation_reservation,advertising_reward_intent,continuation_wallet; ALTER TABLE continuation_usage DROP COLUMN free_used_count; ALTER TABLE user_account DROP COLUMN discovery_paused, DROP COLUMN preferred_language; ALTER TABLE device_token DROP COLUMN language; ALTER TABLE user_consent DROP COLUMN document_language; ALTER TABLE otp_verification DROP COLUMN language; DROP TABLE trait_translation, profile_question_translation, subscription_plan_translation, subscription_plan_feature_translation;", "histae_migrate")
            query("UPDATE profile_question SET prompt='Question personnalisée avant migration ?' WHERE id='51000000-0000-4000-8000-000000000001';")
            migrate()
            assert query("SELECT count(*) FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='content_moderation_case' AND column_name IN ('face_count','sharpness_score','nsfw_score','face_detectable','sharp_enough','content_allowed');") == "0"
            assert query(f"SELECT status||':'||policy_version||':'||cardinality(reason_codes) FROM content_moderation_case WHERE photo_id='{photo_ids[0]}';") == "approved:manual_photo_v1:0"
            assert query(f"SELECT status||':'||policy_version FROM content_moderation_case WHERE photo_id='{photo_ids[1]}';") == "approved:manual_photo_v1"
            for index, status in ((2, 'rejected'), (3, 'approved'), (4, 'pending')):
                assert query(f"SELECT status||':'||review_reason FROM content_moderation_case WHERE photo_id='{photo_ids[index]}';") == status + ":Human decision kept"
            photo_versions = query(f"SELECT string_agg(id::text||':'||version,',' ORDER BY id) FROM content_moderation_case WHERE user_id IN ('{first}','{second}');")
            assert query(f"SELECT latitude IS NULL AND longitude IS NULL AND NOT is_location_fresh FROM user_presence WHERE user_id='{first}';") == "t"
            assert query("SELECT count(*) FROM profile_question_translation WHERE question_id='51000000-0000-4000-8000-000000000001';") == "0"
            query("UPDATE profile_question SET prompt='À quoi ressemble ton dimanche idéal ?' WHERE id='51000000-0000-4000-8000-000000000001';")
            assert query("SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tablename IN ('billing_store_account','billing_store_subscription','billing_store_notification','billing_store_finance','web_billing_challenge','web_billing_session');") == "6"
            assert query(f"SELECT provider||':'||provider_subscription_id FROM effective_user_subscription WHERE user_id='{first}';") == "stripe:sub_LegacyStoreMigration"
            assert query(f"SELECT discovery_paused FROM user_account WHERE user_id='{first}';") == "f"
            assert query(f"SELECT used_count||':'||free_used_count FROM continuation_usage WHERE user_id='{first}';") == "2:2"
            assert query(f"SELECT count(*) FROM match_state WHERE match_id='{match_id}' AND continued;") == "0"
            assert query(f"SELECT continuation_initiator_id IS NULL FROM match_init WHERE id='{match_id}';") == "t"
            query(f"UPDATE match_state SET continued=true WHERE match_id='{match_id}' AND user_id='{first}'; INSERT INTO continuation_reservation(match_id,user_id,source,week_start) VALUES ('{match_id}','{first}','free',date_trunc('week',clock_timestamp() AT TIME ZONE 'UTC')::date);")
            assert query(f"SELECT preferred_language FROM user_account WHERE user_id='{first}';") == "fr"
            translation_id = query("SELECT trait_id FROM trait_translation WHERE language='en' ORDER BY trait_id LIMIT 1;")
            assert translation_id
            query(f"DELETE FROM trait_translation WHERE trait_id='{translation_id}' AND language='en'; UPDATE trait_translation SET name='Admin translation kept' WHERE trait_id='{translation_id}' AND language='it';")
            query(f"UPDATE user_account SET discovery_paused=true,preferred_language='es' WHERE user_id='{first}';")
            query(f"UPDATE user_presence SET latitude=89.975,longitude=0,is_location_fresh=true WHERE user_id='{first}';")
            migrate()
            assert query(f"SELECT string_agg(id::text||':'||version,',' ORDER BY id) FROM content_moderation_case WHERE user_id IN ('{first}','{second}');") == photo_versions
            print("Photo analyser retirement preserves human decisions and is idempotent.", flush=True)
            assert query(f"SELECT latitude=89.975 AND longitude=0 AND is_location_fresh FROM user_presence WHERE user_id='{first}';") == "t"
            assert query(f"SELECT discovery_paused FROM user_account WHERE user_id='{first}';") == "t"
            assert query(f"SELECT preferred_language FROM user_account WHERE user_id='{first}';") == "es"
            assert query(f"SELECT count(*) FROM trait_translation WHERE trait_id='{translation_id}' AND language='en';") == "0"
            assert query(f"SELECT name FROM trait_translation WHERE trait_id='{translation_id}' AND language='it';") == "Admin translation kept"
            assert query(f"SELECT count(*) FROM match_state WHERE match_id='{match_id}' AND continued;") == "1"
            assert query(f"SELECT count(*) FROM continuation_reservation WHERE match_id='{match_id}';") == "1"
            assert query(f"SELECT used_count||':'||free_used_count FROM continuation_usage WHERE user_id='{first}';") == "2:2"
            query(f"DELETE FROM user_account WHERE user_id IN ('{first}','{second}');")
            print("Legacy billing/quota/pause/language migrations and repeated upgrade passed.", flush=True)
            port = docker("port", name, "5432/tcp").split(":")[-1]
            runtime.update(POSTGRES_PORT=port, POSTGRES_USER="histae_app", POSTGRES_PASSWORD=app_password)
            environment = os.environ.copy()
            environment.update(runtime)
            environment.setdefault("CARGO_BUILD_JOBS", "2")
            selected = args.test or TESTS
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
