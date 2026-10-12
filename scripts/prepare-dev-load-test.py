#!/usr/bin/env python3
"""Create/revoke bounded sessions for the existing synthetic development accounts."""

import argparse
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('load_seed', ROOT / 'scripts/seed-dev-load-api.py')
seed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(seed)
API_SERVICE = 'api'
MINIMUM_ACTIVE_ACCOUNTS = 1000


def guard():
    for service in (API_SERVICE, 'postgres'):
        container = seed.docker('ps', '-q', service)
        if not container or '\n' in container:
            raise RuntimeError(f'Expected one running development {service} container')
        label = subprocess.run(
            ['docker', 'inspect', '--format',
             '{{index .Config.Labels "com.docker.compose.project"}}', container],
            check=True, capture_output=True, text=True).stdout.strip()
        if label != 'histae-rust-dev':
            raise RuntimeError('Refusing a container outside histae-rust-dev')
    if seed.docker('exec', '-T', API_SERVICE, 'printenv', 'ENV') != 'development':
        raise RuntimeError('Refusing a non-development API')
    if seed.docker('exec', '-T', 'postgres', 'printenv', 'POSTGRES_DB') != 'histae-dev':
        raise RuntimeError('Refusing a database other than histae-dev')
    return seed.docker('exec', '-T', 'postgres', 'printenv', 'POSTGRES_USER')


def optional_env(name, fallback):
    try:
        return seed.docker('exec', '-T', API_SERVICE, 'printenv', name) or fallback
    except subprocess.CalledProcessError:
        return fallback


def access_ttl():
    value = optional_env('JWT_ACCESS_TTL', '15m')
    match = re.fullmatch(r'([0-9]+)(ms|s|m|h)', value)
    if not match:
        raise RuntimeError('Invalid JWT_ACCESS_TTL')
    seconds = int(match[1]) * {'ms': 0.001, 's': 1, 'm': 60, 'h': 3600}[match[2]]
    if not 60 <= seconds <= 3600:
        raise RuntimeError('JWT_ACCESS_TTL must be between 1m and 60m')
    return int(seconds)


def token(actor, session, secret, key_id, issued, expires):
    header = seed.encoded(json.dumps(
        {'typ': 'JWT', 'alg': 'HS256', 'kid': key_id}, separators=(',', ':')).encode())
    claims = seed.encoded(json.dumps({
        'sub': str(actor), 'sid': str(session), 'typ': 'access', 'iat': issued,
        'exp': expires, 'aud': 'histae-app', 'iss': 'histae-api',
    }, separators=(',', ':')).encode())
    signed = f'{header}.{claims}'
    signature = seed.encoded(hmac.new(secret, signed.encode(), hashlib.sha256).digest())
    return f'{signed}.{signature}'


def remove_sessions(database_user, accounts):
    synthetic = {seed.user_id(number) for number in range(1, 10001)}
    pairs = []
    for account in accounts:
        actor, session = uuid.UUID(account['user_id']), uuid.UUID(account['session_id'])
        if actor not in synthetic or session.version != 4 or session.variant != uuid.RFC_4122:
            raise RuntimeError('Refusing a non-synthetic account or invalid session in the fixture')
        pairs.append((actor, session))
    # Scope deletion to both the synthetic owner and this run's session ID.
    for start in range(0, len(pairs), 500):
        values = ','.join(f"('{actor}'::uuid, '{session}'::uuid)"
                          for actor, session in pairs[start:start + 500])
        seed.query(f"""
          DELETE FROM refresh_token_family AS family
          USING (VALUES {values}) AS selected(user_id, session_id)
          WHERE family.user_id = selected.user_id AND family.id = selected.session_id
        """, database_user)


def create_sessions(database_user, count, expires):
    rows = seed.query(f"""
      WITH synthetic AS (
        SELECT uuid_generate_v5('{seed.NAMESPACE}'::uuid,
                 'histae-load-user-' || number) AS id
        FROM generate_series(1, {count}) AS number
      ), inserted AS (
        INSERT INTO refresh_token_family
          (id, user_id, created_at, last_refreshed_at, expires_at)
        SELECT gen_random_uuid(), account.user_id, now(), now(), to_timestamp({expires})
        FROM synthetic JOIN user_account AS account ON account.user_id = synthetic.id
        WHERE account.role = 'user' AND account.deleted_at IS NULL AND NOT account.is_banned
        RETURNING user_id, id
      )
      SELECT user_id, id FROM inserted
    """, database_user)
    sessions = {uuid.UUID(actor): uuid.UUID(session)
                for actor, session in (row.split('\t') for row in rows.splitlines())}
    if len(sessions) < MINIMUM_ACTIVE_ACCOUNTS:
        remove_sessions(database_user, [{'user_id': str(actor), 'session_id': str(session)}
                                         for actor, session in sessions.items()])
        raise RuntimeError(f'At least {MINIMUM_ACTIVE_ACCOUNTS} active synthetic accounts are required; found {len(sessions)}')
    if len(sessions) != count:
        print(f'Skipped {count - len(sessions)} inactive or non-user synthetic accounts.')
    return sessions


