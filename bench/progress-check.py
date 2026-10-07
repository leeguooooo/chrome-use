#!/usr/bin/env python3
"""Exercise progress advisories through real, isolated CLI launch handshakes."""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time


FIXTURE = b'''<!doctype html><title>Progress fixture</title>
<button id="noop">No visible change</button>
<button id="increment">Increment</button><output id="count">0</output>
<script>window.noopCount=0;
document.querySelector('#noop').onclick=()=>{window.noopCount++};
document.querySelector('#increment').onclick=()=>{
let c=document.querySelector('#count');c.textContent=String(+c.textContent+1)};
</script>'''


class FixtureHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(FIXTURE)))
        self.end_headers()
        self.wfile.write(FIXTURE)

    def log_message(self, *_):
        pass


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def verify_result(record, success=True):
    result = record['result']
    require(isinstance(result, dict), 'CLI response is not a protocol object')
    require(isinstance(result.get('data'), dict), 'Missing protocol data object')
    if success:
        require(record['exitCode'] == 0 and result.get('success') is True,
                'CLI command did not succeed')
    else:
        require(record['exitCode'] == 1 and result.get('data', {}).get('ok') is False,
                'Failed script must report data.ok=false and exit 1')
        advisories = result['data'].get('advisories')
        require(isinstance(advisories, list) and bool(advisories)
                and all(isinstance(item, dict) and item.get('retryAction') is False
                        for item in advisories),
                'Failed script lost its progress advisories')
    timing = result.get('timing')
    require(isinstance(timing, dict), 'Missing structured timing')
    values = [timing.get(k) for k in ('ms', 'cdpBusyMs', 'nonCdpMs')]
    require(all(type(v) is int and v >= 0 for v in values), 'Invalid timing values')
    require(values[0] == values[1] + values[2], 'Timing decomposition disagrees')
    return result.get('data', {})


def kill_owned(proc):
    """Only signal the process group that this runner started."""
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)


def invoke(argv, env, timeout, input_text=None):
    proc = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, start_new_session=True)
    try:
        out, err = proc.communicate(input=input_text, timeout=timeout)
    except subprocess.TimeoutExpired:
        kill_owned(proc)
        proc.communicate()
        raise TimeoutError('Owned CLI invocation exceeded timeout') from None
    return proc.returncode, out, err


