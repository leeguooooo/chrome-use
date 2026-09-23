# chrome-use installer for Windows - downloads the prebuilt binary from the
# GitHub Release (no npm, no admin, no auth for you or your users).
#
#   irm https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.ps1 | iex
#
# Env overrides (set before running, e.g. $env:AGENT_BROWSER_VERSION = 'v1.5.138'):
#   AGENT_BROWSER_VERSION   pin a specific release tag
#   AGENT_BROWSER_BIN_DIR   install location (default: %LOCALAPPDATA%\Programs\chrome-use)
#   AGENT_BROWSER_NO_SETUP  skip the guided Chrome-extension setup
#   AGENT_BROWSER_NO_SKILL  skip installing the AI agent skill
#   AGENT_BROWSER_NO_PATH   do not add the install directory to your user PATH
#
# Windows PowerShell 5.1 and PowerShell 7 are both supported. Everything runs
# inside one function: under `irm | iex` this script executes in the caller's
# session, where `exit` would close the user's window, so failures throw.

function Install-ChromeUse {
  $ErrorActionPreference = 'Stop'
  # Invoke-WebRequest's progress bar makes downloads many times slower on 5.1.
  $ProgressPreference = 'SilentlyContinue'
  # GitHub requires TLS 1.2; Windows PowerShell 5.1 may not offer it by default.
  [Net.ServicePointManager]::SecurityProtocol =
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

  $repo = 'leeguooooo/chrome-use'
  $exeName = 'chrome-use.exe'

  function Say($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
  function Fail($msg) { throw "chrome-use installation failed: $msg" }

  # This also works with old releases whose `skill install` requires npx.
  # Read JSON as UTF-8 explicitly: Windows PowerShell 5.1 otherwise decodes
  # native stdout using the console code page, corrupting non-ASCII content.
  function Get-BundledAgentSkill($target) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $target
    $start.Arguments = 'skills get chrome-use --json'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.Encoding]::UTF8
    $start.StandardErrorEncoding = [Text.Encoding]::UTF8
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $start
    try {
      if (-not $process.Start()) { throw 'could not start the CLI to read its bundled skill' }
      $stdout = $process.StandardOutput.ReadToEndAsync()
      $stderr = $process.StandardError.ReadToEndAsync()
      if (-not $process.WaitForExit(30000)) {
        $process.Kill()
        throw 'reading the bundled skill timed out after 30s'
      }
      $output = $stdout.GetAwaiter().GetResult()
      $errorText = $stderr.GetAwaiter().GetResult()
      if ($process.ExitCode -ne 0) { throw "reading the bundled skill failed (exit $($process.ExitCode)): $errorText" }
      $document = $output | ConvertFrom-Json
      $skills = @($document.data | Where-Object { $_.name -eq 'chrome-use' })
      if (-not $document.success -or $skills.Count -ne 1) { throw 'CLI did not return the chrome-use discovery skill' }
      $content = [string]$skills[0].content
      if ($content -notmatch '(?m)^name: chrome-use\s*$' -or $content -notmatch 'chrome-use skills get core') {
        throw 'bundled skill is missing its name or core-guide handoff; upgrade the CLI'
      }
      return $content
    } finally {
      $process.Dispose()
    }
  }

  # Keep these paths aligned with cli/src/skills/install.rs. Defaults cover
  # shared skills (including Codex), Claude Code and Cursor. Installing another
  # copy in .codex/skills would make Codex discover this skill twice.
  function Get-AgentSkillDirs {
    $userDir = if ($env:USERPROFILE) { $env:USERPROFILE } else { [Environment]::GetFolderPath('UserProfile') }
    if (-not $userDir) { throw 'could not determine the user profile directory' }
    $claudeDir = if ($env:CLAUDE_CONFIG_DIR) { $env:CLAUDE_CONFIG_DIR } else { Join-Path $userDir '.claude' }
    $configDir = if ($env:XDG_CONFIG_HOME) { $env:XDG_CONFIG_HOME } else { Join-Path $userDir '.config' }
    $dirs = @(
      (Join-Path $userDir '.agents\skills'),
      (Join-Path $claudeDir 'skills'),
      (Join-Path $userDir '.cursor\skills')
    )
    foreach ($runner in @(
      (Join-Path $userDir '.pi\agent'),
      (Join-Path $userDir '.codeium\windsurf'),
      (Join-Path $configDir 'opencode'),
      (Join-Path $userDir '.codebuddy'),
      (Join-Path $userDir '.trae'),
      (Join-Path $userDir '.trae-cn')
    )) {
      if (Test-Path -LiteralPath $runner -PathType Container) { $dirs += Join-Path $runner 'skills' }
    }
    return $dirs | Select-Object -Unique
  }

  function Install-AgentSkill($target, $dirs = (Get-AgentSkillDirs)) {
    $content = Get-BundledAgentSkill $target
    $utf8 = New-Object Text.UTF8Encoding($false)
    $errors = @()
    if (@($dirs).Count -eq 0) { throw 'no skill installation directories found' }
    foreach ($baseDir in $dirs) {
      $skillDir = Join-Path $baseDir 'chrome-use'
      $destination = Join-Path $skillDir 'SKILL.md'
      $staging = Join-Path $skillDir ('.SKILL-' + [guid]::NewGuid().ToString('N') + '.tmp')
      try {
        $null = [IO.Directory]::CreateDirectory($skillDir)
        [IO.File]::WriteAllText($staging, $content, $utf8)
        if ([IO.File]::Exists($destination)) {
          [IO.File]::Replace($staging, $destination, [NullString]::Value)
        } else {
          [IO.File]::Move($staging, $destination)
        }
        if ([IO.File]::ReadAllText($destination, $utf8) -cne $content) {
          throw 'installed content differs from the bundled skill'
        }
        Say "agent skill installed and verified: $destination"
      } catch {
        $errors += "${destination}: $($_.Exception.Message)"
      } finally {
        if ([IO.File]::Exists($staging)) { [IO.File]::Delete($staging) }
      }
    }
    if ($errors.Count) { throw ($errors -join "`n") }
    Say 'Restart your agent or reload its skills to discover chrome-use.'
  }

  # --- platform -> release asset ---------------------------------------------
  # Only an x64 build is published. Windows 11 on ARM runs it under x64
  # emulation, so ARM64 gets the same binary with a note rather than a refusal.
  $arch = $env:PROCESSOR_ARCHITEW6432
  if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
  switch ($arch) {
    'AMD64' { }
    'ARM64' { Say 'ARM64 detected: installing the x64 build, which runs under x64 emulation on Windows 11.' }
    default { Fail "unsupported architecture: $arch (only x64 builds are published)"; return }
  }
  $asset = 'chrome-use-win32-x64'

  foreach ($tool in 'tar.exe') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
      Fail "$tool is required (it ships with Windows 10 1803 and later)"; return
    }
  }

  # --- resolve release tag ---------------------------------------------------
  $tag = $env:AGENT_BROWSER_VERSION
  if (-not $tag) {
    Say 'resolving latest release...'
    # Follow github.com's releases/latest redirect rather than the JSON API,
    # which rate-limits unauthenticated callers to 60 requests an hour.
    try {
      $req = [System.Net.HttpWebRequest]::Create("https://github.com/$repo/releases/latest")
      $req.AllowAutoRedirect = $false
      $req.Method = 'HEAD'
      $resp = $req.GetResponse()
      $location = $resp.Headers['Location']
      $resp.Close()
    } catch {
      $location = $null
    }
    if ($location -match '/releases/tag/([^/?#]+)') { $tag = $Matches[1] }
    if (-not $tag) {
      Fail "could not resolve the latest release (set `$env:AGENT_BROWSER_VERSION = 'vX.Y.Z')"; return
    }
  }

  $base = "https://github.com/$repo/releases/download/$tag"
  $tgzUrl = "$base/$asset.tar.gz"
  $shaUrl = "$tgzUrl.sha256"

  # --- download + verify -----------------------------------------------------
  $tmp = Join-Path ([IO.Path]::GetTempPath()) ("chrome-use-install-" + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force -Path $tmp | Out-Null
  try {
    $tgz = Join-Path $tmp 'pkg.tar.gz'
    Say "downloading $asset ($tag)..."
    try {
      Invoke-WebRequest -UseBasicParsing -Uri $tgzUrl -OutFile $tgz
    } catch {
      Fail "download failed: $tgzUrl (is '$asset.tar.gz' attached to release $tag?)"; return
    }

    # The checksum is not optional here. An interrupted download still extracts
    # into a chrome-use.exe, which then dies at launch with an access violation
    # (exit code -1073741819) instead of anything that says the file is short.
    Say 'verifying checksum...'
    # Saved to a file and read back as text: Windows PowerShell 5.1 hands back
    # `.Content` as a byte array when the server sends a binary content type,
    # and splitting that yields the first byte (50, for a hash starting "2").
    $shaFile = Join-Path $tmp 'pkg.sha256'
    try {
      Invoke-WebRequest -UseBasicParsing -Uri $shaUrl -OutFile $shaFile
      $expected = ((Get-Content -Raw -Path $shaFile).Trim() -split '\s+')[0].ToLower()
    } catch {
      Fail "could not download the checksum ($shaUrl); not installing an unverified binary"; return
    }
    $actual = (Get-FileHash -Algorithm SHA256 -Path $tgz).Hash.ToLower()
    if ($expected -ne $actual) {
      Fail ("checksum mismatch: the download is incomplete or corrupted. Nothing was installed; run the " +
            "installer again.`n  expected $expected`n  got      $actual")
      return
    }

    & tar.exe -xzf $tgz -C $tmp
    if ($LASTEXITCODE -ne 0) { Fail 'could not extract the release archive' }
    $newExe = Join-Path $tmp $exeName
    if (-not (Test-Path $newExe)) { Fail "the archive did not contain $exeName"; return }

    # --- install -------------------------------------------------------------
    $binDir = $env:AGENT_BROWSER_BIN_DIR
    if (-not $binDir) { $binDir = Join-Path $env:LOCALAPPDATA 'Programs\chrome-use' }
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    $target = Join-Path $binDir $exeName

    # A running chrome-use.exe (a session daemon) cannot be overwritten, but it
    # can be renamed. Move the old one aside, put the new one in place, and
    # delete the old one if nothing is still running it; otherwise the next
    # install removes it.
    Get-ChildItem -Path $binDir -Filter "$exeName.old*" -ErrorAction SilentlyContinue |
      ForEach-Object { Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue }
    if (Test-Path $target) {
      $aside = "$target.old-" + [guid]::NewGuid().ToString('N').Substring(0, 8)
      Move-Item -Path $target -Destination $aside -Force
      Move-Item -Path $newExe -Destination $target -Force
      Remove-Item $aside -Force -ErrorAction SilentlyContinue
    } else {
      Move-Item -Path $newExe -Destination $target -Force
    }
    Say "installed -> $target"
    & $target --version
    if ($LASTEXITCODE -ne 0) { Fail 'the installed CLI could not run' }
  } finally {
    $cleanupRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $cleanupTarget = [IO.Path]::GetFullPath($tmp)
    if ($cleanupTarget.StartsWith($cleanupRoot, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($cleanupTarget).StartsWith('chrome-use-install-')) {
      Remove-Item -Recurse -Force -LiteralPath $cleanupTarget -ErrorAction SilentlyContinue
    }
  }

  # --- PATH ------------------------------------------------------------------
  # Added to the USER path only (no admin), and only once. Read and written
  # through the registry, not [Environment]::SetEnvironmentVariable: that API
  # hands back the user Path with %VARIABLES% already expanded and writes it
  # back as a plain string, permanently replacing every %USERPROFILE%-style
  # entry the user had with an absolute path. Here the raw value and its
  # REG_EXPAND_SZ type are kept exactly as they were, plus one entry.
  if (-not $env:AGENT_BROWSER_NO_PATH) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    try {
      $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
      $entries = @($raw -split ';' | Where-Object { $_ })
      $present = $entries | Where-Object {
        [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $binDir.TrimEnd('\')
      }
      if (-not $present) {
        $newPath = (@($entries) + $binDir) -join ';'
        $key.SetValue('Path', $newPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
        # Tell running programs (Explorer, and so every terminal it starts) that
        # the environment changed. Setting and clearing a throwaway variable
        # through the .NET API is what broadcasts WM_SETTINGCHANGE.
        [Environment]::SetEnvironmentVariable('CHROME_USE_PATH_REFRESH', '1', 'User')
        [Environment]::SetEnvironmentVariable('CHROME_USE_PATH_REFRESH', $null, 'User')
        Say "added $binDir to your user PATH (new terminals pick it up)"
      }
    } finally {
      $key.Close()
    }
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $binDir.TrimEnd('\') })) {
      $env:Path = "$env:Path;$binDir"
    }
  } else {
    Say "left PATH unchanged; run it as `"$target`" or add $binDir yourself"
  }

  # Interactive only when a person is at the console. Over SSH, in CI or inside
  # an agent the guided setup would wait on a prompt nobody can answer.
  $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected -and
    -not $env:SSH_CONNECTION -and -not $env:CI

  # --- guided setup: Chrome extension ----------------------------------------
  $setupFailed = $false
  if ($interactive -and -not $env:AGENT_BROWSER_NO_SETUP) {
    Say 'setting up the Chrome extension...'
    try {
      & $target extension install
      if ($LASTEXITCODE -ne 0) { throw "exit $LASTEXITCODE" }
    } catch {
      $setupFailed = $true
      Write-Warning "Extension setup failed: $($_.Exception.Message). Run chrome-use extension install to retry."
    }
  } else {
    Say "skipped extension setup (no interactive console). Run ``chrome-use extension install`` later."
  }

  # --- AI agent skill ----------------------------------------------------------
  if (-not $env:AGENT_BROWSER_NO_SKILL) {
    Say 'installing the bundled agent skill (no Node required)...'
    try { Install-AgentSkill $target } catch { Fail "agent skill: $($_.Exception.Message)" }
  } else {
    Say 'agent skill installation skipped (AGENT_BROWSER_NO_SKILL is set).'
  }

  # --- self-check + first prompt ---------------------------------------------
  # Bounded, so the installer always finishes. Up to v1.5.138 this check ran
  # `chrome.exe --version`, which on Windows starts the browser instead of
  # printing a version and never returns; pinning AGENT_BROWSER_VERSION to one
  # of those releases would otherwise leave the installer waiting forever.
  Say 'self-check...'
  try {
    $check = Start-Process -FilePath $target -ArgumentList 'doctor --quick --offline' -NoNewWindow -PassThru
    $null = $check.Handle
    if (-not $check.WaitForExit(30000)) {
      Stop-Process -Id $check.Id -Force -ErrorAction SilentlyContinue
      throw 'self-check did not finish in 30s and was stopped; run chrome-use doctor later'
    }
    if ($check.ExitCode -ne 0) { throw "doctor exited with code $($check.ExitCode); resolve the failures above" }
  } catch { Fail "self-check: $($_.Exception.Message)" }
  if ($setupFailed) { Fail 'CLI and skill steps finished, but extension setup still needs repair' }
  Write-Host ''
  Say 'CLI installation complete. Check the extension status above, then try this in your AI agent:'
  Write-Host ''
  Write-Host '    Use chrome-use to open https://news.ycombinator.com and tell me the top 3 titles'
  Write-Host ''
}

Install-ChromeUse
