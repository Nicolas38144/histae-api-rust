#!/usr/bin/env python3
"""Prepare/restore bounded synthetic feed fixtures on the temporary load API."""

import argparse
import importlib.util
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import uuid


ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('load_prepare', ROOT / 'scripts/prepare-dev-load-test.py')
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)
seed = prepare.seed
BASE = 'http://127.0.0.1:18081'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--compose-file', type=Path, required=True)
    parser.add_argument('--variant', choices=('dense-1km', 'large-radius-500km', 'sparse-empty',
                                            'deep-page-21', 'heavy-exclusions'))
    parser.add_argument('--restore', action='store_true')
    args = parser.parse_args()
    prepare.API_SERVICE = 'load-api'
    seed.COMPOSE.extend(['-f', str(args.compose_file.resolve())])
    database_user = prepare.guard()
    for name in ('SMS_PROVIDER', 'BILLING_PROVIDER', 'PUSH_PROVIDER'):
        if prepare.optional_env(name, 'invalid') != 'disabled':
            raise RuntimeError('External providers must be disabled')
    api = json.loads(subprocess.check_output(['docker', 'inspect', seed.docker('ps', '-q', 'load-api')], text=True))[0]
    if api['NetworkSettings']['Ports']['8080/tcp'] != [{'HostIp': '127.0.0.1', 'HostPort': '18081'}]:
        raise RuntimeError('Expected the temporary loopback API on port 18081')
    postgres = seed.docker('ps', '-q', 'postgres')
    synthetic = {str(seed.user_id(i)) for i in range(1, 10001)}
    snapshot_file = args.output.with_suffix('.snapshot.json')

    def query(sql):
        # Large UUID sets travel through stdin, not the operating system argv limit.
        return subprocess.run(['docker', 'exec', '-i', postgres, 'psql', '-X', '-qAt',
                               '-v', 'ON_ERROR_STOP=1', '-U', database_user, '-d', 'histae-dev'],
                              input=sql, check=True, capture_output=True, text=True).stdout.strip()

    def array(values):
        values = [str(uuid.UUID(x)) for x in values]
        if any(x not in synthetic for x in values) or len(values) != len(set(values)):
            raise RuntimeError('Refusing non-synthetic or duplicate owners')
        return 'ARRAY[' + ','.join(literal(x) for x in values) + ']::uuid[]'

    def restore(saved):
        owners, banned = saved['owners'], saved['banned']
        selected = array(owners)
        all_ids = array(owners + banned)
        if int(query(f"SELECT count(*) FROM user_account WHERE user_id=ANY({all_ids}) AND role='user' AND deleted_at IS NULL")) != len(owners + banned):
            raise RuntimeError('Restoration owners changed role or availability')
        statements = []
        if banned:
            statements.append(f"UPDATE user_account SET is_banned=false WHERE user_id=ANY({array(banned)}) AND role='user'")
        for table, rows in saved['original'].items():
            if table not in ('user_preferences', 'user_presence') or {x['user_id'] for x in rows} != set(owners):
                raise RuntimeError('Invalid restoration table or owners')
            fields = ['min_age', 'max_age', 'max_distance_km', 'looking_for'] if table == 'user_preferences' else ['latitude', 'longitude', 'is_location_fresh', 'updated_at']
            statements.append(f"UPDATE {table} t SET " + ','.join(f'{k}=v.{k}' for k in fields) +
                              f" FROM json_populate_recordset(NULL::{table},{literal(json.dumps(rows))}::json) v WHERE t.user_id=v.user_id")
        query('BEGIN;' + ';'.join(statements) + ';COMMIT;')
        for table, rows in saved['original'].items():
            actual = json.loads(query(f"SELECT json_agg(x ORDER BY user_id) FROM {table} x WHERE user_id=ANY({selected})"))
            if actual != rows:
                raise RuntimeError('Restoration differs from the original synthetic state')
        if banned and int(query(f"SELECT count(*) FROM user_account WHERE user_id=ANY({array(banned)}) AND is_banned")):
            raise RuntimeError('A temporary ban remains')

    if args.restore:
        restore(json.loads(snapshot_file.read_text()))
        args.output.unlink(missing_ok=True)
        snapshot_file.unlink()
        print('Feed variant fixture restored and private token file removed.')
        return
    if not args.variant:
        parser.error('--variant is required when preparing')
    if args.output.exists() or snapshot_file.exists():
        raise RuntimeError('Existing variant: restore it before preparing another')
    fixture = json.loads(args.fixture.read_text())
    if fixture.get('environment') != 'development' or fixture.get('schema_version') != 1 or fixture['expires_at'] < time.time() + 600:
        raise RuntimeError('A fresh development fixture is required')
    owners = fixture['accounts'][1000:1200]
    if len(owners) != 200:
        raise RuntimeError('At least 1200 active synthetic fixture accounts are required')
    owner_ids = [a['user_id'] for a in owners]
    banned = []
    if args.variant == 'heavy-exclusions':
        count = math.ceil(len(fixture['accounts']) * 0.9)
        banned = [a['user_id'] for a in fixture['accounts'] if a['user_id'] not in owner_ids][:count]
    selected, all_ids = array(owner_ids), array(owner_ids + banned)
    if int(query(f"SELECT count(*) FROM user_account WHERE user_id=ANY({all_ids}) AND role='user' AND deleted_at IS NULL AND NOT is_banned")) != len(owner_ids + banned):
        raise RuntimeError('Expected active synthetic mobile accounts')

    def request(account, path):
        req = urllib.request.Request(BASE + path, headers={'Authorization': 'Bearer ' + account['access_token'],
                                                         'Accept': 'application/json'})
        with urllib.request.build_opener(NoRedirect).open(req, timeout=15) as response:
            return json.load(response)

    for account in owners:
        if request(account, '/api/auth/me')['user_id'] != account['user_id']:
            raise RuntimeError('A fixture token does not belong to its synthetic owner')
    original = {}
    for table in ('user_preferences', 'user_presence'):
        original[table] = json.loads(query(f"SELECT json_agg(x ORDER BY user_id) FROM {table} x WHERE user_id=ANY({selected})"))
        if len(original[table]) != 200:
            raise RuntimeError('Missing synthetic profile prerequisites')
    saved = {'owners': owner_ids, 'banned': banned, 'original': original}
    with os.fdopen(os.open(snapshot_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as target:
        json.dump(saved, target)
    max_distance = 1 if args.variant == 'dense-1km' else 500 if args.variant == 'large-radius-500km' else 50
    try:
        if args.variant != 'deep-page-21':
            query(f"UPDATE user_preferences SET max_distance_km={max_distance} WHERE user_id=ANY({selected})")
        if args.variant == 'sparse-empty':
            values = ','.join(f"('{a['user_id']}'::uuid,{-60 + (i // 20) * 10},{-160 + (i % 20) * 15})" for i, a in enumerate(owners))
            query(f"UPDATE user_presence p SET latitude=v.lat,longitude=v.lon,is_location_fresh=true,updated_at=clock_timestamp() FROM (VALUES {values}) v(user_id,lat,lon) WHERE p.user_id=v.user_id")
        allowed = []
        if banned:
            query(f"UPDATE user_account SET is_banned=true WHERE user_id=ANY({array(banned)}) AND role='user'")
            allowed = json.loads(query('SELECT json_agg(user_id) FROM user_account WHERE NOT is_banned AND deleted_at IS NULL'))
        accounts = []
        for account in owners:
            item = dict(account, previous_ids=[], cursor='')
            if args.variant == 'deep-page-21':
                for _ in range(20):
                    body = request(account, '/api/feed?limit=20' + ('&cursor=' + urllib.parse.quote(item['cursor'], safe='') if item['cursor'] else ''))
                    if not body['profiles'] or not body['next_cursor']:
                        raise RuntimeError('Insufficient candidates for page 21')
                    item['previous_ids'].extend(x['user_id'] for x in body['profiles'])
                    item['cursor'] = body['next_cursor']
                if len(item['previous_ids']) != 400 or len(set(item['previous_ids'])) != 400:
                    raise RuntimeError('Repeated profiles while preparing deep pagination')
            accounts.append(item)
        output = {'environment': 'development', 'schema_version': 1, 'api_origin': BASE,
                  'variant': args.variant, 'expires_at': fixture['expires_at'], 'accounts': accounts,
                  'expect_empty': args.variant == 'sparse-empty', 'allowed_ids': allowed,
                  'max_distance_km': max_distance}
        with os.fdopen(os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as target:
            json.dump(output, target)
    except BaseException:
        restore(saved)
        args.output.unlink(missing_ok=True)
        snapshot_file.unlink()
        raise
    print(f'Prepared {args.variant}: 200 readers, {len(banned)} temporary exclusions. Restore after the measurement.')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(f'Feed variant preparation failed ({type(error).__name__}). Review the private snapshot.', file=sys.stderr)
        sys.exit(1)
