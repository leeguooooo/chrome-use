#!/usr/bin/env python3
"""CI-only real Bitwarden locked-menu diagnostic. Never uses a real vault."""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'windows-capture-798'))
from probe import Registry, Windows, checked, digest, execute, wait_for
from owned_processes import OwnedProcessHandles, cleanup_acknowledged, descendants, processes


def worker(endpoint, expression):
    result = subprocess.run(['node', str(Path(__file__).with_name('worker-eval.mjs'))],
        input=json.dumps({'endpoint': endpoint, 'expression': expression}),
        capture_output=True, text=True, timeout=12, check=True)
    return json.loads(result.stdout)


def discover_workers(port):
    with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/list', timeout=3) as response:
        targets = json.load(response)
    result = {}
    for target in targets:
        if target.get('type') != 'service_worker' or not target.get('url', '').startswith('chrome-extension://'):
            continue
        endpoint = target['webSocketDebuggerUrl']
        identity = worker(endpoint, '({id:chrome.runtime.id,version:chrome.runtime.getManifest().version,name:chrome.runtime.getManifest().name})')
        if identity['version'] == '2026.10.1':
            result['bitwarden'] = (endpoint, identity)
        elif identity['name'] == 'chrome-use':
            result['chromeUse'] = (endpoint, identity)
    return result


def seed_locked_fixture(endpoint):
    user = '78000000-0000-4000-8000-000000000780'
    # A local UI fixture token, never accepted by any server. DNS for external
    # services is disabled for this entire disposable Chrome process.
    encode = lambda value: base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip('=')
    token = encode({'alg':'none','typ':'JWT'}) + '.' + encode({'sub':user,'email':'fixture@example.invalid','exp':4102444800}) + '.fixture-not-a-signature'
    seed = {'global_account_accounts':{user:{'email':'fixture@example.invalid','emailVerified':True,'name':'Disposable fixture'}},
        'global_account_activeAccountId':user, 'global_autofillSettingsLocal_inlineMenuVisibility':2,
        f'user_{user}_token_accessToken':token}
    expression = '(async()=>{await chrome.storage.local.set(' + json.dumps(seed) + ');return {syntheticAccount:true,version:chrome.runtime.getManifest().version};})()'
    return worker(endpoint, expression)


class FormFixture:
    def __init__(self, case):
        self.events = []
        self.lock = threading.Lock()
        owner = self
        html = '''<!doctype html><title>BitwardenFixture CASE</title>
        <form id="fixture"><label>Email<input id="email" name="email" type="email" autocomplete="username"></label>
        <label>Password<input id="password" name="password" type="password" autocomplete="current-password"></label>
        <button type="submit">Do not submit</button></form>
        <script>
        const recordedEvents=[];
        window.fixtureState=()=>({caseId:'CASE',events:recordedEvents.slice()});
        for(const type of ['focus','blur','keydown','keyup','visibilitychange','submit'])
          document.addEventListener(type,event=>{
            if(type==='submit')event.preventDefault();
            const observed={type,trusted:event.isTrusted,key:event.key||null,
              visibility:document.visibilityState,atMs:performance.now()};
            recordedEvents.push(observed);
            fetch('/event',{method:'POST',body:JSON.stringify(observed)});
          },true);
        </script>'''.replace('CASE', case).encode()
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_GET(self):
                self.send_response(200); self.send_header('Content-Type','text/html'); self.end_headers(); self.wfile.write(html)
            def do_POST(self):
                size = int(self.headers.get('Content-Length','0'))
                if self.path != '/event' or not 0 < size < 4096:
                    self.send_error(400); return
                event = json.loads(self.rfile.read(size))
                with owner.lock: owner.events.append(event)
                self.send_response(204); self.end_headers()
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_port}/login?{case}'
        threading.Thread(target=self.server.serve_forever,daemon=True).start()
    def close(self):
        self.server.shutdown(); self.server.server_close()


