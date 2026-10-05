#!/usr/bin/env bash
set -euo pipefail

base_url=${1:-http://127.0.0.1:8080}
if [[ ! $base_url =~ ^https?://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]+)?$ ]]; then
  echo 'The smoke test only accepts a loopback HTTP(S) base URL.' >&2
  exit 2
fi

request() {
  local method=$1 path=$2 expected_status=$3 expected_code=$4 body=${5-}
  local response status payload
  if [[ $method == POST ]]; then
    response=$(curl --silent --show-error --max-time 10 --request POST \
      --header 'Content-Type: application/json' --data "$body" \
      --write-out '\n%{http_code}' "$base_url$path")
  else
    response=$(curl --silent --show-error --max-time 10 \
      --write-out '\n%{http_code}' "$base_url$path")
  fi
  status=${response##*$'\n'}
  payload=${response%$'\n'*}
  if [[ $status != "$expected_status" ]]; then
    echo "Unexpected HTTP status for $method $path: $status (expected $expected_status)" >&2
    exit 1
  fi
  if [[ $expected_code == status:* ]]; then
    if ! jq -e --arg expected "${expected_code#status:}" '.status == $expected' <<<"$payload" >/dev/null; then
      echo "Unexpected status field for $method $path" >&2
      exit 1
    fi
  elif ! jq -e --arg expected "$expected_code" '.error.code == $expected' <<<"$payload" >/dev/null; then
    echo "Unexpected error code for $method $path" >&2
    exit 1
  fi
}

request GET /health/live 200 status:ok
request GET /health/ready 200 status:ready
request GET /api/users/me 401 authentication_required
admin_id=$(cat /proc/sys/kernel/random/uuid)
request GET "/api/admin/users/$admin_id" 401 admin_session_invalid
request POST /api/auth/otp/send 400 invalid_request_body '{}'
request GET /api/s29-route-that-does-not-exist 404 route_not_found

echo 'API smoke passed.'
