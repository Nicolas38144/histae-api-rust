#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if [[ ! -f .env ]] || ! docker info >/dev/null 2>&1; then
  echo 'The development .env and Docker access are required. Run with sudo if needed.' >&2
  exit 1
fi

compose=(docker compose --env-file .env -f compose.yaml -f compose.dev.yaml)
container_id=$("${compose[@]}" ps -q postgres)
if [[ -z $container_id ]]; then
  echo 'The development PostgreSQL container is not running.' >&2
  exit 1
fi

project=$(docker inspect --format '{{index .Config.Labels "com.docker.compose.project"}}' "$container_id")
service=$(docker inspect --format '{{index .Config.Labels "com.docker.compose.service"}}' "$container_id")
database_name=$("${compose[@]}" exec -T postgres printenv POSTGRES_DB)
database_user=$("${compose[@]}" exec -T postgres printenv POSTGRES_USER)
if [[ $project != histae-rust-dev || $service != postgres || $database_name != histae-dev || -z $database_user ]]; then
  echo 'Refusing to purge anything outside the Histae Rust development database.' >&2
  exit 2
fi

"${compose[@]}" exec -T postgres psql -X -v ON_ERROR_STOP=1 \
  -U "$database_user" -d "$database_name" < scripts/purge-dev-users.sql
