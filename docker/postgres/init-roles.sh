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
[[ $HISTAE_BOOTSTRAP_MIGRATE_PASSWORD =~ ^[a-f0-9]{64}$ ]] || fail
[[ $HISTAE_BOOTSTRAP_APP_PASSWORD =~ ^[a-f0-9]{64}$ ]] || fail
[[ ${POSTGRES_PASSWORD:-} =~ ^[a-f0-9]{64}$ ]] || fail
[[ $HISTAE_BOOTSTRAP_MIGRATE_PASSWORD != "$HISTAE_BOOTSTRAP_APP_PASSWORD" ]] || fail
[[ $HISTAE_BOOTSTRAP_MIGRATE_PASSWORD != "${POSTGRES_PASSWORD:-}" ]] || fail
[[ $HISTAE_BOOTSTRAP_APP_PASSWORD != "${POSTGRES_PASSWORD:-}" ]] || fail
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
\i /etc/histae/db/000_bootstrap_postgres.sql
SQL
then
  unset HISTAE_BOOTSTRAP_MIGRATE_PASSWORD HISTAE_BOOTSTRAP_APP_PASSWORD
  fail
fi
unset HISTAE_BOOTSTRAP_MIGRATE_PASSWORD HISTAE_BOOTSTRAP_APP_PASSWORD
printf 'PostgreSQL roles initialized.\n'
