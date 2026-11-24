#!/usr/bin/env bash
set -euo pipefail
umask 077

directory=${1:-.secrets/store-verifier}
if [[ -e $directory || -L $directory ]]; then
  printf 'TLS directory already exists: %s\n' "$directory" >&2
  exit 1
fi
mkdir -p -- "$(dirname -- "$directory")"
work=$(mktemp -d -- "$(dirname -- "$directory")/.store-verifier-tls.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

openssl req -x509 -newkey rsa:3072 -nodes -days 1825 \
  -keyout "$work/ca.key" -out "$work/ca.crt" -subj '/CN=Histae Store Verifier CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign'
openssl req -new -newkey rsa:3072 -nodes -keyout "$work/server.key" \
  -out "$work/server.csr" -subj '/CN=store-verifier'
cat > "$work/server.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:store-verifier
EOF
openssl x509 -req -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 365 -sha256 -extfile "$work/server.ext" -out "$work/server.crt"
openssl verify -purpose sslserver -verify_hostname store-verifier -CAfile "$work/ca.crt" "$work/server.crt"
rm -- "$work/server.csr" "$work/server.ext" "$work/ca.srl"
chmod 644 -- "$work/ca.crt" "$work/server.crt"
mv -Tn -- "$work" "$directory"
[[ ! -e $work ]]
printf 'Store verifier TLS material installed in %s\n' "$directory"
