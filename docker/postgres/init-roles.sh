#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'ERROR postgres_roles_initialization_failed\n' >&2
  exit 1
}

[[ ${POSTGRES_USER:-} == histae_admin && -n ${POSTGRES_DB:-} ]] || fail
[[ $POSTGRES_DB != postgres && $POSTGRES_DB != template0 && $POSTGRES_DB != template1 ]] || fail

: "${HISTAE_BOOTSTRAP_MIGRATE_PASSWORD:?Missing migration role password}"
: "${HISTAE_BOOTSTRAP_APP_PASSWORD:?Missing application role password}"
# Older development clusters can bootstrap with job roles left as NOLOGIN.
export HISTAE_BOOTSTRAP_PHONE_SYNC_PASSWORD="${HISTAE_BOOTSTRAP_PHONE_SYNC_PASSWORD:-}"
export HISTAE_BOOTSTRAP_OUTBOX_PASSWORD="${HISTAE_BOOTSTRAP_OUTBOX_PASSWORD:-}"
export HISTAE_BOOTSTRAP_MAINTENANCE_PASSWORD="${HISTAE_BOOTSTRAP_MAINTENANCE_PASSWORD:-}"
[[ ${POSTGRES_PASSWORD:-} =~ ^[a-f0-9]{64}$ ]] || fail
seen_passwords=("$POSTGRES_PASSWORD")
for password in "$HISTAE_BOOTSTRAP_MIGRATE_PASSWORD" "$HISTAE_BOOTSTRAP_APP_PASSWORD" \
    "$HISTAE_BOOTSTRAP_PHONE_SYNC_PASSWORD" "$HISTAE_BOOTSTRAP_OUTBOX_PASSWORD" \
    "$HISTAE_BOOTSTRAP_MAINTENANCE_PASSWORD"; do
  [[ -n $password ]] || continue
  [[ $password =~ ^[a-f0-9]{64}$ ]] || fail
  for seen in "${seen_passwords[@]}"; do
    [[ $password != "$seen" ]] || fail
  done
  seen_passwords+=("$password")
done
unset seen_passwords seen password
export HISTAE_BOOTSTRAP_MIGRATE_PASSWORD HISTAE_BOOTSTRAP_APP_PASSWORD

# psql reads passwords from its environment, never command-line arguments.
# Suppress SQL diagnostics because an interpolated CREATE ROLE could expose a
# password on failure. The transaction rolls back the whole role bootstrap.
if ! psql -X --set ON_ERROR_STOP=1 --username "$POSTGRES_USER" \
    --dbname "$POSTGRES_DB" >/dev/null 2>&1 <<'SQL'
SET log_statement = 'none';
SET log_min_error_statement = 'panic';
\getenv migrate_password HISTAE_BOOTSTRAP_MIGRATE_PASSWORD
\getenv app_password HISTAE_BOOTSTRAP_APP_PASSWORD
\getenv phone_sync_password HISTAE_BOOTSTRAP_PHONE_SYNC_PASSWORD
\getenv outbox_password HISTAE_BOOTSTRAP_OUTBOX_PASSWORD
\getenv maintenance_password HISTAE_BOOTSTRAP_MAINTENANCE_PASSWORD
\i /etc/histae/db/000_bootstrap_postgres.sql
SQL
then
  fail
fi
unset HISTAE_BOOTSTRAP_MIGRATE_PASSWORD HISTAE_BOOTSTRAP_APP_PASSWORD \
  HISTAE_BOOTSTRAP_PHONE_SYNC_PASSWORD HISTAE_BOOTSTRAP_OUTBOX_PASSWORD HISTAE_BOOTSTRAP_MAINTENANCE_PASSWORD
printf 'PostgreSQL roles initialized.\n'
