#!/usr/bin/env bash
# Creates and destroys only its own temporary PostgreSQL container and tmpfs.
set -euo pipefail

PROJECT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ROLE_TEST_IMAGE=${HISTAE_POSTGRES_IMAGE:-histae-postgres:18.6-postgis3.6.4}
ROLE_TEST_API_IMAGE=${HISTAE_API_IMAGE:-histae-api:local}
ROLE_TEST_CONTAINER="histae-postgres-roles-test-$$"
ROLE_TEST_DIRECTORY=$(mktemp -d)
chmod 700 "$ROLE_TEST_DIRECTORY"
ROLE_TEST_CREATED=false
cleanup() {
  if [[ $ROLE_TEST_CREATED == true ]]; then
    docker rm -f "$ROLE_TEST_CONTAINER" >/dev/null
  fi
  rm -rf -- "$ROLE_TEST_DIRECTORY"
}
trap cleanup EXIT

command -v openssl >/dev/null
docker image inspect "$ROLE_TEST_IMAGE" >/dev/null
docker image inspect "$ROLE_TEST_API_IMAGE" >/dev/null
umask 077
{
  printf 'POSTGRES_DB=histae-roles-test\nPOSTGRES_USER=histae_admin\n'
  printf 'POSTGRES_INITDB_ARGS=--auth-host=scram-sha-256\n'
  printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 32)"
  printf 'HISTAE_BOOTSTRAP_MIGRATE_PASSWORD=%s\n' "$(openssl rand -hex 32)"
  printf 'HISTAE_BOOTSTRAP_APP_PASSWORD=%s\n' "$(openssl rand -hex 32)"
} > "$ROLE_TEST_DIRECTORY/postgres.env"

docker run -d --name "$ROLE_TEST_CONTAINER" --network none \
  --env-file "$ROLE_TEST_DIRECTORY/postgres.env" \
  --tmpfs /var/lib/postgresql:rw,size=512m \
  --mount "type=bind,src=$PROJECT_DIR/db,dst=/etc/histae/db,readonly" \
  --mount "type=bind,src=$PROJECT_DIR/docker/postgres/init-roles.sh,dst=/docker-entrypoint-initdb.d/001_roles.sh,readonly" \
  "$ROLE_TEST_IMAGE" postgres -c shared_preload_libraries=pg_stat_statements >/dev/null
ROLE_TEST_CREATED=true

ready=false
for ((attempt = 0; attempt < 90; attempt++)); do
  if docker exec "$ROLE_TEST_CONTAINER" pg_isready -h 127.0.0.1 -U histae_admin -d histae-roles-test >/dev/null 2>&1; then
    ready=true
    break
  fi
  if [[ $(docker inspect --format '{{.State.Running}}' "$ROLE_TEST_CONTAINER") != true ]]; then
    printf 'PostgreSQL role bootstrap failed.\n' >&2
    exit 1
  fi
  sleep 1
done
[[ $ready == true ]] || { printf 'PostgreSQL role bootstrap timed out.\n' >&2; exit 1; }

as_role() {
  docker exec -i "$ROLE_TEST_CONTAINER" bash -ec '
    case "$1" in
      histae_admin) export PGPASSWORD="$POSTGRES_PASSWORD" ;;
      histae_migrate) export PGPASSWORD="$HISTAE_BOOTSTRAP_MIGRATE_PASSWORD" ;;
      histae_app) export PGPASSWORD="$HISTAE_BOOTSTRAP_APP_PASSWORD" ;;
      *) exit 2 ;;
    esac
    exec psql -X -q --set ON_ERROR_STOP=1 -h 127.0.0.1 -U "$1" -d histae-roles-test
  ' bash "$1"
}

# Authentication really uses TCP/SCRAM; no SET ROLE from a superuser session.
{
  printf 'ENV=test\nPOSTGRES_HOST=127.0.0.1\nPOSTGRES_PORT=5432\nPOSTGRES_DB=histae-roles-test\n'
  printf 'POSTGRES_USER=histae_migrate\nPOSTGRES_SSLMODE=disable\nPOSTGRES_POOL_MAX=1\n'
  printf 'POSTGRES_PASSWORD=%s\n' "$(sed -n 's/^HISTAE_BOOTSTRAP_MIGRATE_PASSWORD=//p' "$ROLE_TEST_DIRECTORY/postgres.env")"
  printf 'JWT_SECRET=%s\n' "$(openssl rand -hex 32)"
  printf 'PHONE_ENCRYPTION_KEY=%s\n' "$(openssl rand -hex 32)"
  printf 'PHONE_HASH_KEY=%s\n' "$(openssl rand -hex 32)"
  printf 'RATE_LIMIT_STORE=memory\nSMS_PROVIDER=disabled\nBILLING_PROVIDER=disabled\n'
  printf 'PUSH_PROVIDER=disabled\nPHOTO_MODERATION_PROVIDER=disabled\nMETRICS_ENABLED=false\n'
} > "$ROLE_TEST_DIRECTORY/migrate.env"
migrate() {
  local phase=$1
  if ! docker run --rm --network "container:$ROLE_TEST_CONTAINER" \
    --env-file "$ROLE_TEST_DIRECTORY/migrate.env" --user 1000:1000 \
    --read-only --cap-drop ALL --security-opt no-new-privileges:true \
    --entrypoint /app/bin/db-migrate "$ROLE_TEST_API_IMAGE" \
    > "$ROLE_TEST_DIRECTORY/migrate.log" 2>&1; then
    printf 'Limited PostgreSQL migrator failed during %s.\n' "$phase" >&2
    grep -oE 'error_code=[a-z0-9_]+' "$ROLE_TEST_DIRECTORY/migrate.log" >&2 || true
    exit 1
  fi
}
migrate initial
migrate upgrade
as_role histae_migrate >/dev/null <<'SQL'
CREATE TABLE public.role_permission_probe (id integer PRIMARY KEY, value text);
CREATE SEQUENCE public.role_sequence_probe;
CREATE FUNCTION public.role_function_probe() RETURNS integer LANGUAGE sql AS 'SELECT 1';
SQL
as_role histae_app < "$PROJECT_DIR/tests/postgres_roles.sql" >/dev/null
as_role histae_migrate >/dev/null <<'SQL'
DROP TABLE public.role_permission_probe;
DROP SEQUENCE public.role_sequence_probe;
DROP FUNCTION public.role_function_probe();
SQL
as_role histae_admin < "$PROJECT_DIR/db/drop_postgres.sql" >/dev/null 2>&1
{ printf '\\set bootstrap_mode access\n'; cat "$PROJECT_DIR/db/000_bootstrap_postgres.sql"; } | as_role histae_admin >/dev/null
migrate reset
as_role histae_migrate >/dev/null <<'SQL'
CREATE TABLE public.role_permission_probe (id integer PRIMARY KEY, value text);
CREATE SEQUENCE public.role_sequence_probe;
CREATE FUNCTION public.role_function_probe() RETURNS integer LANGUAGE sql AS 'SELECT 1';
SQL
as_role histae_app < "$PROJECT_DIR/tests/postgres_roles.sql" >/dev/null
printf 'PostgreSQL role separation verification passed (fresh installation, upgrade and schema reset).\n'
