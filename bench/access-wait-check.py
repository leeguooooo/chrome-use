#!/usr/bin/env python3
"""Check early access denial and real slow-page readiness through a private relay."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('--state-file', required=True)
parser.add_argument('--port', required=True, type=int)
parser.add_argument('--output', required=True)
args = parser.parse_args()
state = json.loads(Path(args.state_file).read_text())
assert Path(state['registry']).is_dir(), 'Start a live private relay fixture first'
env = dict(os.environ, CHROME_USE_NO_UPDATE_CHECK='1', CHROME_USE_RELAY_DIR=state['registry'],
           AGENT_BROWSER_SOCKET_DIR=state['socketDir'])
profiles = json.loads(subprocess.check_output([state['binary'], 'browsers', '--json'], env=env, text=True))['data']['browsers']
assert len(profiles) == 1, 'Expected one private fixture browser'
browser = profiles[0]['id']
records, sessions = [], []
verdict, error = 'FAIL', None

def run(session, *command):
    started = time.monotonic()
    proc = subprocess.run([state['binary'], '--session', session, '--browser', browser,
        *command, '--json'], env=env, capture_output=True, text=True, timeout=45)
    response = json.loads(proc.stdout)
    row = {'session': session, 'command': command, 'seconds': round(time.monotonic()-started, 3),
           'exit_code': proc.returncode, 'response': response}
    records.append(row)
    return row

try:
    for index, wait in enumerate(['load', 'load', 'load', 'domcontentloaded', 'none']):
        session = 'wait-check-' + str(index)
        sessions.append(session)
        row = run(session, 'open', f'http://127.0.0.1:{args.port}/relay-tabs.html?id=blocked-{index}', '--wait-until', wait, '--observe')
        assert row['response'].get('code') == 'debugger_access_denied', row
        assert row['response'].get('retryable') is False, row
        assert row['seconds'] < 10, 'Denial still waited for the lifecycle timeout'
    session = 'wait-slow-control'
    sessions.append(session)
    row = run(session, 'open', f'http://localhost:{args.port}/slow-lifecycle.html', '--wait-until', 'load', '--observe')
    assert row['response'].get('success') and row['seconds'] >= 1.8, row
    ready = run(session, 'eval', '({ready:document.readyState,complete:document.images[0].complete,width:document.images[0].naturalWidth})')
    assert ready['response']['data']['result'] == {'ready': 'complete', 'complete': True, 'width': 1}, ready
    verdict = 'PASS'
except Exception as exc:
    error = str(exc)
finally:
    for session in sessions:
        try:
            assert run(session, 'close')['response'].get('success')
        except Exception as exc:
            verdict, error = 'FAIL', (error or '') + '; cleanup: ' + str(exc)
    Path(args.output).write_text(json.dumps({'verdict': verdict, 'error': error, 'records': records}, indent=2))
print(json.dumps({'verdict': verdict, 'error': error, 'commands': len(records), 'output': args.output}))
raise SystemExit(0 if verdict == 'PASS' else 1)
