#!/usr/bin/env python3
"""Choose a product from observed context, then verify the actual cart receipt."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument('--state-file', required=True)
parser.add_argument('--port', type=int, required=True)
parser.add_argument('--output', required=True)
args = parser.parse_args()
s = json.loads(Path(args.state_file).read_text())
env = dict(os.environ, CHROME_USE_RELAY_DIR=s['registry'],
           AGENT_BROWSER_SOCKET_DIR=s['socketDir'], CHROME_USE_NO_UPDATE_CHECK='1', NO_COLOR='1')
binary = s['binary']
browsers = json.loads(subprocess.check_output([binary, 'browsers', '--json'], env=env, text=True))['data']['browsers']
assert len(browsers) == 1
base = [binary, '--browser', browsers[0]['id'], '--session', 'product-context']
records = []

def run(command):
    result = subprocess.run(base + command, env=env, capture_output=True, text=True, timeout=45)
    records.append({'command': command, 'exitCode': result.returncode,
                    'stdout': result.stdout, 'stderr': result.stderr,
                    'stdoutBytes': len(result.stdout.encode())})
    assert result.returncode == 0, records[-1]
    return result.stdout

verdict, error = 'FAIL', None
try:
    observed = run(['open', f'http://127.0.0.1:{args.port}/product-context.html', '--observe'])
    choices = []
    for line in observed.splitlines():
        if not line.startswith('- button ') or 'disabled' in line:
            continue
        ref = re.search(r'\bref=(e\d+)\b', line)
        context = re.search(r'\bcontext=("(?:[^"\\]|\\.)*")', line)
        assert ref and context, 'Actionable button lacks context: ' + line
        text = json.loads(context[1])
        price = re.search(r'Price: \$(\d+\.\d{2})', text)
        assert price and 'In stock' in text and '[truncated]' not in text
        choices.append((float(price[1]), ref[1]))
    assert len(choices) == 2
    _, chosen = min(choices)
    run(['click', '@' + chosen, '--observe'])
    receipt = run(['get', 'text', '#receipt']).strip()
    assert receipt == 'Cart: folder', receipt
    verdict = 'PASS'
except Exception as exc:
    error = str(exc)
finally:
    subprocess.run(base + ['close'], env=env, capture_output=True, timeout=30)
    Path(args.output).write_text(json.dumps({'verdict': verdict, 'error': error,
                                            'records': records}, indent=2))
print(json.dumps({'verdict': verdict, 'error': error, 'commands': len(records), 'output': args.output}))
raise SystemExit(0 if verdict == 'PASS' else 1)