def menu_frames(endpoint, fixture_url, bitwarden_id):
    expression = '''(async()=>{const matches=(await chrome.tabs.query({})).filter(t=>t.url===URL);
    if(matches.length!==1)throw new Error('fixture tab is not unique');
    const tab=matches[0],frames=await chrome.webNavigation.getAllFrames({tabId:tab.id});
    return {tabId:tab.id,windowId:tab.windowId,active:tab.active,
      bitwardenFrames:frames.filter(f=>f.url.startsWith('chrome-extension://'+BW+'/')).map(f=>({frameId:f.frameId,url:f.url}))};})()'''
    return worker(endpoint, expression.replace('URL',json.dumps(fixture_url)).replace('BW',json.dumps(bitwarden_id)))


def native_focus(windows, hwnd, pid, title):
    before = windows.state(hwnd)
    if before['pid'] != pid or title not in before['title'] or before['minimized']:
        raise RuntimeError('native input target not verified')
    user32 = ctypes.WinDLL('user32',use_last_error=True)
    user32.SetForegroundWindow.argtypes=[wintypes.HWND];user32.SetForegroundWindow.restype=wintypes.BOOL
    user32.SetForegroundWindow(hwnd)
    focused = wait_for(lambda: windows.state(hwnd), lambda w:w['foreground']==hwnd, 2)
    if focused['visible'] is not True:raise RuntimeError('foreground fixture window is not visible')
    return before, focused


def send_escape(hwnd):
    extra = ctypes.c_size_t
    class Mouse(ctypes.Structure):
        _fields_=[('dx',wintypes.LONG),('dy',wintypes.LONG),('mouseData',wintypes.DWORD),('flags',wintypes.DWORD),('time',wintypes.DWORD),('extra',extra)]
    class Key(ctypes.Structure):
        _fields_=[('vk',wintypes.WORD),('scan',wintypes.WORD),('flags',wintypes.DWORD),('time',wintypes.DWORD),('extra',extra)]
    class Hardware(ctypes.Structure):
        _fields_=[('message',wintypes.DWORD),('low',wintypes.WORD),('high',wintypes.WORD)]
    class Payload(ctypes.Union): _fields_=[('mouse',Mouse),('key',Key),('hardware',Hardware)]
    class Input(ctypes.Structure): _fields_=[('kind',wintypes.DWORD),('payload',Payload)]
    assert ctypes.sizeof(Input)==40, 'x64 Windows INPUT ABI'
    user32=ctypes.WinDLL('user32',use_last_error=True)
    user32.GetForegroundWindow.restype=wintypes.HWND
    if user32.GetForegroundWindow()!=hwnd: raise RuntimeError('foreground changed; no input sent')
    user32.GetAsyncKeyState.argtypes=[ctypes.c_int];user32.GetAsyncKeyState.restype=wintypes.SHORT
    if any(user32.GetAsyncKeyState(key)&0x8000 for key in [16,17,18,91,92]):
        raise RuntimeError('modifier key is held; no native Escape sent')
    user32.SendInput.argtypes=[wintypes.UINT,ctypes.POINTER(Input),ctypes.c_int];user32.SendInput.restype=wintypes.UINT
    inputs=(Input*2)(Input(1,Payload(key=Key(27,1,0,0,0))),Input(1,Payload(key=Key(27,1,2,0,0))))
    count=user32.SendInput(2,inputs,ctypes.sizeof(Input))
    if count!=2: raise RuntimeError(f'unknown native input outcome: {count}/2 events admitted; never replay')
    return {'admitted':count,'replayed':False}


def terminate_owned(chrome):
    owned=descendants(processes(),chrome.pid)
    handles=OwnedProcessHandles(owned)
    try:
        killed=subprocess.run(['taskkill','/PID',str(chrome.pid),'/T','/F'],capture_output=True,text=True,timeout=20)
        parent=chrome.wait(timeout=20)
        exits=handles.observe()
    finally:
        handles.close()
    after=processes();post=descendants(after,chrome.pid,owned)
    result={'before':owned,'taskkill':{'exit':killed.returncode,'stdout':killed.stdout,'stderr':killed.stderr},'parentExit':parent,'ownedHandleExits':exits,'postTree':post}
    result['verified']=cleanup_acknowledged(result['taskkill'],owned,exits,parent,post)
    return result