def wait_ready(proc, path, timeout=30):
    """Probe the actual owned daemon transport; a socket file alone is insufficient."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        require(proc.poll() is None, 'Owned daemon exited before readiness')
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
                peer.settimeout(0.5)
                peer.connect(str(path))
                peer.sendall(b'{"id":"progress-readiness","action":"status"}\n')
                reply = bytearray()
                while b'\n' not in reply and len(reply) < 65536:
                    chunk = peer.recv(4096)
                    if not chunk:
                        break
                    reply.extend(chunk)
                if isinstance(json.loads(reply), dict):
                    return
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise TimeoutError('Owned daemon transport was not ready within 30 seconds')


def acceptance(binary, expected_sha256, expected_version, output, timeout):
    records, checks = [], []
    report = {'verdict': 'FAIL', 'binary': str(binary), 'records': records, 'checks': checks,
              'binary_source_verified': False, 'model_round_trips': None}
    daemon = None
    # Short socket paths fit macOS sockaddr_un. Never inherit browser/session selectors.
    with tempfile.TemporaryDirectory(prefix='cu-p-', dir='/tmp') as directory:
        root = Path(directory)
        env = {key: os.environ[key] for key in ('PATH', 'HOME', 'TMPDIR', 'SystemRoot')
               if key in os.environ}
        env.update(AGENT_BROWSER_SOCKET_DIR=str(root / 's'),
                   CHROME_USE_RELAY_DIR=str(root / 'relay'),
                   CHROME_USE_NO_UPDATE_CHECK='1', NO_COLOR='1', CI='1',
                   AGENT_BROWSER_ALLOW_HEADLESS='1')
        session = 'progress-check'
        base = [str(binary), '--session', session, '--launch', '--headed', 'false']
        server = ThreadingHTTPServer(('127.0.0.1', 0), FixtureHandler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        url = f'http://127.0.0.1:{server.server_port}/fixture'

        def call(args, success=True, input_text=None):
            code, out, err = invoke(base + args + ['--json'], env, timeout, input_text)
            try:
                result = json.loads(out)
            except ValueError:
                result = None
            record = {'args': args, 'exitCode': code, 'stdoutBytes': len(out.encode()),
                      'stderrBytes': len(err.encode()), 'result': result}
            records.append(record)
            # Only synthetic fixture JSON is persisted; stderr may contain environment paths.
            return verify_result(record, success)

        try:
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            report['sha256'] = digest
            require(digest == expected_sha256.lower(), 'Binary SHA-256 mismatch')
            code, out, _ = invoke([str(binary), '--version'], env, timeout)
            report['version'] = out.splitlines()[0] if out.splitlines() else ''
            require(code == 0 and report['version'] == f'chrome-use {expected_version}',
                    'Binary version mismatch')
            (root / 's').mkdir()
            daemon_env = dict(env, AGENT_BROWSER_DAEMON='1', AGENT_BROWSER_SESSION=session)
            daemon = subprocess.Popen([str(binary)], env=daemon_env, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL, start_new_session=True)
            wait_ready(daemon, root / 's' / f'{session}.sock')
            call(['open', url])
            hints = []
            for _ in range(5):
                data = call(['click', '#noop', '--observe'])
                hints.append(data.get('observed', {}).get('noProgress'))
            actual = call(['eval', 'window.noopCount']).get('result')
            report['noopCounter'] = actual
            require(type(actual) is int and actual >= 3, 'Noop actions did not actually execute')
            require(any(hints), 'Ordinary CLI launch handshakes erased noProgress')
            require(all(h is None or (isinstance(h, dict) and h.get('retryAction') is False
                        and type(h.get('attempts')) is int and h['attempts'] >= 3)
                        for h in hints),
                    'Advisory requested an unsafe automatic replay')
            checks.append('ordinary_cli_progress_preserved')

            call(['open', url])
            call(['batch', 'click #increment', 'click #increment', 'click #increment', 'snapshot'])
            count = call(['eval', 'Number(document.querySelector("#count").textContent)']).get('result')
            require(count == 3, 'Batch postcondition counter is not 3')
            report['batchCounter'] = count
            checks.append('batch_postcondition')

            script = '\n'.join(["cu._call('click',{selector:'#noop',observe:true});"] * 5)
            script += "\nthrow new Error('synthetic failure');"
            failed = root / 'failed.js'
            failed.write_text(script)
            call(['open', url])
            call(['script', str(failed)], success=False)
            checks.append('failed_script_advisories')
            nested = root / 'nested.js'
            nested.write_text("cu._call('script',{source:" + json.dumps(script) + '});')
            call(['open', url])
            call(['script', str(nested)], success=False)
            checks.append('nested_script_advisories')
            report['verdict'] = 'PASS'
        except (AssertionError, OSError, TimeoutError, ValueError) as exc:
            report['error'] = str(exc)
        finally:
            if daemon is not None:
                try:
                    code, _, _ = invoke(base + ['close', '--json'], env, timeout)
                    report['cleanupExitCode'] = code
                except (OSError, TimeoutError):
                    report['cleanupExitCode'] = None
                kill_owned(daemon)
                if report.get('cleanupExitCode') != 0:
                    report['verdict'] = 'FAIL'
                    report.setdefault('error', 'Owned browser close did not succeed')
            server.shutdown()
            server.server_close()
            worker.join(timeout=2)
    output.write_text(json.dumps(report, indent=2) + '\n')
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--sha256', required=True, help='Expected build receipt or release checksum')
    parser.add_argument('--version', required=True, help='Exact expected version, without v prefix')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--timeout', type=float, default=60)
    args = parser.parse_args()
    if args.timeout <= 0 or not args.timeout < float('inf'):
        parser.error('timeout must be finite and positive')
    if args.output.exists():
        parser.error('refusing to overwrite an existing report')
    report = acceptance(args.binary.resolve(), args.sha256, args.version, args.output, args.timeout)
    print(json.dumps({key: report.get(key) for key in ('verdict', 'error', 'sha256', 'version', 'checks')}))
    return 0 if report['verdict'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
