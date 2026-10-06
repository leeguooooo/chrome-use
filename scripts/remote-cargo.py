#!/usr/bin/env python3
"""Run repository Cargo checks on a configured SSH host without local compilation.

Only Git-listed working-tree files (including intent-to-add) are uploaded.
Each invocation gets its own source snapshot, shares a serialized Cargo cache,
checks source hashes before/after execution, and verifies downloaded artifacts.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
import uuid

REMOTE_RUNNER = r'''
import fcntl, hashlib, json, os, platform, shutil, signal, subprocess, sys, tarfile, threading, time
from pathlib import Path
cfg=json.loads(sys.argv[1])
root=Path.home()/cfg['remote_root'];root.mkdir(parents=True,exist_ok=True,mode=0o700)
job=root/'jobs'/cfg['job'];src=job/'src';src.mkdir(parents=True,mode=0o700)
if shutil.disk_usage(root).free<cfg['min_free_gib']*1024**3:
 raise SystemExit('Remote disk space below configured minimum; local fallback is disabled')
received=set()
with tarfile.open(fileobj=sys.stdin.buffer,mode='r|gz') as archive:
 for member in archive:
  name=Path(member.name)
  if name.is_absolute() or '..' in name.parts or member.isdev() or member.islnk():
   raise SystemExit('Unsafe source archive member')
  if member.issym():
   target=(src/name).parent/member.linkname
   if not target.resolve().is_relative_to(src.resolve()):raise SystemExit('Source symlink escapes snapshot')
  archive.extract(member,src);received.add(member.name)
manifest=json.loads((src/'.chrome-use-build-manifest.json').read_text())
if received!={row['path'] for row in manifest['files']}|{'.chrome-use-build-manifest.json'}:
 raise SystemExit('Archive entries do not match source manifest')
def verify_source():
 for row in manifest['files']:
  p=src/row['path'];data=os.readlink(p).encode() if row['kind']=='symlink' else p.read_bytes()
  if hashlib.sha256(data).hexdigest()!=row['sha256']:raise RuntimeError('Source changed: '+row['path'])
verify_source()
canonical=json.dumps(manifest['files'],sort_keys=True,separators=(',',':')).encode()
if hashlib.sha256(canonical).hexdigest()!=cfg['source_hash']:raise SystemExit('Source manifest hash mismatch')
cache=root/'target';cache.mkdir(exist_ok=True)
with (root/'cargo.lock').open('a') as lock:
 print('Remote source '+cfg['source_hash'][:12]+' verified; waiting for Cargo cache lock',flush=True)
 fcntl.flock(lock,fcntl.LOCK_EX)
 # Cargo embeds its manifest directory; macOS Unix sockets cannot use a long
 # checkout prefix. Keep source paths short and stable for incremental reuse.
 short_root=Path('/tmp')/('cu190-'+str(os.getuid()))
 short_root.mkdir(exist_ok=True,mode=0o700)
 if short_root.is_symlink() or short_root.stat().st_uid!=os.getuid():raise SystemExit('Unsafe short source root')
 short_src=short_root/cfg['source_hash'][:16]
 uploaded_src=src
 if short_src.exists():
  src=short_src;verify_source()
  shutil.rmtree(uploaded_src)  # This invocation's verified archive only.
 else:
  shutil.move(str(uploaded_src),str(short_src));src=short_src
 verify_source()
 env=os.environ.copy();env['PATH']='/opt/homebrew/bin:'+str(Path.home()/'.cargo/bin')+':/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'
 env['CARGO_TARGET_DIR']=str(cache);env['CARGO_PROFILE_DEV_DEBUG']='0';env['CARGO_PROFILE_TEST_DEBUG']='0'
 cargo=shutil.which('cargo',path=env['PATH'])
 if not cargo:raise SystemExit('Cargo is missing on remote host; local fallback is disabled')
 args=cfg['cargo_args'];kind=args[0];cmd=[cargo,*args]
 if kind in ('build','test','clippy'):
  cmd[2:2]=['--jobs',str(cfg['jobs'])]
  if '--locked' not in args:cmd.insert(2,'--locked')
 if cfg['headless']:env['AGENT_BROWSER_ALLOW_HEADLESS']='1'
 start=time.time();abort=[]
 print('Remote host '+platform.node()+' '+platform.machine()+'; running '+ ' '.join(cmd),flush=True)
 proc=subprocess.Popen(cmd,cwd=src/'cli',env=env,start_new_session=True)
 def interrupted(signum,frame):
  try:os.killpg(proc.pid,signal.SIGTERM)
  except ProcessLookupError:pass
  raise SystemExit(128+signum)
 for sig in (signal.SIGHUP,signal.SIGTERM,signal.SIGINT):signal.signal(sig,interrupted)
 def monitor():
  while proc.poll() is None:
   reason=None
   if shutil.disk_usage(root).free<1024**3:reason='Remote free space dropped below 1 GiB'
   if time.time()-start>cfg['timeout']:reason='Remote Cargo command timed out'
   if reason:
    abort.append(reason)
    try:os.killpg(proc.pid,signal.SIGTERM)
    except ProcessLookupError:pass
    return
   time.sleep(2)
 threading.Thread(target=monitor,daemon=True).start()
 code=proc.wait();verify_source()
 receipt={'source_hash':cfg['source_hash'],'source_commit':manifest['commit'],'host':platform.node(),'system':platform.system(),'arch':platform.machine(),'cargo_version':subprocess.check_output([cargo,'--version'],env=env,text=True).strip(),'cargo_args':args,'returncode':code,'duration_seconds':round(time.time()-start,2),'job':cfg['job'],'source_directory':str(src),'artifact':None}
 if abort:receipt['error']=abort[0]
 if code==0 and kind=='build':
  profile='release' if '--release' in args else 'debug';target=None
  for i,arg in enumerate(args):
   if arg=='--profile':profile=args[i+1]
   if arg.startswith('--profile='):profile=arg.split('=',1)[1]
   if arg=='--target':target=args[i+1]
   if arg.startswith('--target='):target=arg.split('=',1)[1]
  binary=cache
  if target:binary/=target
  binary=binary/profile/('chrome-use.exe' if target and 'windows' in target else 'chrome-use')
  if not binary.is_file():raise SystemExit('Cargo succeeded without requested CLI artifact')
  dest=job/binary.name;shutil.copy2(binary,dest)
  receipt['artifact']={'path':str(dest),'sha256':hashlib.sha256(dest.read_bytes()).hexdigest(),'bytes':dest.stat().st_size}
 (job/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
 print('CHROME_USE_REMOTE_RESULT '+json.dumps(receipt),flush=True)
 sys.exit(0 if code==0 else 1)
'''


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def snapshot(root, destination):
    listed = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-z']).decode().split('\0')
    files = []
    data_files = []
    for name in sorted(x for x in listed if x):
        relative = PurePosixPath(name)
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('Invalid Git source path')
        p = root / name
        if not p.exists() and not p.is_symlink():
            continue  # A tracked working-tree deletion is part of the snapshot.
        if p.is_symlink():
            target = os.readlink(p)
            if not (p.parent / target).resolve().is_relative_to(root.resolve()):
                raise ValueError('Source symlink escapes checkout: ' + name)
            payload = target.encode()
            kind, mode = 'symlink', 0o777
        elif p.is_file():
            payload = p.read_bytes()
            kind, mode = 'file', 0o755 if p.stat().st_mode & 0o111 else 0o644
        else:
            raise ValueError('Source path is not a file: ' + name)
        row = {'path': name, 'kind': kind, 'mode': mode, 'sha256': hashlib.sha256(payload).hexdigest()}
        files.append(row)
        data_files.append((row, payload))
    source_hash = hashlib.sha256(json.dumps(files, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    manifest = {'files': files, 'commit': git(root, 'rev-parse', 'HEAD')}
    with tarfile.open(destination, 'w:gz', compresslevel=1) as archive:
        for row, payload in data_files:
            info = tarfile.TarInfo(row['path']);info.mode = row['mode'];info.mtime = 0
            if row['kind'] == 'symlink':
                info.type = tarfile.SYMTYPE;info.linkname = payload.decode()
                archive.addfile(info)
            else:
                info.size = len(payload);archive.addfile(info, io.BytesIO(payload))
        payload = json.dumps(manifest).encode()
        info = tarfile.TarInfo('.chrome-use-build-manifest.json');info.mode = 0o600;info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    return source_hash, manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host', help='SSH alias; defaults to CHROME_USE_BUILD_HOST or git chromeuse.remoteHost')
    parser.add_argument('--remote-root', default='.cache/chrome-use-remote')
    parser.add_argument('--jobs', type=int, default=4)
    parser.add_argument('--timeout', type=int, default=1800)
    parser.add_argument('--min-free-gib', type=float, default=2)
    parser.add_argument('--headless', action='store_true', help='Permit isolated browser-backed tests on the remote host')
    parser.add_argument('--output', type=Path, help='Verified build artifact destination')
    parser.add_argument('cargo_args', nargs=argparse.REMAINDER)
    opts = parser.parse_args()
    root = Path(git(Path.cwd(), 'rev-parse', '--show-toplevel'))
    host = opts.host or os.environ.get('CHROME_USE_BUILD_HOST')
    if not host:
        host = subprocess.run(['git', '-C', str(root), 'config', '--get', 'chromeuse.remoteHost'], capture_output=True, text=True).stdout.strip()
    if not host or host.startswith('-') or not re.fullmatch(r'[A-Za-z0-9_.@-]+', host):
        parser.error('Configure an SSH alias with git config --local chromeuse.remoteHost <alias>; local fallback is disabled')
    if not re.fullmatch(r'[A-Za-z0-9_./-]+', opts.remote_root) or '..' in PurePosixPath(opts.remote_root).parts or opts.remote_root.startswith('/') or opts.remote_root in ('.', ''):
        parser.error('--remote-root must be a safe path relative to remote home')
    args = opts.cargo_args
    if args and args[0] == '--':args = args[1:]
    if not args or args[0] not in ('build', 'test', 'fmt', 'clippy'):
        parser.error('Pass build, test, fmt or clippy and Cargo arguments')
    if args[0] == 'fmt' and '--check' not in args:
        parser.error('Remote formatting only supports --check; source edits stay in the local checkout')
    if any(a == '--target-dir' or a.startswith('--target-dir=') for a in args):
        parser.error('The remote target cache is managed by this runner')
    if not 1 <= opts.jobs <= 16 or opts.timeout <= 0 or opts.min_free_gib < 1:
        parser.error('Invalid jobs, timeout or disk reserve')
    job = uuid.uuid4().hex
    ssh = ['ssh', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ConnectTimeout=10', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=4', host]
    with tempfile.TemporaryDirectory(prefix='chrome-use-remote-') as td:
        archive = Path(td) / 'source.tar.gz'
        source_hash, manifest = snapshot(root, archive)
        config = {'job': job, 'remote_root': opts.remote_root, 'source_hash': source_hash, 'cargo_args': args, 'jobs': opts.jobs, 'timeout': opts.timeout, 'min_free_gib': opts.min_free_gib, 'headless': opts.headless}
        command = 'export PATH=/opt/homebrew/bin:$PATH; python3 -u -c ' + shlex.quote(REMOTE_RUNNER) + ' ' + shlex.quote(json.dumps(config))
        print('Uploading Git-listed source snapshot ' + source_hash[:12] + ' to ' + host, flush=True)
        receipt = None
        with archive.open('rb') as data:
            proc = subprocess.Popen(ssh + [command], stdin=data, stdout=subprocess.PIPE, text=True)
            for line in proc.stdout:
                if line.startswith('CHROME_USE_REMOTE_RESULT '):
                    receipt = json.loads(line.split(' ', 1)[1])
                else:
                    print(line, end='', flush=True)
            status = proc.wait()
        if not receipt:
            raise RuntimeError('Remote job returned no receipt; local fallback is disabled (SSH exit ' + str(status) + ')')
        if receipt['source_hash'] != source_hash or receipt['source_commit'] != manifest['commit']:
            raise RuntimeError('Remote receipt does not match uploaded source')
        if status or receipt['returncode']:
            print(json.dumps(receipt, indent=2), file=sys.stderr)
            return 1
        if receipt['artifact']:
            if receipt['system'] != platform.system() or receipt['arch'] != platform.machine():
                raise RuntimeError('Remote native artifact platform differs from this machine')
            dest = opts.output or root / 'cli/target/remote-builds' / job / 'chrome-use'
            if not dest.is_absolute():dest = root / dest
            dest.parent.mkdir(parents=True, exist_ok=True)
            temporary = dest.with_name(dest.name + '.' + job + '.remote-download')
            artifact = receipt['artifact']
            subprocess.run(['scp', '-q', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', host + ':' + artifact['path'], str(temporary)], check=True)
            if hashlib.sha256(temporary.read_bytes()).hexdigest() != artifact['sha256']:
                temporary.unlink(missing_ok=True)
                raise RuntimeError('Downloaded artifact checksum mismatch')
            temporary.chmod(0o755);temporary.replace(dest)
            receipt['artifact']['local_path'] = str(dest)
        receipts = root / 'cli/target/remote-build-receipts';receipts.mkdir(parents=True, exist_ok=True)
        receipt_path = receipts / (job + '.json');receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
        print('Remote Cargo succeeded; receipt: ' + str(receipt_path), flush=True)
        if receipt['artifact']:print('Verified artifact: ' + receipt['artifact']['local_path'], flush=True)
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as exc:
        print('Remote Cargo failed: ' + str(exc), file=sys.stderr)
        sys.exit(1)
