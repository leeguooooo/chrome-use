# Scenario tests for install.ps1, the Windows installer.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File install-test.ps1 [-Installer <url-or-path>] [-ExpectVersion vX.Y.Z] [-Keep]
#
# This runs the REAL installer against the real per-user location
# (%LOCALAPPDATA%\Programs\chrome-use) and the real user PATH, exactly as a user
# install would. Both are put back afterwards: the binary that was there before
# (or none), and the user PATH value with its registry type. Pass -Keep to leave
# the fresh install in place instead. Exit code 0 when every scenario passes.
#
# -Installer      a URL (default: install.ps1 on main) or a local file, so a
#                 change can be tested before it is pushed
# -ExpectVersion  the tag the installer should end up on (default: the latest
#                 release, resolved the same way install.ps1 does)
#
# Kept to ASCII: Windows PowerShell 5.1 reads a BOM-less script in the system
# code page, so a UTF-8 character can be misread on a non-English system.

param(
  [string]$Installer = 'https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.ps1',
  [string]$ExpectVersion,
  [switch]$Keep
)

[Console]::OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference = 'Continue'
[Net.ServicePointManager]::SecurityProtocol =
  [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

Add-Type -Name Power -Namespace CuInstallTest -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
'@
[CuInstallTest.Power]::SetThreadExecutionState([uint32]2147483649) | Out-Null

$repo = 'leeguooooo/chrome-use'
$binDir = Join-Path $env:LOCALAPPDATA 'Programs\chrome-use'
$exe = Join-Path $binDir 'chrome-use.exe'
$results = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$pass, [string]$detail) {
  $results.Add([pscustomobject]@{ step = $name; pass = $pass; detail = $detail })
}

if (-not $ExpectVersion) {
  try {
    $req = [System.Net.HttpWebRequest]::Create("https://github.com/$repo/releases/latest")
    $req.AllowAutoRedirect = $false; $req.Method = 'HEAD'
    $resp = $req.GetResponse(); $loc = $resp.Headers['Location']; $resp.Close()
    if ($loc -match '/releases/tag/([^/?#]+)') { $ExpectVersion = $Matches[1] }
  } catch { }
  if (-not $ExpectVersion) { Write-Host 'could not resolve the latest release; pass -ExpectVersion'; exit 1 }
}
$expectNumber = $ExpectVersion.TrimStart('v')

function ReadUserPath {
  $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
  try {
    $raw = $k.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    [pscustomobject]@{ raw = $raw; kind = if ($null -ne $raw) { "$($k.GetValueKind('Path'))" } else { 'absent' } }
  } finally { $k.Close() }
}
function Entries($raw) { @(($raw + '') -split ';' | Where-Object { $_ }) }

