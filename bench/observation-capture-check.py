#!/usr/bin/env python3
"""Verify that a returned action is not confused with a failed observation.

The loopback fixture records a real POST, then introduces protected content.
Only the synthetic fixture's counter API is read outside the browser.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--state-file', required=True)
parser.add_argument('--binary', required=True)
parser.add_argument('--port', required=True, type=int)
parser.add_argument('--output', required=True)
args = parser.parse_args()
state = json.loads(Path(args.state_file).read_text())
env = dict(os.environ, CHROME_USE_NO_UPDATE_CHECK='1', CHROME_USE_RELAY_DIR=state['registry'],
           AGENT_BROWSER_SOCKET_DIR=state['socketDir'], NO_COLOR='1')
profiles = json.loads(subprocess.check_output([args.binary, 'browsers', '--json'], env=env, text=True))['data']['browsers']
assert len(profiles) == 1
browser = profiles[0]['id']
records, sessions = [], []
verdict, error = 'FAIL', None
stamp = str(time.monotonic_ns())

def run(session, command, as_json=True):
    cmd = [args.binary, '--session', session, '--browser', browser, *command]
    if as_json:
        cmd.append('--json')
    result = subprocess.run(cmd, env=env, text=True, capture_output=True, timeout=45)
    payload = json.loads(result.stdout) if as_json else result.stdout
    records.append({'session': session, 'command': command, 'exitCode': result.returncode,
                    'result': payload, 'stderr': result.stderr})
    assert result.returncode == 0, records[-1]
    return payload

try:
    for kind in ('click-json', 'form-json', 'click-text'):
        session = 'capture-' + kind
        sessions.append(session)
        case = stamp + '-' + kind
        page = 'observation-form-failure.html' if kind == 'form-json' else 'observation-failure.html'
        url = f'http://127.0.0.1:{args.port}/{page}?trigger=after&case={case}'
        assert run(session, ['open', url, '--observe'])['success']
        command = ['form', 'fill', '--map', '{"#value":"fixture"}', '--submit', '#submit'] if kind == 'form-json' else ['click', '#submit', '--observe']
        result = run(session, command, as_json=kind != 'click-text')
        if kind != 'click-text':
            observed = result['data']['observed']
            assert result['success'] and observed['status'] == 'unavailable'
            assert observed['changed'] is None and observed['retryAction'] is False
            assert 'delta' not in observed and 'urlChanged' not in observed
            assert observed['errors']
            if kind == 'form-json':
                assert result['data']['errors'] is None
        else:
            assert 'observation status: unavailable' in result
            assert 'observed: no change' not in result
            assert 'Do not replay' in records[-1]['stderr']
        with urllib.request.urlopen(f'http://127.0.0.1:{args.port}/counter?case={case}', timeout=5) as response:
            count = json.load(response)['count']
        assert count == 1
        records[-1]['backendCount'] = count
    verdict = 'PASS'
except Exception as exc:
    error = str(exc)
finally:
    for session in sessions:
        try:
            assert run(session, ['close'])['success']
        except Exception as exc:
            verdict, error = 'FAIL', (error or '') + '; cleanup: ' + str(exc)
    Path(args.output).write_text(json.dumps({'verdict': verdict, 'error': error, 'records': records}, indent=2))
print(json.dumps({'verdict': verdict, 'error': error, 'commands': len(records), 'output': args.output}))
raise SystemExit(0 if verdict == 'PASS' else 1)
