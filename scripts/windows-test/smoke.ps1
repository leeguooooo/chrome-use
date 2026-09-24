# Functional smoke test for chrome-use on Windows.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File smoke.ps1 [-Exe <path>] [-SkipDoctor]
#
# Drives a throwaway browser (`--launch`, an isolated profile) through the
# everyday commands and checks each one against the page itself rather than
# trusting exit codes. Needs Chrome installed; needs no network (the test page
# is a data: URL). Leaves nothing behind: the session is closed and its daemon
# must exit. Exit code 0 when every step passes, 1 otherwise.
#
# -Exe        the binary to test (default: `chrome-use` on PATH, else the
#             install.ps1 location)
# -SkipDoctor skip `doctor --quick --offline`. Releases up to v1.5.138 ran
#             `chrome.exe --version` there, which on Windows starts Chrome
#             against the user's real profile and never returns.
#
# Kept to ASCII: Windows PowerShell 5.1 reads a BOM-less script in the system
# code page, so a UTF-8 character can be misread on a non-English system.

param(
  [string]$Exe,
  [switch]$SkipDoctor
)

[Console]::OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference = 'Continue'

if (-not $Exe) {
  $cmd = Get-Command chrome-use -ErrorAction SilentlyContinue
  $Exe = if ($cmd) { $cmd.Source } else { Join-Path $env:LOCALAPPDATA 'Programs\chrome-use\chrome-use.exe' }
}
if (-not (Test-Path $Exe)) { Write-Host "chrome-use not found at $Exe (pass -Exe)"; exit 1 }

# Keep the machine awake while this process runs. A test machine that sleeps
# mid-run turns a pass into a timeout; this changes nothing after exit.
Add-Type -Name Power -Namespace CuSmoke -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
'@
[CuSmoke.Power]::SetThreadExecutionState([uint32]2147483649) | Out-Null  # ES_CONTINUOUS | ES_SYSTEM_REQUIRED

$env:CHROME_USE_NO_UPDATE_CHECK = '1'
$env:NO_COLOR = '1'
$Session = 'cu-smoke-' + [guid]::NewGuid().ToString('N').Substring(0, 6)
# A socket directory of our own: the daemon writes <session>.pid there, which is
# how this test finds the exact daemon it started (on Windows the daemon's
# command line carries no session name), and it keeps any real sessions on the
# machine out of the way.
$SockDir = Join-Path $env:TEMP "$Session-sock"
New-Item -ItemType Directory -Force -Path $SockDir | Out-Null
$env:AGENT_BROWSER_SOCKET_DIR = $SockDir
$results = New-Object System.Collections.Generic.List[object]

