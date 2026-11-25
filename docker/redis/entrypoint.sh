#!/bin/sh
set -eu

: "${REDIS_PASSWORD:?Redis requires a password}"
umask 077

acl_file=/run/redis/users.acl
acl_rules=$(cat /etc/redis/histae-users.acl)
password_digest=$(printf '%s' "$REDIS_PASSWORD" | sha256sum)
printf '%s #%s\n' "$acl_rules" "${password_digest%% *}" > "$acl_file"
chown redis:redis "$acl_file"

unset REDIS_PASSWORD REDISCLI_AUTH
exec /usr/local/bin/docker-entrypoint.sh redis-server /etc/redis/histae.conf --aclfile "$acl_file"
