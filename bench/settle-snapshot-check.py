#!/usr/bin/env python3
"""Measure warm snapshot settling with a receipt-verified release CLI.

Run on the configured SSH build host, never the maintainer's working desktop.
Creates and closes its own headed, empty --launch profiles. No personal Chrome
profile or vault is used. --compare-binary alternates baseline/candidate captures
on separately owned profiles; each target is activated outside the timed call.
"""
import argparse, hashlib, http.server, json, os, pathlib, platform, re, statistics, subprocess, threading, time, uuid
p = argparse.ArgumentParser()
p.add_argument('binary')
p.add_argument('--receipt', required=True)
p.add_argument('--output', required=True)
p.add_argument('--compare-binary')
p.add_argument('--compare-receipt')
a = p.parse_args()
assert not pathlib.Path(a.output).exists(), 'output already exists'
assert bool(a.compare_binary) == bool(a.compare_receipt), 'supply both comparison arguments'
binary = pathlib.Path(a.binary).resolve()
receipt = json.loads(pathlib.Path(a.receipt).read_text())
assert receipt['artifact']['sha256'] == hashlib.sha256(binary.read_bytes()).hexdigest(), 'artifact differs from build receipt'
assert '--release' in receipt['cargo_args'], 'benchmark needs a release build'
page = '<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><h1>Snapshot settle fixture</h1><button>Stable action</button><a href="#target">Stable link</a><div id="noise" hidden></div><script>window.fixtureTicks=0;if(location.pathname.includes(\'noise\'))setInterval(()=>{fixtureTicks++;noise.dataset.tick=String(performance.now())},16)</script>'

