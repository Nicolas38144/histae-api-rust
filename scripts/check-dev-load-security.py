#!/usr/bin/env python3
"""Check live authorization on the temporary development load API.

Run alongside a read-only benchmark whose fixture excludes the first 500 accounts.
Only synthetic accounts are changed. Consent/block/session changes use HTTP routes;
bans and old consent versions are injected as PostgreSQL fixtures. Snapshots permit
restoration after failure. This is not a load generator or a production test tool.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.error
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
    parser.add_argument('--compose-file', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--restore', action='store_true')
    args = parser.parse_args()
    prepare.API_SERVICE = 'load-api'
    seed.COMPOSE.extend(['-f', str(args.compose_file.resolve())])
    database_user = prepare.guard()
    for name in ('SMS_PROVIDER', 'BILLING_PROVIDER', 'PUSH_PROVIDER'):
        if prepare.optional_env(name, 'invalid') != 'disabled':
            raise RuntimeError('External providers must be disabled')
    container = seed.docker('ps', '-q', 'load-api')
    inspected = json.loads(subprocess.check_output(['docker', 'inspect', container], text=True))[0]
    if inspected['NetworkSettings']['Ports']['8080/tcp'] != [{'HostIp': '127.0.0.1', 'HostPort': '18081'}]:
        raise RuntimeError('Expected the temporary loopback load API on port 18081')
    snapshot_file = args.output.with_suffix('.snapshot.json')
    synthetic = {str(seed.user_id(i)) for i in range(1, 10001)}
    snapshots = {}
    block_pairs = []
    results = []
    completed = False

    def query(sql):
        return seed.query(sql, database_user).strip()

    def state(actor):
        actor = str(uuid.UUID(actor))
        if actor not in synthetic:
            raise RuntimeError('Refusing a non-synthetic account')
        value = json.loads(query(f"""
          SELECT json_build_object('role', a.role, 'is_banned', a.is_banned,
            'sex', p.sex,
            'preferences', (SELECT row_to_json(x) FROM user_preferences x WHERE user_id=a.user_id),
            'presence', (SELECT row_to_json(x) FROM user_presence x WHERE user_id=a.user_id),
            'consents', (SELECT coalesce(json_agg(x ORDER BY event_sequence),'[]'::json)
                         FROM user_consent x WHERE user_id=a.user_id))
          FROM user_account a JOIN user_profile p ON p.user_id=a.user_id
          WHERE a.user_id='{actor}' AND a.deleted_at IS NULL
        """))
        if value['role'] != 'user':
            raise RuntimeError('Refusing a synthetic administrator')
        return value

    def save():
        with tempfile.NamedTemporaryFile('w', dir=snapshot_file.parent, delete=False) as target:
            json.dump({'users': snapshots, 'block_pairs': block_pairs}, target)
            temporary = Path(target.name)
        try:
            temporary.replace(snapshot_file)
        finally:
            temporary.unlink(missing_ok=True)

    def remember(actor):
        if actor not in snapshots:
            snapshots[actor] = state(actor)
            save()

    def restore_user(actor):
        original = snapshots[actor]
        state(actor)  # Validate ownership and current role again before writing.
        statements = [f"UPDATE user_account SET is_banned={str(original['is_banned']).lower()} WHERE user_id='{actor}' AND role='user'",
                      f"UPDATE user_profile SET sex={literal(original['sex']) if original['sex'] is not None else 'NULL'} WHERE user_id='{actor}'"]
        for table, key in [('user_preferences', 'preferences'), ('user_presence', 'presence'), ('user_consent', 'consents')]:
            rows = original[key] if key == 'consents' else [original[key]] if original[key] else []
            if any(row['user_id'] != actor for row in rows):
                raise RuntimeError('Invalid restoration owner')
            statements.append(f"DELETE FROM {table} WHERE user_id='{actor}'")
            if rows:
                statements.append(f"INSERT INTO {table} SELECT * FROM json_populate_recordset(NULL::{table}, {literal(json.dumps(rows))}::json)")
        query('BEGIN;' + ';'.join(statements) + ';COMMIT;')
        if state(actor) != original:
            raise RuntimeError('Synthetic state restoration differs from its snapshot')

    def restore_all():
        for actor in snapshots:
            restore_user(actor)
        for owner, target in block_pairs:
            if owner not in synthetic or target not in synthetic:
                raise RuntimeError('Invalid block restoration owner')
            query(f"DELETE FROM user_block WHERE blocker_id='{uuid.UUID(owner)}' AND blocked_id='{uuid.UUID(target)}'")

    if args.restore:
        saved = json.loads(snapshot_file.read_text())
        snapshots, block_pairs = saved['users'], saved['block_pairs']
        restore_all()
        snapshot_file.unlink()
        print('Synthetic security fixtures restored.')
        return
    if args.output.exists() or snapshot_file.exists():
        raise RuntimeError('Existing results or snapshot: restore/review them first')
    fixture = json.loads(args.fixture.read_text())
    if fixture.get('environment') != 'development' or fixture.get('schema_version') != 1 or fixture['expires_at'] < time.time() + 300:
        raise RuntimeError('A fresh development fixture is required')
    cohort = fixture['accounts'][:500]
    if len(cohort) != 500 or any(str(uuid.UUID(a['user_id'])) not in synthetic for a in cohort):
        raise RuntimeError('Expected 500 reserved synthetic accounts')
    accounts = {a['user_id']: a for a in cohort}
    used = {cohort[0]['user_id'], cohort[6]['user_id']}
    identities_checked = set()

    def request(account, path, method='GET', body=None):
        headers = {'Authorization': 'Bearer ' + account['access_token'],
                   'Accept': 'application/json', 'User-Agent': 'histae-load-security'}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        request = urllib.request.Request(BASE + path, data=json.dumps(body).encode() if body is not None else None,
                                         headers=headers, method=method)
        try:
            response = urllib.request.build_opener(NoRedirect).open(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None

    def wave(name, account, path, status=200, code=None, excluded=None):
        with ThreadPoolExecutor(max_workers=8) as pool:
            responses = list(pool.map(lambda _: request(account, path), range(16)))
        valid = all(s == status and (code is None or p.get('error', {}).get('code') == code)
                    and (excluded is None or (isinstance(p.get('profiles'), list)
                                             and all(x['user_id'] != excluded for x in p['profiles'])))
                    for s, p in responses)
        results.append({'case': name, 'requests': len(responses), 'passed': valid,
                        'statuses': sorted({s for s, _ in responses})})
        if not valid:
            raise RuntimeError('An authorization regression failed')

    def verify_identity(account):
        if account['user_id'] not in identities_checked:
            status, payload = request(account, '/api/auth/me')
            if status != 200 or payload.get('user_id') != account['user_id']:
                raise RuntimeError('Fixture token does not belong to its synthetic owner')
            identities_checked.add(account['user_id'])

    def pair(index, reciprocal_page=False):
        for owner in cohort[index:index + 50]:
            if owner['user_id'] in used:
                continue
            verify_identity(owner)
            status, payload = request(owner, '/api/feed?limit=20')
            if status != 200:
                raise RuntimeError('Reserved reader is not ready')
            for profile in payload['profiles']:
                target = profile['user_id']
                if target not in accounts or target in used or target == owner['user_id']:
                    continue
                verify_identity(accounts[target])
                if reciprocal_page:
                    reverse_status, reverse = request(accounts[target], '/api/feed?limit=20')
                    if reverse_status != 200 or not any(p['user_id'] == owner['user_id'] for p in reverse['profiles']):
                        continue
                remember(target)
                used.update((owner['user_id'], target))
                return owner, accounts[target]
        raise RuntimeError('No eligible reserved candidate in the first pages')

    restored = False
    try:
        actor = cohort[0]
        verify_identity(actor)
        wave('mobile_token_cannot_access_admin', actor, '/api/admin/users/' + actor['user_id'],
             401, 'admin_session_invalid')
        forged = dict(actor)
        pieces = actor['access_token'].split('.')
        pieces[2] = ('A' if pieces[2][0] != 'A' else 'B') + pieces[2][1:]
        forged['access_token'] = '.'.join(pieces)
        wave('forged_signature', forged, '/api/auth/me', 401, 'invalid_or_expired_access_token')
        foreign = next(a for a in cohort if a['match_id'] != actor['match_id'] and
                       actor['user_id'] not in query(f"SELECT user1_id::text || ',' || user2_id::text FROM match_init WHERE id='{uuid.UUID(a['match_id'])}'").split(','))
        wave('foreign_conversation', actor, '/api/matches/' + foreign['match_id'] + '/messages',
             404, 'match_not_found')

        owner, target = pair(1)
        query(f"UPDATE user_account SET is_banned=true WHERE user_id='{target['user_id']}' AND role='user'")
        wave('banned_candidate_excluded', owner, '/api/feed?limit=20', excluded=target['user_id'])
        wave('banned_actor_denied', target, '/api/auth/me', 403, 'account_unavailable')
        restore_user(target['user_id'])

        for index, consent in [(2, 'sensitive_data_consent'), (3, 'location_consent')]:
            owner, target = pair(index)
            status, _ = request(target, '/api/users/me/consents', 'PUT',
                                {'consents': [{'consent_type': consent, 'granted': False}]})
            if status != 200:
                raise RuntimeError('Consent withdrawal route failed')
            wave(consent + '_candidate_excluded', owner, '/api/feed?limit=20', excluded=target['user_id'])
            wave(consent + '_reader_not_ready', target, '/api/feed?limit=20', 409, 'discovery_not_ready')
            restore_user(target['user_id'])

        owner, target = pair(4)
        query(f"UPDATE user_consent SET document_version='load-test-old-version' WHERE user_id='{target['user_id']}' AND consent_type='sensitive_data_consent'")
        wave('old_consent_version_excluded', owner, '/api/feed?limit=20', excluded=target['user_id'])
        restore_user(target['user_id'])

        owner, target = pair(5, reciprocal_page=True)
        block_pairs.append((owner['user_id'], target['user_id']))
        save()
        status, _ = request(owner, '/api/users/me/blocks/' + target['user_id'], 'POST')
        if status != 204:
            raise RuntimeError('Block route failed')
        wave('blocked_candidate_excluded', owner, '/api/feed?limit=20', excluded=target['user_id'])
        wave('reverse_block_excluded', target, '/api/feed?limit=20', excluded=owner['user_id'])
        status, _ = request(owner, '/api/users/me/blocks/' + target['user_id'], 'DELETE')
        if status != 204:
            raise RuntimeError('Unblock route failed')

        actor = cohort[6]
        verify_identity(actor)
        status, _ = request(actor, '/api/auth/sessions/' + str(uuid.UUID(actor['session_id'])), 'DELETE')
        if status != 204:
            raise RuntimeError('Session revocation route failed')
        wave('revoked_session_denied', actor, '/api/auth/me', 401, 'authentication_required')
        completed = True
    finally:
        restore_all()
        restored = True
        args.output.write_text(json.dumps({'checks': results, 'passed': completed and len(results) == 13 and all(x['passed'] for x in results),
                                          'requests_checked': sum(x['requests'] for x in results),
                                          'synthetic_state_restored': restored}, indent=2) + '\n')
        if restored:
            snapshot_file.unlink(missing_ok=True)
    print(f"Security checks passed: {len(results)} cases, {sum(x['requests'] for x in results)} concurrent requests.")


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Captured Docker output and HTTP payloads may contain private data.
        print(f'Security check failed ({type(error).__name__}). Review the private results/snapshot.', file=sys.stderr)
        sys.exit(1)