def prepare(database_user, count, output):
    if output.exists():
        raise RuntimeError('Fixture already exists: run --cleanup before preparing a new one')
    for name in ('SMS_PROVIDER', 'BILLING_PROVIDER', 'PUSH_PROVIDER'):
        if optional_env(name, 'disabled') != 'disabled':
            raise RuntimeError(f'{name} must be disabled on the load-test instance')
    secret = seed.docker('exec', '-T', API_SERVICE, 'printenv', 'JWT_SECRET').encode()
    if len(secret) < 32:
        raise RuntimeError('Missing or invalid development JWT_SECRET')
    key_id = optional_env('JWT_ACTIVE_KID', 'primary')
    issued = int(time.time())
    expires = issued + access_ttl()
    sessions = create_sessions(database_user, count, expires)
    accounts = [{'user_id': str(actor), 'session_id': str(session)}
                for actor, session in sessions.items()]
    created_file = False
    try:
        # Every fixture account needs an actual readable conversation. Reports,
        # expired matches and closed matches must not turn the benchmark into 404s.
        rows = seed.query(f"""
          WITH selected AS (
            SELECT uuid_generate_v5('{seed.NAMESPACE}'::uuid,
                     'histae-load-user-' || number) AS id
            FROM generate_series(1, {count}) AS number
          )
          SELECT selected.id, conversation.id
          FROM selected
          JOIN LATERAL (
            SELECT match.id FROM match_init AS match
            WHERE (match.user1_id = selected.id OR match.user2_id = selected.id)
              AND match.status = 'confirmed'
              AND EXISTS (SELECT 1 FROM chat_message WHERE match_id = match.id)
            ORDER BY match.id LIMIT 1
          ) AS conversation ON true
        """, database_user)
        conversations = dict(row.split('\t') for row in rows.splitlines())
        if any(str(actor) not in conversations for actor in sessions):
            raise RuntimeError('Run seed-dev-load.sh first: every active account needs a confirmed match with messages')
        seed.query(f"""
          UPDATE user_presence AS presence
          SET updated_at = clock_timestamp(), is_location_fresh = true
          FROM user_account AS account
          WHERE presence.user_id = account.user_id AND account.role = 'user'
            AND NOT account.is_banned AND account.deleted_at IS NULL
            AND presence.user_id IN (
              SELECT uuid_generate_v5('{seed.NAMESPACE}'::uuid,
                       'histae-load-user-' || number)
              FROM generate_series(1, {count}) AS number
            )
        """, database_user)
        for account in accounts:
            account['match_id'] = conversations[account['user_id']]
            account['expires_at'] = expires
            account['access_token'] = token(account['user_id'], account['session_id'],
                                            secret, key_id, issued, expires)
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        created_file = True
        with os.fdopen(descriptor, 'w') as target:
            json.dump({'schema_version': 1, 'environment': 'development',
                       'issued_at': issued, 'expires_at': expires, 'accounts': accounts}, target)
        print(f'Prepared {len(accounts)} synthetic sessions; lifetime {expires - issued}s.')
        print(f'Private fixture: {output}. The JWT signing key is not exported.')
    except BaseException:
        remove_sessions(database_user, accounts)
        if created_file:
            output.unlink(missing_ok=True)
        raise


def main():
    global API_SERVICE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('count', nargs='?', type=int, default=5000)
    parser.add_argument('--fixture', type=Path, default=Path('.load-test/accounts.json'))
    parser.add_argument('--cleanup', action='store_true')
    parser.add_argument('--api-service', choices=('api', 'load-api'), default='api')
    parser.add_argument('--compose-file', type=Path, action='append', default=[],
                        help='Additional Compose override, repeatable')
    args = parser.parse_args()
    if not 5000 <= args.count <= 10000 or args.count % 100:
        parser.error('COUNT must be 5000..10000 and divisible by 100')
    os.chdir(ROOT)
    API_SERVICE = args.api_service
    for compose_file in args.compose_file:
        seed.COMPOSE.extend(['-f', str(compose_file)])
    database_user = guard()
    if args.cleanup:
        with args.fixture.open() as source:
            fixture = json.load(source)
        if fixture.get('environment') != 'development' or fixture.get('schema_version') != 1:
            raise RuntimeError('Invalid development fixture')
        remove_sessions(database_user, fixture['accounts'])
        args.fixture.unlink()
        print('Load-test sessions removed and local fixture deleted. Accounts and messages kept.')
    else:
        prepare(database_user, args.count, args.fixture)


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        # Never print captured Docker output: it can contain a signing secret.
        print(f'Load-test preparation failed: {error}', file=sys.stderr)
        sys.exit(1)