def main():
    if platform.system()!='Windows' or os.environ.get('GITHUB_ACTIONS')!='true':
        raise SystemExit('Only a disposable Windows GitHub runner is allowed')
    parser=argparse.ArgumentParser()
    for name in ['binary','chrome','extension','bitwarden','output']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=True)
    case='bw780-'+uuid.uuid4().hex[:10]
    fixture=FormFixture(case);windows=Windows();record={'caseId':case,'verdict':'INCONCLUSIVE',
        'scope':'Windows CI + official Bitwarden 2026.10.1, seeded locked UI fixture; no real account or vault',
        'binarySha256':digest(args.binary),'chromeSha256':digest(args.chrome),
        'bitwardenBackgroundSha256':digest(args.bitwarden/'background.js'),
        'sourceExtensionBackgroundSha256':digest(args.extension/'background.js'),
        'sourceGitSha':os.environ.get('GITHUB_SHA'),
        'realVaultTested':False,'productionRecoveryImplemented':False}
    try:
        with tempfile.TemporaryDirectory(prefix='chrome780-') as tmp:
            root=Path(tmp);extension=root/'chrome-use';shutil.copytree(args.extension,extension)
            manifest_path=extension/'manifest.json';manifest=json.loads(manifest_path.read_text())
            # Equivalent to granting only our disposable fixture site access.
            manifest.setdefault('host_permissions',[]).append('http://127.0.0.1/*')
            manifest_path.write_text(json.dumps(manifest,indent=2))
            record['fixtureManifestHostPermission']='http://127.0.0.1/*'
            record['sourceExtensionVersion']=manifest['version']
            key=hashlib.sha256(base64.b64decode(manifest['key'])).hexdigest()[:32]
            extension_id=''.join(chr(ord('a')+int(c,16)) for c in key)
            profile=root/'profile';relay=root/'relay';relay.mkdir()
            env=dict(os.environ,CHROME_USE_RELAY_DIR=str(relay),AGENT_BROWSER_SOCKET_DIR=str(root/'socket'),
                CHROME_USE_NO_UPDATE_CHECK='1',AGENT_BROWSER_AUTO_LOGIN='off')
            with Registry(args.binary,root,extension_id),(args.output/'chrome.log').open('w') as log:
                chrome=subprocess.Popen([str(args.chrome),'--no-first-run','--no-default-browser-check',
                    '--user-data-dir='+str(profile),'--remote-debugging-port=0',
                    '--load-extension='+str(extension)+','+str(args.bitwarden),
                    '--disable-extensions-except='+str(extension)+','+str(args.bitwarden),
                    '--disable-background-networking','--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1',
                    '--window-size=1280,800','about:blank'],env=env,stdout=log,stderr=log)
                record['chromePid']=chrome.pid;common=None
                try:
                    active_port=wait_for(lambda: profile/'DevToolsActivePort',lambda p:p.exists(),15)
                    port=int(active_port.read_text().splitlines()[0])
                    controls=wait_for(lambda:discover_workers(port),lambda x:set(x)=={'chromeUse','bitwarden'},30)
                    bw_endpoint,bw_identity=controls['bitwarden'];our_endpoint,our_identity=controls['chromeUse']
                    record['extensions']={'bitwarden':bw_identity,'chromeUse':our_identity}
                    record['seed']=seed_locked_fixture(bw_endpoint)
                    record['stealth']=worker(our_endpoint,'(async()=>{await chrome.storage.local.set({ab_stealth:true});return (await chrome.storage.local.get("ab_stealth")).ab_stealth;})()')
                    files=wait_for(lambda:list(relay.glob('relay-cdp-url-*')),lambda f:len(f)==1,30)
                    browser=files[0].name.removeprefix('relay-cdp-url-')
                    common=[str(args.binary),'--session',case,'--browser',browser,'--window','dedicated','--json']
                    invoke=lambda name,*words:execute(common+list(words),env,args.output/name,timeout=40)
                    record['open']=invoke('open','open',fixture.url)
                    opened=checked(record['open'])
                    if opened.get('url')!=fixture.url:raise RuntimeError('fixture URL not reached: '+str(opened.get('url')))
                    hwnd=wait_for(lambda:windows.find(chrome.pid,'BitwardenFixture '+case),lambda x:x is not None,5)
                    record['windowBefore']=windows.state(hwnd)
                    # One focus action only. A failure may mean focus took effect:
                    # never replay it in search of a successful acknowledgement.
                    record['focus']=invoke('focus','click','#email')
                    record['initialMenu']=wait_for(lambda:menu_frames(our_endpoint,fixture.url,bw_identity['id']),
                        lambda x:len(x['bitwardenFrames'])>0,8)
                    record['accessBefore']=invoke('access-before','eval','document.title')
                    menu=menu_frames(our_endpoint,fixture.url,bw_identity['id']);record['menuAfterExistingGuard']=menu
                    if not menu['bitwardenFrames']:
                        record['mechanism']='menu-absent-after-cli-read'
                    else:
                        response=record['accessBefore'].get('response') or {}
                        if 'debugger_access_denied' not in str(response.get('error','')):
                            raise RuntimeError('real menu exists, but protected-access failure was not reproduced')
                        if menu['active'] is not True:raise RuntimeError('native candidate requires the bound fixture tab already active')
                        before,focused=native_focus(windows,hwnd,chrome.pid,'BitwardenFixture '+case)
                        record['nativeFocus']={'before':before,'after':focused}
                        record['menuAfterNativeFocus']=menu_frames(our_endpoint,fixture.url,bw_identity['id'])
                        if record['menuAfterNativeFocus']['bitwardenFrames']:
                            record['nativeEscape']=send_escape(hwnd);record['mechanism']='menu-absent-after-native-escape'
                        else:record['mechanism']='menu-absent-after-native-focus'
                        record['menuAfterNative']=wait_for(lambda:menu_frames(our_endpoint,fixture.url,bw_identity['id']),
                            lambda x:not x['bitwardenFrames'],4)
                    record['accessAfter']=invoke('access-after','eval','({title:document.title,fixture:window.fixtureState()})')
                    after_data=checked(record['accessAfter'])
                    observed=after_data.get('result') or {}
                    if observed.get('title') != 'BitwardenFixture '+case or (observed.get('fixture') or {}).get('caseId')!=case:
                        raise RuntimeError('recovery read answered from a different page')
                    record['finalMenu']=menu_frames(our_endpoint,fixture.url,bw_identity['id'])
                    with fixture.lock:record['receivedFixtureEvents']=list(fixture.events)
                    record['fixtureEvents']=observed['fixture'].get('events')
                    if not isinstance(record['fixtureEvents'],list):raise RuntimeError('fixture history missing')
                    if record['finalMenu']['bitwardenFrames']:raise RuntimeError('menu reopened after recovery read')
                    if any(e['type']=='submit' or (e['type']=='visibilitychange' and e['trusted'] is not True) for e in record['fixtureEvents']):
                        raise RuntimeError('unexpected submission or synthetic visibility event')
                    record['verdict']='PASS'
                except Exception as exc:
                    record['error']=type(exc).__name__+': '+str(exc)[:1200]
                finally:
                    if common:
                        try:record['sessionCleanup']=execute(common+['close'],env,args.output/'close',timeout=20)
                        except Exception as exc:record['sessionCleanupError']=str(exc)[:300]
                    try:
                        if chrome.poll() is not None:raise RuntimeError('owned root exited before cleanup identity observation')
                        record['browserCleanup']=terminate_owned(chrome)
                        if record['browserCleanup']['verified'] is not True:record['verdict']='INCONCLUSIVE'
                    except Exception as exc:
                        record['cleanupError']=str(exc)[:300];record['verdict']='INCONCLUSIVE'
                        if chrome.poll() is None:chrome.kill();chrome.wait(timeout=10)
    finally:
        fixture.close()
        (args.output/'summary.json').write_text(json.dumps(record,indent=2),encoding='utf-8')
    print(json.dumps({k:record.get(k) for k in ['verdict','mechanism','error','cleanupError']}))
    return 0 if record['verdict']=='PASS' else 2


if __name__=='__main__':sys.exit(main())
