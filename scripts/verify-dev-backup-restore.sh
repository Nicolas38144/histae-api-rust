#!/usr/bin/env bash
set -euo pipefail

container=${1:-histae-rust-dev-postgres-1}
source_database=${2:-histae-dev}
database_user=${3:-}

if [[ $source_database != histae-dev || ! $container =~ ^histae-rust-dev-postgres-[0-9]+$ ]]; then
  echo 'Backup/restore verification is restricted to the Rust local development database and container.' >&2
  exit 2
fi
if docker info >/dev/null 2>&1; then
  docker_cmd=(docker)
elif command -v sudo >/dev/null 2>&1 && sudo docker info >/dev/null 2>&1; then
  docker_cmd=(sudo docker)
else
  echo 'Docker is unavailable; check the daemon and your sudo access.' >&2
  exit 1
fi

container_name=$("${docker_cmd[@]}" inspect --format '{{.Name}}' "$container")
compose_project=$("${docker_cmd[@]}" inspect --format '{{index .Config.Labels "com.docker.compose.project"}}' "$container")
compose_service=$("${docker_cmd[@]}" inspect --format '{{index .Config.Labels "com.docker.compose.service"}}' "$container")
if [[ $container_name != "/$container" || $compose_project != histae-rust-dev || $compose_service != postgres ]]; then
  echo 'The container is not the Rust development PostgreSQL service.' >&2
  exit 2
fi

database_name=$("${docker_cmd[@]}" exec "$container" printenv POSTGRES_DB)
if [[ $database_name != histae-dev ]]; then
  echo 'The container does not declare the local development database.' >&2
  exit 2
fi
configured_user=$("${docker_cmd[@]}" exec "$container" printenv POSTGRES_USER)
database_user=${database_user:-$configured_user}
if [[ ! $database_user =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ || $database_user != "$configured_user" ]]; then
  echo 'The PostgreSQL role must match the Rust development container configuration.' >&2
  exit 2
fi

suffix=$(cat /proc/sys/kernel/random/uuid)
suffix=${suffix//-/}
restore_database="histae_s29_restore_$suffix"
archive="/tmp/histae_s29_$suffix.dump"
created=0

cleanup() {
  local result=$?
  trap - EXIT
  if (( created )); then
    if ! "${docker_cmd[@]}" exec "$container" dropdb -U "$database_user" --if-exists "$restore_database" >/dev/null; then
      echo 'Could not remove the temporary restore database.' >&2
      result=1
    fi
  fi
  if ! "${docker_cmd[@]}" exec "$container" rm -f -- "$archive" >/dev/null; then
    echo 'Could not remove the temporary backup archive.' >&2
    result=1
  fi
  exit "$result"
}
trap cleanup EXIT

"${docker_cmd[@]}" exec "$container" pg_dump -U "$database_user" -d "$source_database" -Fc -f "$archive"
created=1
"${docker_cmd[@]}" exec "$container" createdb -U "$database_user" "$restore_database"
"${docker_cmd[@]}" exec "$container" pg_restore -U "$database_user" -d "$restore_database" --exit-on-error "$archive"

schema_query="SELECT md5(string_agg(table_name || ':' || column_name || ':' || data_type || ':' || is_nullable, '|' ORDER BY table_name, ordinal_position)) FROM information_schema.columns WHERE table_schema = 'public'"
source_schema=$("${docker_cmd[@]}" exec "$container" psql -U "$database_user" -d "$source_database" -Atc "$schema_query")
restored_schema=$("${docker_cmd[@]}" exec "$container" psql -U "$database_user" -d "$restore_database" -Atc "$schema_query")
if [[ -z $source_schema || $source_schema != "$restored_schema" ]]; then
  echo 'The restored public schema differs from the source.' >&2
  exit 1
fi

table_query="SELECT count(*) FROM pg_tables WHERE schemaname = 'public'"
source_tables=$("${docker_cmd[@]}" exec "$container" psql -U "$database_user" -d "$source_database" -Atc "$table_query")
restored_tables=$("${docker_cmd[@]}" exec "$container" psql -U "$database_user" -d "$restore_database" -Atc "$table_query")
if [[ $source_tables != "$restored_tables" ]]; then
  echo "Restored table count differs: source=$source_tables restored=$restored_tables" >&2
  exit 1
fi

echo "Development backup/restore verification passed ($source_tables public tables)."
