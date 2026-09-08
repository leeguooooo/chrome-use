#!/usr/bin/env python3
"""Check real relay session isolation using the loopback relay-tabs fixture.

Requires a separately started candidate browser/extension/native-host and fixture
HTTP server. Does not install extensions, modify other profiles, or adopt tabs.
"""
import argparse
import json
import os
import subprocess
import time
from urllib.parse import urlparse, urlencode

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--browser', required=True)
parser.add_argument('--base-url', required=True)
parser.add_argument('--session-prefix', default='relay-isolation-check')
parser.add_argument('--output', required=True)
args = parser.parse_args()
assert urlparse(args.base_url).hostname in ('localhost', '127.0.0.1'), 'Loopback fixtures only'
env = dict(os.environ, CHROME_USE_NO_UPDATE_CHECK='1')
assert env.get('CHROME_USE_RELAY_DIR'), 'Set a private relay registry before running this check'
records = []
sessions = [args.session_prefix + '-beta', args.session_prefix + '-gamma']

def run(session, *command, expect_success=True):
    started = time.monotonic()
    result = subprocess.run([args.binary, '--session', session, '--browser', args.browser,
                             *command, '--json'], env=env, text=True, capture_output=True, timeout=60)
    try:
        response = json.loads(result.stdout)
    except ValueError:
        response = {'success': False, 'error': 'Non-JSON response', 'raw': result.stdout[:1000]}
    recorded = response
    if command[:2] == ('tab', 'list') and isinstance(response.get('data'), dict):
        tabs = response['data'].get('tabs', [])
        own_fixtures = [tab for tab in tabs if tab.get('title', '').startswith('Relay ' + args.session_prefix)]
        recorded = dict(response, data={'tabs': own_fixtures, 'otherTabsOmitted': len(tabs) - len(own_fixtures)})
    records.append({'session': session, 'command': list(command), 'exit_code': result.returncode,
                    'seconds': round(time.monotonic() - started, 3), 'response': recorded})
    if expect_success:
        assert result.returncode == 0 and response.get('success'), records[-1]
    else:
        assert result.returncode != 0 and response.get('success') is False, records[-1]
    return response.get('data') or {}

verdict = 'FAIL'
error = None
boundary = None
try:
    for session, identity in zip(sessions, sessions):
        run(session, 'open', args.base_url + '/relay-tabs.html?' + urlencode({'id': identity}), '--observe')
    beta, gamma = sessions
    for _ in range(3):
        run(beta, 'click', '#increment', '--observe')
    for session, expected in [(beta, 3), (gamma, 0)]:
        data = run(session, 'eval', '({title:document.title,count:Number(document.querySelector("#count").value)})')
        assert data['result']['count'] == expected, data
        assert data['result']['title'] == 'Relay ' + session, data
    run(beta, 'click', '#navigate', '--observe')
    assert run(beta, 'get', 'title')['title'] == 'Relay ' + beta + '-next'
    assert run(gamma, 'get', 'title')['title'] == 'Relay ' + gamma
    tabs = run(beta, 'tab', 'list')['tabs']
    foreign = [tab for tab in tabs if tab.get('title') == 'Relay ' + gamma and tab.get('ownership') == 'foreign']
    assert len(foreign) <= 1, f"Unexpected duplicate fixture tabs: {len(foreign)}"
    if foreign:
        run(beta, 'tab', 'select', foreign[0]['tabId'], expect_success=False)
        boundary = 'foreign tab exposed but selection refused'
    else:
        # A relay may enforce the boundary earlier by withholding the foreign
        # tab altogether. Its owner was just checked above, so it is not missing.
        boundary = 'foreign tab not exposed to this session'
    assert run(beta, 'get', 'title')['title'] == 'Relay ' + beta + '-next'
    verdict = 'PASS'
except Exception as exc:
    error = str(exc)
finally:
    # Only these newly named test sessions are closed, including after assertion failures.
    for session in sessions:
        try:
            run(session, 'close')
        except Exception as exc:
            verdict = 'FAIL'
            error = (error or '') + '; cleanup: ' + str(exc)
    with open(args.output, 'w') as stream:
        json.dump({'verdict': verdict, 'error': error, 'isolationBoundary': boundary, 'records': records}, stream, indent=2)
print(json.dumps({'verdict': verdict, 'error': error, 'commands': len(records), 'output': args.output}))
raise SystemExit(0 if verdict == 'PASS' else 1)