# Windows PowerShell 5.1 joins an -ArgumentList array with spaces and no quoting,
# so 'Casey Sample' would arrive as two arguments. Quote each one the way the
# MSVC runtime splits a command line.
function Quote([string]$s) {
  if ($s -eq '') { return '""' }
  if ($s -notmatch '[\s"]') { return $s }
  $r = '"'; $bs = 0
  foreach ($ch in $s.ToCharArray()) {
    if ($ch -eq '\') { $bs++ }
    elseif ($ch -eq '"') { $r += ('\' * ($bs * 2 + 1)) + '"'; $bs = 0 }
    else { $r += ('\' * $bs) + $ch; $bs = 0 }
  }
  $r + ('\' * ($bs * 2)) + '"'
}

function Run([string[]]$argv, [int]$timeoutSec = 60) {
  $o = Join-Path $env:TEMP "$Session-out.txt"
  $e = Join-Path $env:TEMP "$Session-err.txt"
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process $Exe -ArgumentList (($argv | ForEach-Object { Quote $_ }) -join ' ') `
    -NoNewWindow -PassThru -RedirectStandardOutput $o -RedirectStandardError $e
  $null = $p.Handle  # without this, ExitCode reads back empty
  $done = $p.WaitForExit($timeoutSec * 1000)
  if (-not $done) { Stop-Process $p -Force -ErrorAction SilentlyContinue }
  [pscustomobject]@{
    ok   = $done -and $p.ExitCode -eq 0
    code = if ($done) { $p.ExitCode } else { 'TIMEOUT' }
    ms   = $sw.ElapsedMilliseconds
    out  = ((Get-Content $o -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) + '').Trim()
    err  = ((Get-Content $e -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) + '').Trim()
  }
}

function Js([string]$expr) {
  $r = Run @('--session', $Session, 'eval', $expr, '--json')
  try { ($r.out | ConvertFrom-Json).data.result } catch { "EVAL-FAILED: $($r.err)" }
}

function Clip([string]$s, [int]$n = 90) {
  $s = ($s -replace '\s+', ' ').Trim()
  if ($s.Length -gt $n) { $s.Substring(0, $n) + '...' } else { $s }
}

function Check([string]$name, [bool]$pass, [string]$detail, $ms = '') {
  $results.Add([pscustomobject]@{ step = $name; pass = $pass; ms = $ms; detail = (Clip $detail) })
}

$page = 'data:text/html,' + [uri]::EscapeDataString(@'
<title>Smoke</title><h1>Smoke test</h1>
<label>Name <input id=name></label>
<label>Email <input id=email value=old@example.test></label>
<select id=plan><option>Free</option><option>Team</option></select>
<label><input type=checkbox id=agree> I agree</label>
<button id=go onclick="document.getElementById('out').textContent='Saved '+document.getElementById('name').value">Save</button>
<p id=out></p>
<div style="height:3000px"></div><p id=bottom>Bottom</p>
'@)

$r = Run @('--version')
Check 'version' $r.ok ($r.out -split "`n")[0] $r.ms

$r = Run @('--launch', '--session', $Session, 'open', $page) 120
Check 'open (--launch)' $r.ok (($r.out + ' ' + $r.err) -split "`n")[0] $r.ms
if (-not $r.ok) {
  $results | Format-Table -AutoSize -Wrap | Out-String -Width 200
  Write-Host 'Could not open a browser; the remaining steps would all fail. Is Chrome installed?'
  exit 1
}

$daemonPid = $null
$pidFile = Join-Path $SockDir "$Session.pid"
if (Test-Path $pidFile) { $daemonPid = [int]((Get-Content $pidFile -Raw).Trim()) }
Check 'daemon started and recorded its pid' ($daemonPid -and (Get-Process -Id $daemonPid -ErrorAction SilentlyContinue)) "pid=$daemonPid"

$r = Run @('--session', $Session, 'get', 'title')
Check 'get title' ($r.out -eq 'Smoke') $r.out $r.ms

$r = Run @('--session', $Session, 'snapshot', '-i')
Check 'snapshot -i' ($r.ok -and $r.out -match 'textbox') "$(($r.out -split "`n").Count) lines" $r.ms

$r = Run @('--session', $Session, 'fill', '#name', 'Casey Sample')
$v = Js "document.getElementById('name').value"
Check 'fill (value with a space)' ($v -eq 'Casey Sample') "value=$v" $r.ms

# Click, select-all, insert: the insert must replace, not append (v1.5.135).
$null = Run @('--session', $Session, 'click', '#email')
$r = Run @('--session', $Session, 'press', 'Control+a')
$null = Run @('--session', $Session, 'keyboard', 'inserttext', 'new@example.test')
$v = Js "document.getElementById('email').value"
Check 'Control+a then inserttext replaces' ($v -eq 'new@example.test') "value=$v" $r.ms

$r = Run @('--session', $Session, 'select', '#plan', 'Team')
$v = Js "document.getElementById('plan').value"
Check 'select' ($v -eq 'Team') "value=$v" $r.ms

$r = Run @('--session', $Session, 'check', '#agree')
$v = Js "document.getElementById('agree').checked"
Check 'check' ($v -eq $true) "checked=$v" $r.ms

$r = Run @('--session', $Session, 'click', '#go')
$v = Js "document.getElementById('out').textContent"
Check 'click a button' ($v -eq 'Saved Casey Sample') "text=$v" $r.ms

$r = Run @('--session', $Session, 'wait', '--text', 'Saved Casey', '--timeout', '5000')
Check 'wait --text (present)' $r.ok $r.out $r.ms

# A condition that never holds must not be diagnosed as a dead connection (v1.5.134).
$r = Run @('--session', $Session, 'wait', '--text', 'saved casey', '--timeout', '1500')
$said = $r.err + ' ' + $r.out
Check 'wait --text timeout: no reconnect advice' ((-not $r.ok) -and $said -notmatch 'Reconnect with') $said $r.ms

$r = Run @('--session', $Session, 'get', 'text', '#out')
Check 'get text' ($r.out -eq 'Saved Casey Sample') $r.out $r.ms

$r = Run @('--session', $Session, 'scroll', 'down', '2000')
$v = Js 'Math.round(scrollY)'
Check 'scroll' ([int]$v -gt 1000) "scrollY=$v" $r.ms

foreach ($mode in @('viewport', 'full')) {
  $png = Join-Path $env:TEMP "$Session-$mode.png"
  Remove-Item $png -ErrorAction SilentlyContinue
  $argv = @('--session', $Session, 'screenshot')
  if ($mode -eq 'full') { $argv += '--full' }
  $r = Run ($argv + $png)
  $size = if (Test-Path $png) { (Get-Item $png).Length } else { 0 }
  Check "screenshot ($mode)" ($size -gt 1000) "$size bytes" $r.ms
  Remove-Item $png -ErrorAction SilentlyContinue
}

$r = Run @('--session', $Session, 'tab', 'new', 'about:blank')
Check 'tab new' $r.ok ($r.out -split "`n")[0] $r.ms
$r = Run @('--session', $Session, 'tab', 'list')
$tabs = @(($r.out -split "`n") | Where-Object { $_ -match '\[t\d+\]' }).Count
Check 'tab list shows both tabs' ($tabs -ge 2) "$tabs tabs" $r.ms
$r = Run @('--session', $Session, 'tab', 'close')
Check 'tab close' $r.ok ($r.out -split "`n")[0] $r.ms

$r = Run @('--session', $Session, 'get', 'url', '--json')
$parsed = $null; try { $parsed = $r.out | ConvertFrom-Json } catch { }
Check '--json output parses' ($null -ne $parsed -and $parsed.success) $r.out $r.ms

if (-not $SkipDoctor) {
  $r = Run @('doctor', '--quick', '--offline') 90
  $chromeLine = ($r.out -split "`n") | Where-Object { $_ -match 'Chrome [0-9]|Chrome at' } | Select-Object -First 1
  Check 'doctor --quick --offline finishes' ($r.code -ne 'TIMEOUT') "exit=$($r.code); $chromeLine" $r.ms
}

$r = Run @('--session', $Session, 'close')
Check 'close' $r.ok ($r.out -split "`n")[0] $r.ms

# `close` answers before the daemon has finished exiting; give it a moment.
$alive = $true
for ($i = 0; $i -lt 20 -and $daemonPid; $i++) {
  $alive = [bool](Get-Process -Id $daemonPid -ErrorAction SilentlyContinue)
  if (-not $alive) { break }
  Start-Sleep -Milliseconds 250
}
Check 'daemon exits after close' ($daemonPid -and -not $alive) "pid $daemonPid $(if ($alive) {'still running'} else {'gone'})"

# Nothing here should start a Chrome on the user's real profile; if something
# did, stop it. Only this session is inspected, never the user's desktop.
$mine = (Get-Process -Id $PID).SessionId
$stray = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -eq 'chrome.exe' -and $_.SessionId -eq $mine -and $_.CommandLine -notmatch 'chrome-use' })
foreach ($c in $stray) { Stop-Process -Id $c.ProcessId -Force -ErrorAction SilentlyContinue }
Check 'no Chrome started on the real profile' ($stray.Count -eq 0) "$($stray.Count) found"

Remove-Item (Join-Path $env:TEMP "$Session-*") -Recurse -Force -ErrorAction SilentlyContinue

$results | Format-Table -AutoSize -Wrap | Out-String -Width 200
$passed = @($results | Where-Object pass).Count
Write-Host "PASSED $passed / $($results.Count)  ($Exe)"
if ($passed -eq $results.Count) { exit 0 } else { exit 1 }