class Handler(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.end_headers()
        self.wfile.write(page.encode())
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
session = 'settle-769-' + uuid.uuid4().hex[:8]
env = os.environ.copy()
env.update(CHROME_USE_NO_UPDATE_CHECK='1', AGENT_BROWSER_AUTO_LOGIN='off')
sides = [{'name': 'baseline', 'binary': binary, 'session': session, 'receipt': receipt}]
if a.compare_binary:
    other = pathlib.Path(a.compare_binary).resolve()
    other_receipt = json.loads(pathlib.Path(a.compare_receipt).read_text())
    assert other_receipt['artifact']['sha256'] == hashlib.sha256(other.read_bytes()).hexdigest(), 'comparison artifact differs from receipt'
    assert '--release' in other_receipt['cargo_args'], 'comparison needs a release build'
    sides.append({'name': 'candidate', 'binary': other, 'session': 'settle-769-' + uuid.uuid4().hex[:8], 'receipt': other_receipt})
side = sides[0]
report = {'sources': [{**x, 'binary': str(x['binary'])} for x in sides], 'mode': 'headed isolated launch', 'machine': {'host': platform.node(), 'os': platform.platform(), 'cores': os.cpu_count(), 'loadBefore': os.getloadavg()}, 'samples': [], 'failures': []}

def select_side(chosen):
    global binary, session, side
    side = chosen
    binary = chosen['binary']
    session = chosen['session']

def call(*args):
    start = time.monotonic()
    r = subprocess.run([str(binary), '--session', session, '--json', *args], env=env, text=True, capture_output=True, timeout=90)
    elapsed = (time.monotonic() - start) * 1000
    try:
        out = json.loads(r.stdout)
    except ValueError:
        out = {'success': False, 'error': 'non-JSON response: ' + r.stdout[:200]}
    if r.returncode or not out.get('success'):
        report['failures'].append({'side': side['name'], 'command': list(args), 'exit': r.returncode, 'error': out.get('error'), 'stderr': r.stderr[:300]})
        raise RuntimeError(report['failures'][-1])
    return (out.get('data') or {}, elapsed, len(r.stdout.encode()) + len(r.stderr.encode()))

def sample(label, no_settle, timed=True):
    (data, ms, bytes_) = call('snapshot', '-i', *(['--no-settle'] if no_settle else []))
    tree = data.get('snapshot', '')
    assert isinstance(tree, str) and tree, 'snapshot was empty'
    if label == 'github':
        assert 'chrome-use' in tree and len(tree) > 1000, 'GitHub repository content absent'
    else:
        assert 'Stable action' in tree and 'Stable link' in tree, 'fixture controls absent'
    canonical = re.sub('\\[ref=[^\\]]+\\]', '[ref]', tree)
    row = {'side': side['name'], 'page': label, 'noSettle': no_settle, 'ms': round(ms, 2), 'responseBytes': bytes_, 'treeBytes': len(tree.encode()), 'treeHash': hashlib.sha256(canonical.encode()).hexdigest(), 'settle': data.get('settle')}
    if timed:
        report['samples'].append(row)
try:
    for chosen in sides:
        select_side(chosen)
        chosen['started'] = True
        call('--launch', 'open', 'http://127.0.0.1:' + str(server.server_port) + '/quiet')
        chosen['version'] = subprocess.check_output([str(binary), '--version'], text=True, env=env).strip()
        chosen['ua'] = call('eval', 'navigator.userAgent')[0].get('result')
        tabs = call('tab', 'list')[0]['tabs']
        assert len(tabs) == 1, 'unexpected tabs in test profile'
        chosen['tab'] = str(tabs[0]['tabId'])
        if chosen['tab'].isdigit():
            chosen['tab'] = 't' + chosen['tab']
    report['machine']['compilerPidsBefore'] = {name: subprocess.run(['pgrep', '-x', name], capture_output=True, text=True).stdout.split() for name in ['cargo', 'rustc']}
    for (label, url) in [('fixture-quiet', 'http://127.0.0.1:' + str(server.server_port) + '/quiet'), ('fixture-hidden-mutation', 'http://127.0.0.1:' + str(server.server_port) + '/noise'), ('github', 'https://github.com/leeguooooo/chrome-use')]:
        ticks_before = {}
        for chosen in sides:
            select_side(chosen)
            call('tab', 'select', chosen['tab'], '--activate')
            call('open', url)
            sample(label, False, False)
            sample(label, True, False)
            if label.startswith('fixture'):
                ticks_before[chosen['name']] = call('eval', '({ticks:fixtureTicks,visibility:document.visibilityState})')[0].get('result')
        for _ in range(5):
            for chosen in sides:
                select_side(chosen)
                call('tab', 'select', chosen['tab'], '--activate')
                sample(label, False)
                sample(label, True)
        if label.startswith('fixture'):
            for chosen in sides:
                select_side(chosen)
                ticks_after = call('eval', '({ticks:fixtureTicks,visibility:document.visibilityState})')[0].get('result')
                before = ticks_before[chosen['name']]
                report.setdefault('fixtureActivity', {})[label + '-' + chosen['name']] = {'before': before, 'after': ticks_after}
                if label == 'fixture-hidden-mutation':
                    assert ticks_after['ticks'] > before['ticks'] > 0, 'mutation timer never ran'
    report['summary'] = []
    for label in sorted({x['page'] for x in report['samples']}):
        rows = [x for x in report['samples'] if x['page'] == label]
        for chosen in sides:
            for mode in [False, True]:
                r = [x for x in rows if x['noSettle'] == mode and x['side'] == chosen['name']]
                v = [x['ms'] for x in r]
                report['summary'].append({'side': chosen['name'], 'page': label, 'noSettle': mode, 'n': len(r), 'medianMs': statistics.median(v), 'rangeMs': [min(v), max(v)], 'pending': [x['settle'].get('pending') if x['settle'] else None for x in r]})
        assert len({x['treeHash'] for x in rows}) == 1, 'snapshot output changed across samples for ' + label
    report['verdict'] = 'BASELINE_CAPTURED'
except Exception as e:
    report['verdict'] = 'INCONCLUSIVE'
    report['error'] = str(e)
finally:
    for chosen in sides:
        if not chosen.get('started'):
            continue
        try:
            select_side(chosen)
            call('close')
        except Exception as e:
            report['cleanupError'] = str(e)
            report['verdict'] = 'INCONCLUSIVE'
    report['sources'] = [{**x, 'binary': str(x['binary'])} for x in sides]
    server.shutdown()
    report['machine']['loadAfter'] = os.getloadavg()
    report['machine']['compilerPidsAfter'] = {name: subprocess.run(['pgrep', '-x', name], capture_output=True, text=True).stdout.split() for name in ['cargo', 'rustc']}
    pathlib.Path(a.output).write_text(json.dumps(report, indent=2))
print(json.dumps({k: report.get(k) for k in ['verdict', 'summary', 'error', 'cleanupError']}, indent=2))
raise SystemExit(0 if report['verdict'] == 'BASELINE_CAPTURED' else 1)
