#!/usr/bin/env python3
"""Verify budgeted context snapshots reconstruct a stable page and keep usable refs."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--state-file', required=True)
p.add_argument('--port', type=int, required=True)
p.add_argument('--output', required=True)
a = p.parse_args()
s = json.loads(Path(a.state_file).read_text())
env = dict(os.environ, CHROME_USE_RELAY_DIR=s['registry'], AGENT_BROWSER_SOCKET_DIR=s['socketDir'], CHROME_USE_NO_UPDATE_CHECK='1', NO_COLOR='1')
b = s['binary']
profiles = json.loads(subprocess.check_output([b, 'browsers', '--json'], env=env, text=True))['data']['browsers']
assert len(profiles) == 1
base = [b, '--browser', profiles[0]['id'], '--session', 'context-budget']
records = []
def run(args):
    r = subprocess.run(base + args + ['--json'], env=env, text=True, capture_output=True, timeout=45)
    obj = json.loads(r.stdout)
    records.append({'args': args, 'exitCode': r.returncode, 'stdoutBytes': len(r.stdout.encode()), 'result': obj})
    assert r.returncode == 0 and obj['success'], records[-1]
    return obj['data']

verdict, error = 'FAIL', None
try:
    run(['open', f'http://127.0.0.1:{a.port}/product-context.html?layout=long'])
    whole = run(['snapshot', '-i'])['snapshot'].splitlines()
    cursor, reconstructed = 0, []
    for _ in range(100):
        d = run(['snapshot', '-i', '--max-bytes', '200', '--from', str(cursor)])
        lines = d['snapshot'].splitlines()
        assert lines and (len(d['snapshot'].encode()) <= 200 or len(lines) == 1)
        reconstructed.extend(lines)
        if 'nodes' in d:
            assert d['nodes']['from'] == cursor and d['nodes']['to'] == cursor + len(lines)
        if 'nextFrom' not in d:
            break
        assert d['nextFrom'] > cursor
        cursor = d['nextFrom']
    else:
        raise AssertionError('Continuation did not terminate')
    assert reconstructed == whole, 'Continuation changed, duplicated, or omitted nodes'
    folder = [line for line in reconstructed if '- button ' in line and 'context="Folder |' in line]
    assert len(folder) == 1 and 'Price: $9.00' in folder[0]
    ref = re.search(r'\bref=(e\d+)', folder[0])[1]
    result = run(['click', '@' + ref, '--observe'])
    assert 'Cart: folder' in json.dumps(result), result
    verdict = 'PASS'
except Exception as exc:
    error = str(exc)
finally:
    subprocess.run(base + ['close'], env=env, capture_output=True, timeout=30)
    Path(a.output).write_text(json.dumps({'verdict': verdict, 'error': error, 'records': records}, indent=2))
print(json.dumps({'verdict': verdict, 'error': error, 'commands': len(records), 'output': a.output}))
raise SystemExit(0 if verdict == 'PASS' else 1)
