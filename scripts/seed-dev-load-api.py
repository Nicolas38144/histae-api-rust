#!/usr/bin/env python3
"""Exercise development HTTP routes with short-lived synthetic mobile sessions."""

import base64
import hashlib
import hmac
import http.client
import json
import subprocess
import sys
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed


NAMESPACE = uuid.UUID('2fc9a04d-bca3-4fa7-b6a1-a27736103699')
COMPOSE = ['docker', 'compose', '--env-file', '.env', '-f', 'compose.yaml', '-f', 'compose.dev.yaml']
THREAD = threading.local()
WORKERS = 12


def docker(*args):
    return subprocess.run(COMPOSE + list(args), check=True, capture_output=True, text=True).stdout.rstrip('\r\n')


def query(sql, database_user):
    return docker('exec', '-T', 'postgres', 'psql', '-X', '-v', 'ON_ERROR_STOP=1',
                  '-At', '-F', '\t', '-U', database_user, '-d', 'histae-dev', '-c', sql)


def user_id(number):
    return uuid.uuid5(NAMESPACE, f'histae-load-user-{number}')


def partner(number):
    return ((number - 1) // 100) * 100 + (number % 100) + 1


def pass_target(number):
    return ((number - 1) // 100) * 100 + ((number + 16) % 100) + 1


def encoded(value):
    return base64.urlsafe_b64encode(value).rstrip(b'=').decode('ascii')


class Client:
    def __init__(self, port, secret, key_id, sessions):
        self.port = port
        self.secret = secret
        self.key_id = key_id
        self.sessions = sessions

    def token(self, actor):
        now = int(time.time())
        header = {'typ': 'JWT', 'alg': 'HS256', 'kid': self.key_id}
        claims = {'sub': str(actor), 'sid': str(self.sessions[actor]), 'typ': 'access',
                  'iat': now, 'exp': now + 600, 'aud': 'histae-app', 'iss': 'histae-api'}
        head = encoded(json.dumps(header, separators=(',', ':')).encode())
        body = encoded(json.dumps(claims, separators=(',', ':')).encode())
        signed = f'{head}.{body}'
        signature = encoded(hmac.new(self.secret, signed.encode(), hashlib.sha256).digest())
        return f'{signed}.{signature}'

    def request(self, method, path, actor, payload, expected, idempotency_key=None):
        body = json.dumps(payload, separators=(',', ':')).encode() if payload is not None else None
        headers = {'Authorization': f'Bearer {self.token(actor)}'}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        if idempotency_key is not None:
            headers['Idempotency-Key'] = str(idempotency_key)
        for attempt in range(4):
            connection = getattr(THREAD, 'connection', None)
            if connection is None:
                connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=30)
                THREAD.connection = connection
            try:
                connection.request(method, path, body=body, headers=headers)
                response = connection.getresponse()
                data = response.read()
                if response.status in (429, 502, 503, 504) and attempt < 3:
                    time.sleep(min(2 ** attempt, 8))
                    continue
                result = json.loads(data) if data else {}
                if response.status not in expected:
                    raise RuntimeError(f'{method} {path}: HTTP {response.status}: {result}')
                return result
            except (OSError, http.client.HTTPException):
                connection.close()
                THREAD.connection = None
                if attempt == 3:
                    raise
                time.sleep(2 ** attempt)
        raise RuntimeError(f'{method} {path}: retries exhausted')


def message_key(number, index):
    return uuid.UUID(bytes=uuid.uuid5(NAMESPACE, f'api-message-{number}-{index}').bytes, version=4)


def existing_matches(database_user, count):
    rows = query(f"""
      WITH seed AS (
        SELECT uuid_generate_v5('{NAMESPACE}'::uuid, 'histae-load-user-' || number) AS id
        FROM generate_series(1, {count}) AS number
      )
      SELECT match.user1_id, match.user2_id, match.id
      FROM match_init AS match JOIN seed AS first ON first.id = match.user1_id
                               JOIN seed AS second ON second.id = match.user2_id
    """, database_user)
    found = {}
    for row in rows.splitlines():
        first, second, match = row.split('\t')
        found[frozenset((uuid.UUID(first), uuid.UUID(second)))] = uuid.UUID(match)
    return found


def refresh_presence(database_user, count):
    query(f"""
      UPDATE user_presence SET updated_at = clock_timestamp(), is_location_fresh = true
      WHERE user_id IN (
        SELECT uuid_generate_v5('{NAMESPACE}'::uuid, 'histae-load-user-' || number)
        FROM generate_series(1, {count}) AS number
      )
    """, database_user)


def seed_matches(client, database_user, count):
    found = existing_matches(database_user, count)
    for number in range(1, count + 1):
        pair = frozenset((user_id(number), user_id(partner(number))))
        if found.get(pair) == uuid.uuid5(NAMESPACE, f'match-{number}'):
            raise RuntimeError('The previous SQL-only match seed is present. Remove its synthetic '
                               'matches before rerunning through the HTTP routes.')

    def create(number):
        first = user_id(number)
        second = user_id(partner(number))
        existing = found.get(frozenset((first, second)))
        if existing is not None:
            return number, existing
        payload = {'target_user_id': str(second), 'decision': 'like'}
        first_result = client.request('POST', '/api/swipes', first, payload, {201})
        if first_result.get('matched'):
            return number, uuid.UUID(first_result['match']['id'])
        payload = {'target_user_id': str(first), 'decision': 'like'}
        second_result = client.request('POST', '/api/swipes', second, payload, {201})
        if not second_result.get('matched'):
            raise RuntimeError(f'Mutual swipe did not create match {number}')
        return number, uuid.UUID(second_result['match']['id'])

    matches = {}
    last_refresh = time.monotonic()
    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        futures = [pool.submit(create, number) for number in range(1, count + 1)]
        for index, future in enumerate(as_completed(futures), 1):
            number, match = future.result()
            matches[number] = match
            if index % 500 == 0:
                print(f'Matches ready: {index}/{count}', flush=True)
                if time.monotonic() - last_refresh > 900:
                    refresh_presence(database_user, count)
                    last_refresh = time.monotonic()
    return matches


def seed_passes(client, database_user, count):
    def swipe(number):
        client.request('POST', '/api/swipes', user_id(number),
                       {'target_user_id': str(user_id(pass_target(number))), 'decision': 'pass'},
                       {201})

    last_refresh = time.monotonic()
    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        futures = [pool.submit(swipe, number) for number in range(1, count + 1)]
        for index, future in enumerate(as_completed(futures), 1):
            future.result()
            if index % 500 == 0 and time.monotonic() - last_refresh > 900:
                refresh_presence(database_user, count)
                last_refresh = time.monotonic()
    print(f'Pass swipes ready: {count}', flush=True)


MESSAGES = (
    'Bonjour, comment se passe ta journée ?',
    'Tu connais un bon endroit pour se promener ?',
    "J'aime bien découvrir de nouveaux cafés.",
    'Quel est ton film préféré en ce moment ?',
    'Une balade ce week-end pourrait être sympa.',
    'Je suis curieux de connaître tes passions.',
    'Merci pour ton message, ça me fait plaisir.',
    'On pourrait continuer cette conversation bientôt.',
)


def seed_messages(client, matches):
    def send(number, match):
        participants = (user_id(number), user_id(partner(number)))
        for index in range(1, 2 * (5 + number % 6) + 1):
            actor = participants[(index + 1) % 2]
            content = '[Test de charge] ' + MESSAGES[(number + index) % len(MESSAGES)]
            client.request('POST', f'/api/matches/{match}/messages', actor,
                           {'content': content}, {201}, message_key(number, index))

    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        futures = [pool.submit(send, number, match) for number, match in matches.items()]
        for index, future in enumerate(as_completed(futures), 1):
            future.result()
            if index % 500 == 0:
                print(f'Conversations ready: {index}/{len(matches)}', flush=True)


def seed_continuations(client, database_user, matches):
    # A controlled clock jump lets the existing continuation route keep fixtures available.
    statuses = {}
    ids = list(matches.values())
    for start in range(0, len(ids), 500):
        batch = ','.join(f"'{match}'::uuid" for match in ids[start:start + 500])
        for row in query(f'SELECT id, status FROM match_init WHERE id IN ({batch})',
                         database_user).splitlines():
            match_id, status = row.split('\t')
            statuses[uuid.UUID(match_id)] = status
        query(f"UPDATE match_init SET expires_at = now() - interval '1 second' "
              f"WHERE id IN ({batch}) AND status = 'active'", database_user)

    def confirm(number, match):
        if statuses[match] == 'confirmed':
            return
        if statuses[match] not in ('active', 'awaiting_continuation'):
            raise RuntimeError(f'Match {match} cannot be continued: {statuses[match]}')
        first = user_id(number)
        second = user_id(partner(number))
        path = f'/api/matches/{match}/continue'
        result = client.request('PATCH', path, first, None, {200})
        if not result.get('match_confirmed'):
            result = client.request('PATCH', path, second, None, {200})
        if not result.get('match_confirmed'):
            raise RuntimeError(f'Match {match} was not confirmed')

    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        futures = [pool.submit(confirm, number, match) for number, match in matches.items()]
        for index, future in enumerate(as_completed(futures), 1):
            future.result()
            if index % 500 == 0:
                print(f'Confirmed matches: {index}/{len(matches)}', flush=True)


def seed_reports(client, database_user, matches):
    reported = set()
    rows = query('SELECT DISTINCT match_id FROM user_report WHERE match_id IS NOT NULL', database_user)
    if rows:
        reported = {uuid.UUID(row) for row in rows.splitlines()}
    reasons = ('spam', 'fake_profile', 'harassment', 'inappropriate_content', 'other')
    created = 0
    for number, match in sorted(matches.items()):
        if number % 25 != 0 or match in reported:
            continue
        first = user_id(number)
        second = user_id(partner(number))
        actor, target = (first, second) if number % 2 == 0 else (second, first)
        response = client.request('POST', '/api/reports', actor,
                                  {'reported_user_id': str(target), 'match_id': str(match),
                                   'reason': reasons[(number // 25) % len(reasons)],
                                   'description': 'Signalement fictif créé pour tester le traitement administratif.'},
                                  {201, 409})
        if 'error' in response:
            if response['error'].get('code') != 'report_already_pending':
                raise RuntimeError(f'Unexpected report conflict for match {match}: {response}')
        else:
            created += 1
    print(f'Reports created: {created}; existing reports kept: {count_reports(matches, reported)}', flush=True)


def count_reports(matches, reported):
    return sum(number % 25 == 0 and match in reported for number, match in matches.items())


def create_sessions(database_user, count):
    rows = query(f"""
      WITH seed AS (
        SELECT uuid_generate_v5('{NAMESPACE}'::uuid, 'histae-load-user-' || number) AS id
        FROM generate_series(1, {count}) AS number
      ), inserted AS (
        INSERT INTO refresh_token_family
          (id, user_id, created_at, last_refreshed_at, expires_at)
        SELECT gen_random_uuid(), account.user_id, now(), now(), now() + interval '8 hours'
        FROM seed JOIN user_account AS account ON account.user_id = seed.id
        WHERE account.role = 'user' AND account.deleted_at IS NULL AND NOT account.is_banned
        RETURNING user_id, id
      )
      SELECT user_id, id FROM inserted
    """, database_user)
    sessions = {}
    for row in rows.splitlines():
        actor, session = row.split('\t')
        sessions[uuid.UUID(actor)] = uuid.UUID(session)
    if len(sessions) != count:
        cleanup_sessions(database_user, sessions)
        raise RuntimeError(f'Expected {count} synthetic sessions, got {len(sessions)}')
    return sessions


def cleanup_sessions(database_user, sessions):
    ids = list(sessions.values())
    for start in range(0, len(ids), 500):
        batch = ','.join(f"'{session}'::uuid" for session in ids[start:start + 500])
        query(f'DELETE FROM refresh_token_family WHERE id IN ({batch})', database_user)


def main():
    if len(sys.argv) != 2 or not sys.argv[1].isdigit():
        raise SystemExit('Usage: seed-dev-load-api.py COUNT')
    count = int(sys.argv[1])
    if not 5000 <= count <= 10000 or count % 100:
        raise SystemExit('COUNT must be 5000..10000 and divisible by 100')
    if docker('exec', '-T', 'api', 'printenv', 'ENV') != 'development':
        raise RuntimeError('Refusing to call a non-development API')
    if docker('exec', '-T', 'postgres', 'printenv', 'POSTGRES_DB') != 'histae-dev':
        raise RuntimeError('Refusing to use a non-development database')
    database_user = docker('exec', '-T', 'postgres', 'printenv', 'POSTGRES_USER')
    secret = docker('exec', '-T', 'api', 'printenv', 'JWT_SECRET').encode()
    try:
        key_id = docker('exec', '-T', 'api', 'printenv', 'JWT_ACTIVE_KID') or 'primary'
    except subprocess.CalledProcessError:
        key_id = 'primary'
    if len(secret) < 32:
        raise RuntimeError('JWT_SECRET is missing or invalid')
    published = docker('port', 'api', '8080').splitlines()[0]
    port = int(published.rsplit(':', 1)[1])
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
    connection.request('GET', '/health/ready')
    response = connection.getresponse()
    response.read()
    connection.close()
    if response.status != 200:
        raise RuntimeError(f'API is not ready: HTTP {response.status}')
    sessions = create_sessions(database_user, count)
    try:
        client = Client(port, secret, key_id, sessions)
        identity = client.request('GET', '/api/auth/me', user_id(1), None, {200})
        if identity.get('user_id') != str(user_id(1)) or not identity.get('onboarding_complete'):
            raise RuntimeError('The synthetic mobile session did not authenticate correctly')
        discovery = client.request('GET', '/api/users/me/discovery-status', user_id(1), None, {200})
        if not discovery.get('ready'):
            raise RuntimeError(f'The synthetic account is not discovery-ready: {discovery}')
        matches = seed_matches(client, database_user, count)
        refresh_presence(database_user, count)
        seed_passes(client, database_user, count)
        seed_messages(client, matches)
        seed_continuations(client, database_user, matches)
        seed_reports(client, database_user, matches)
        print(f'API load seed complete: {count} users, {len(matches)} matches.', flush=True)
    finally:
        cleanup_sessions(database_user, sessions)


if __name__ == '__main__':
    main()
