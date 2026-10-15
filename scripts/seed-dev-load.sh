#!/usr/bin/env bash
set -euo pipefail

seed_count=${1:-5000}
if (( $# > 1 )) || [[ ! $seed_count =~ ^[1-9][0-9]{3,4}$ ]] || (( seed_count < 5000 || seed_count > 10000 || seed_count % 100 != 0 )); then
  echo 'Usage: bash scripts/seed-dev-load.sh [5000..10000, multiple of 100]' >&2
  exit 2
fi

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if [[ ! -f .env ]] || ! docker info >/dev/null 2>&1; then
  echo 'The development .env and Docker access are required. Run with sudo if your user cannot access Docker.' >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo 'Python 3 is required to exercise the development API routes.' >&2
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
if [[ $project != histae-rust-dev || $service != postgres ]]; then
  echo 'Refusing to seed a container outside the Histae Rust development project.' >&2
  exit 2
fi

database_name=$("${compose[@]}" exec -T postgres printenv POSTGRES_DB)
database_user=$("${compose[@]}" exec -T postgres printenv POSTGRES_USER)
if [[ $database_name != histae-dev || -z $database_user ]]; then
  echo 'Refusing to seed a database other than histae-dev.' >&2
  exit 2
fi

api_environment=$("${compose[@]}" exec -T api printenv ENV)
if [[ $api_environment != development ]]; then
  echo 'Refusing to seed without a running development API container.' >&2
  exit 2
fi

# The HTTP phase makes over 100,000 requests from one IP. Fail before inserting
# accounts when the normal global limit would turn this into a many-hour run.
global_limit=$("${compose[@]}" exec -T api printenv RATE_LIMIT_GLOBAL || true)
global_window=$("${compose[@]}" exec -T api printenv RATE_LIMIT_GLOBAL_WINDOW || true)
if [[ ! $global_limit =~ ^[0-9]+$ ]] || (( global_limit < 10000 )) || [[ $global_window != 1m ]]; then
  echo 'For this development seed, set RATE_LIMIT_GLOBAL=20000 and RATE_LIMIT_GLOBAL_WINDOW=1m in .env, then recreate the API container.' >&2
  echo 'After the seed, restore your usual rate limit and recreate the API container again.' >&2
  exit 2
fi

# Keep the development fallback in sync with src/config/validation.rs::legal_config.
for name in TERMS_OF_SERVICE_VERSION PRIVACY_POLICY_VERSION SENSITIVE_DATA_CONSENT_VERSION LOCATION_CONSENT_VERSION; do
  value=$("${compose[@]}" exec -T api printenv "$name" || true)
  value=${value:-development-unversioned}
  case $name in
    TERMS_OF_SERVICE_VERSION) terms_version=$value ;;
    PRIVACY_POLICY_VERSION) privacy_version=$value ;;
    SENSITIVE_DATA_CONSENT_VERSION) sensitive_version=$value ;;
    LOCATION_CONSENT_VERSION) location_version=$value ;;
  esac
done

"${compose[@]}" exec -T postgres psql -X -v ON_ERROR_STOP=1 \
  -v seed_count="$seed_count" \
  -v terms_version="$terms_version" \
  -v privacy_version="$privacy_version" \
  -v sensitive_version="$sensitive_version" \
  -v location_version="$location_version" \
  -U "$database_user" -d "$database_name" < scripts/seed-dev-load.sql

python3 scripts/seed-dev-load-api.py "$seed_count"