function RunInstaller([hashtable]$envs = @{}) {
  foreach ($k in $envs.Keys) { Set-Item "Env:$k" $envs[$k] }
  $sw = [Diagnostics.Stopwatch]::StartNew()
  # The installer is run the way a user runs it: piped into iex, in this session.
  $log = & {
    if ($Installer -match '^https?://') { Invoke-RestMethod $Installer | Invoke-Expression }
    else { Get-Content -Raw -Path $Installer | Invoke-Expression }
  } *>&1 | Out-String
  foreach ($k in $envs.Keys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
  [pscustomobject]@{ log = $log; seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
}
function InstalledVersion { if (Test-Path $exe) { (& $exe --version 2>&1 | Select-Object -First 1) + '' } else { '' } }

# --- what the machine looked like before ------------------------------------
$pathBefore = ReadUserPath
$backup = $null
if (Test-Path $exe) {
  $backup = Join-Path $env:TEMP ('cu-install-test-backup-' + [guid]::NewGuid().ToString('N').Substring(0, 6) + '.exe')
  Copy-Item $exe $backup
}
Write-Host "installer: $Installer"
Write-Host "expecting: $ExpectVersion   existing install: $(if ($backup) { InstalledVersion } else { 'none' })"
Write-Host ''

# --- 1. install (or upgrade) to the expected release -------------------------
$r = RunInstaller
$v = InstalledVersion
Check 'installs the expected release' ($v -match [regex]::Escape($expectNumber)) "$v ($($r.seconds)s)"
Check 'the session survives `irm | iex`' $true 'reached the next line'
Check 'self-check finished (not stopped at 30s)' ($r.log -notmatch 'did not finish in 30s') "$($r.seconds)s"
# A child of this session inherits the PATH the installer already patched in
# memory, so it would pass even if nothing reached the registry. Rebuild PATH
# from the registry in the child instead, the way a newly opened terminal does.
$freshCmd = '$env:Path = [Environment]::GetEnvironmentVariable(''Path'',''Machine'') + '';'' + ' +
  '[Environment]::GetEnvironmentVariable(''Path'',''User''); chrome-use --version'
$fresh = Start-Process powershell -ArgumentList '-NoProfile', '-Command', $freshCmd -NoNewWindow -PassThru -Wait `
  -RedirectStandardOutput (Join-Path $env:TEMP 'cu-install-test-fresh.txt')
$freshOut = (Get-Content (Join-Path $env:TEMP 'cu-install-test-fresh.txt') -Raw -ErrorAction SilentlyContinue) + ''
Check 'a new shell finds chrome-use on PATH' ($freshOut -match [regex]::Escape($expectNumber)) (($freshOut -split "`n")[0])

# --- 2. PATH: added once, everything that was there left exactly as it was ---
$r = RunInstaller
$pathAfter = ReadUserPath
$ours = @(Entries $pathAfter.raw | Where-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $binDir.TrimEnd('\') }).Count
Check 'install dir on the user PATH exactly once (after two runs)' ($ours -eq 1) "$ours occurrence(s)"
$missing = @(Entries $pathBefore.raw | Where-Object { (Entries $pathAfter.raw) -cnotcontains $_ })
Check 'existing PATH entries kept verbatim (no %VAR% expanded)' ($missing.Count -eq 0) $(if ($missing) { "changed: $($missing -join '; ')" } else { "$((Entries $pathBefore.raw).Count) entries unchanged" })
$kindOk = ($pathBefore.kind -eq 'absent') -or ($pathAfter.kind -eq $pathBefore.kind)
Check 'PATH registry type unchanged' $kindOk "$($pathBefore.kind) -> $($pathAfter.kind)"

# --- 3. reinstall while a daemon runs from the installed binary --------------
$sock = Join-Path $env:TEMP ('cu-install-test-sock-' + [guid]::NewGuid().ToString('N').Substring(0, 6))
New-Item -ItemType Directory -Force -Path $sock | Out-Null
$env:AGENT_BROWSER_SOCKET_DIR = $sock; $env:CHROME_USE_NO_UPDATE_CHECK = '1'
# An installer run can take minutes on a slow link, longer than the daemon's
# 10-minute idle timeout, and a reaped browser would make this scenario test the
# reaper instead of the installer. Keep this session's browser for the test.
$env:AGENT_BROWSER_IDLE_TIMEOUT_MS = '0'
$null = & $exe --launch --session cu-install-test open about:blank 2>&1
$pidFile = Join-Path $sock 'cu-install-test.pid'
$daemonPid = if (Test-Path $pidFile) { [int]((Get-Content $pidFile -Raw).Trim()) } else { $null }
# Only meaningful if the daemon really is running from the installed binary.
$holding = $daemonPid -and ((Get-Process -Id $daemonPid -ErrorAction SilentlyContinue).Path -eq $exe)
$r = RunInstaller
Check 'reinstall while a daemon holds the binary' ($holding -and $r.log -match 'installed ->' -and (InstalledVersion) -match [regex]::Escape($expectNumber)) "daemon pid $daemonPid running from the installed exe: $holding"
$answer = (& $exe --session cu-install-test get url 2>&1 | Out-String).Trim()
Check 'the running session still answers afterwards' ($answer -match 'about:blank') $answer
$null = & $exe --session cu-install-test close 2>&1
for ($i = 0; $i -lt 20 -and $daemonPid -and (Get-Process -Id $daemonPid -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 250 }
Remove-Item Env:\AGENT_BROWSER_SOCKET_DIR, Env:\AGENT_BROWSER_IDLE_TIMEOUT_MS -ErrorAction SilentlyContinue
Remove-Item $sock -Recurse -Force -ErrorAction SilentlyContinue
$r = RunInstaller
$aside = @(Get-ChildItem $binDir -Filter 'chrome-use.exe.old*' -ErrorAction SilentlyContinue)
Check 'set-aside binaries removed on the next run' ($aside.Count -eq 0) "$($aside.Count) left"

# --- 4. pinned version --------------------------------------------------------
$r = RunInstaller @{ AGENT_BROWSER_VERSION = $ExpectVersion }
Check 'AGENT_BROWSER_VERSION pins the release' ($r.log -match [regex]::Escape($ExpectVersion) -and $r.log -notmatch 'resolving latest') ((($r.log -split "`n") | Where-Object { $_ -match 'downloading' }) -join '').Trim()

# --- 5. custom location, PATH left alone --------------------------------------
$custom = Join-Path $env:TEMP ('cu-install-test-dir-' + [guid]::NewGuid().ToString('N').Substring(0, 6))
$pathMark = (ReadUserPath).raw
$r = RunInstaller @{ AGENT_BROWSER_BIN_DIR = $custom; AGENT_BROWSER_NO_PATH = '1' }
Check 'AGENT_BROWSER_BIN_DIR + AGENT_BROWSER_NO_PATH' ((Test-Path (Join-Path $custom 'chrome-use.exe')) -and (ReadUserPath).raw -eq $pathMark) 'installed there; PATH unchanged'
Remove-Item $custom -Recurse -Force -ErrorAction SilentlyContinue

# --- nothing here should start a Chrome on the user's real profile -------------
$mine = (Get-Process -Id $PID).SessionId
$stray = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -eq 'chrome.exe' -and $_.SessionId -eq $mine -and $_.CommandLine -notmatch 'chrome-use' })
foreach ($c in $stray) { Stop-Process -Id $c.ProcessId -Force -ErrorAction SilentlyContinue }
Check 'no Chrome started on the real profile' ($stray.Count -eq 0) "$($stray.Count) found"

# --- put the machine back ------------------------------------------------------
if (-not $Keep) {
  if ($backup) {
    $asideNow = "$exe.old-" + [guid]::NewGuid().ToString('N').Substring(0, 8)
    Move-Item $exe $asideNow -Force
    Copy-Item $backup $exe
    Remove-Item $asideNow -Force -ErrorAction SilentlyContinue
    Remove-Item $backup -Force -ErrorAction SilentlyContinue
    $restored = "restored the previous binary ($(InstalledVersion))"
  } else {
    Remove-Item $binDir -Recurse -Force -ErrorAction SilentlyContinue
    $restored = 'removed the install (there was none before)'
  }
  $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
  try {
    if ($pathBefore.kind -eq 'absent') { $k.DeleteValue('Path', $false) }
    else { $k.SetValue('Path', $pathBefore.raw, [Microsoft.Win32.RegistryValueKind]$pathBefore.kind) }
  } finally { $k.Close() }
  [Environment]::SetEnvironmentVariable('CU_INSTALL_TEST_REFRESH', '1', 'User')
  [Environment]::SetEnvironmentVariable('CU_INSTALL_TEST_REFRESH', $null, 'User')
  $now = ReadUserPath
  Check 'machine restored' ($now.raw -eq $pathBefore.raw -and $now.kind -eq $pathBefore.kind) "$restored; PATH back to its original value"
} else {
  Write-Host 'leaving the install in place (-Keep)'
}

Remove-Item (Join-Path $env:TEMP 'cu-install-test-*') -Recurse -Force -ErrorAction SilentlyContinue
$results | Format-Table -AutoSize -Wrap | Out-String -Width 200
$passed = @($results | Where-Object pass).Count
Write-Host "PASSED $passed / $($results.Count)"
if ($passed -eq $results.Count) { exit 0 } else { exit 1 }
