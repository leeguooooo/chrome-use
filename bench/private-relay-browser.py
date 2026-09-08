#!/usr/bin/env python3
"""Start a headed relay-only fixture browser without publishing it globally.

No remote debugging port is opened. Use the printed registry with the candidate
CLI and explicit browser ID. The caller must preserve any unexpected user tabs
before stopping the process. Requires Chrome for Testing on macOS/Linux.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--chrome', required=True)
parser.add_argument('--extension', action='append', required=True)
parser.add_argument('--state-file', required=True)
args = parser.parse_args()
binary = str(Path(args.binary).resolve())
help_text = subprocess.check_output([binary, '--help'], text=True,
    env=dict(os.environ, CHROME_USE_NO_UPDATE_CHECK='1'))
if 'CHROME_USE_RELAY_DIR' not in help_text:
    raise SystemExit('Candidate binary does not support isolated relay discovery')
state_file = Path(args.state_file)
if state_file.exists():
    raise SystemExit('State file already exists; preserve the running fixture or choose a new path')
profile = Path(tempfile.mkdtemp(prefix='chrome-use-private-validation-'))
socket_dir = Path(tempfile.mkdtemp(prefix='cu-sock-', dir='/tmp'))
registry = profile / 'relay-registry'
registry.mkdir(mode=0o700)
launcher = profile / 'native-host.sh'
launcher.write_text('#!/bin/sh\nexport CHROME_USE_NO_UPDATE_CHECK=1\nexport CHROME_USE_RELAY_DIR=' +
    shlex.quote(str(registry)) + '\nexec ' + shlex.quote(binary) + ' __nm-host "$@"\n')
launcher.chmod(0o700)
manifest_dir = profile / 'NativeMessagingHosts'
manifest_dir.mkdir()
(manifest_dir / 'com.agent_browser.connect.json').write_text(json.dumps({
    'name': 'com.agent_browser.connect', 'description': 'Private relay test host',
    'path': str(launcher), 'type': 'stdio',
    'allowed_origins': ['chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/'],
}))
extensions = ','.join(str(Path(path).resolve()) for path in args.extension)
log = (profile / 'chrome.log').open('w')
process = subprocess.Popen([args.chrome, '--no-first-run', '--no-default-browser-check',
    '--user-data-dir=' + str(profile), '--load-extension=' + extensions,
    '--disable-extensions-except=' + extensions, 'about:blank'], stdout=log, stderr=log)
state = {'pid': process.pid, 'profile': str(profile), 'registry': str(registry),
         'binary': binary, 'socketDir': str(socket_dir), 'remoteDebuggingPort': False}
try:
    with state_file.open('x') as stream:
        json.dump(state, stream)
except BaseException:
    process.terminate()
    process.wait(timeout=10)
    log.close()
    raise
state_file.chmod(0o600)
print(json.dumps(state), flush=True)

def stop(_signal, _frame):
    process.terminate()

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
try:
    raise SystemExit(process.wait())
finally:
    log.close()
