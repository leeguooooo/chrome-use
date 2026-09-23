# Run with Windows PowerShell 5.1 and PowerShell 7. No Pester dependency.
# Optional: pass a released CLI to verify extraction without Node or npx.
param([string]$ChromeUse)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $repo 'install.ps1'), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw ($parseErrors -join "`n") }
foreach ($name in 'Get-BundledAgentSkill', 'Get-AgentSkillDirs', 'Install-AgentSkill') {
  $definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }.GetNewClosure(), $true)
  . ([scriptblock]::Create($definition.Extent.Text))
}
function Assert($condition, $message) { if (-not $condition) { throw $message } }
$script:messages = @()
function Say($message) { $script:messages += $message }
function Fail($message) { throw $message }
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('chrome-use-installer-test-' + [guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($testRoot)
$savedCodex = $env:CODEX_HOME
$savedClaude = $env:CLAUDE_CONFIG_DIR
$savedSkip = $env:AGENT_BROWSER_NO_SKILL
$savedSetup = $env:AGENT_BROWSER_NO_SETUP
try {
  if ($ChromeUse) {
    $actual = Get-BundledAgentSkill $ChromeUse
    Assert ($actual -match 'chrome-use skills get core') 'released CLI did not return its skill'
    Assert ($actual -match ([string][char]0x4e2d + [char]0x6587)) 'UTF-8 Chinese text was corrupted'
    Write-Output 'pass: released CLI extraction preserves UTF-8'
  }
  $script:content = "---`nname: chrome-use`n---`nchrome-use skills get core`n" + [char]0x4e2d + [char]0x6587
  function Get-BundledAgentSkill($target) { return $script:content }
  $base = Join-Path $testRoot ('runner ' + [char]0x4e2d + [char]0x6587)
  Install-AgentSkill 'unused' @($base)
  $file = Join-Path $base 'chrome-use\SKILL.md'
  Assert ([IO.File]::ReadAllText($file) -ceq $script:content) 'initial installation differs'
  $unrelated = Join-Path $base 'chrome-use\notes.md'
  [IO.File]::WriteAllText($unrelated, 'keep')
  [IO.File]::WriteAllText($file, 'old')
  Install-AgentSkill 'unused' @($base)
  Assert ([IO.File]::ReadAllText($file) -ceq $script:content) 'refresh did not replace old content'
  Assert ([IO.File]::ReadAllText($unrelated) -eq 'keep') 'refresh changed unrelated files'
  Write-Output 'pass: initial install and refresh with spaces and Unicode paths'

  [IO.File]::WriteAllText($file, 'preserve on failed replacement')
  [IO.File]::SetAttributes($file, [IO.FileAttributes]::ReadOnly)
  try {
    $failed = $false
    try { Install-AgentSkill 'unused' @($base) } catch { $failed = $true }
    Assert $failed 'read-only destination did not fail'
    Assert ([IO.File]::ReadAllText($file) -eq 'preserve on failed replacement') 'failed replacement lost the old skill'
    Assert (@(Get-ChildItem -LiteralPath (Split-Path $file) -Filter '.SKILL-*.tmp' -Force).Count -eq 0) 'staging file leaked'
  } finally {
    [IO.File]::SetAttributes($file, [IO.FileAttributes]::Normal)
  }
  Write-Output 'pass: failed replacement preserves the old skill and cleans staging'

  $blocked = Join-Path $testRoot 'blocked'
  [IO.File]::WriteAllText($blocked, 'keep')
  $failed = $false
  try { Install-AgentSkill 'unused' @($blocked, $base) } catch { $failed = $_.Exception.Message.Contains('blocked') }
  Assert $failed 'partial failure did not throw with its path'
  Assert ([IO.File]::ReadAllText($file) -ceq $script:content) 'other destinations were not attempted'
  Assert ([IO.File]::ReadAllText($blocked) -eq 'keep') 'blocked file was modified'
  Write-Output 'pass: partial failure remains an error and preserves existing files'

  $env:CODEX_HOME = Join-Path $testRoot 'codex override'
  $env:CLAUDE_CONFIG_DIR = Join-Path $testRoot 'claude override'
  $destinations = @(Get-AgentSkillDirs)
  Assert ($destinations -notcontains (Join-Path $env:CODEX_HOME 'skills')) 'duplicate Codex skill destination'
  Assert ($destinations -contains (Join-Path $env:CLAUDE_CONFIG_DIR 'skills')) 'CLAUDE_CONFIG_DIR ignored'
  Write-Output 'pass: custom runner homes respected'

  # Execute the actual installer tail, replacing only external effects.
  $source = [IO.File]::ReadAllText((Join-Path $repo 'install.ps1'))
  $start = $source.IndexOf('  # --- guided setup:')
  $end = $source.LastIndexOf("`n}")
  $tail = [scriptblock]::Create($source.Substring($start, $end - $start))
  $target = 'Invoke-FixtureCli'
  function Invoke-FixtureCli { $global:LASTEXITCODE = 7 }
  function Install-AgentSkill($target) { if ($script:skillFails) { throw 'fixture skill failed' } }
  function Start-Process {
    $fake = [pscustomobject]@{ Handle = 1; Id = 0; ExitCode = $script:doctorExit }
    $fake | Add-Member ScriptMethod WaitForExit { param($timeout) return -not $script:doctorTimeout }
    return $fake
  }
  function Stop-Process { }
  foreach ($scenario in 'skill failure', 'extension failure', 'doctor failure', 'doctor timeout', 'success', 'explicit skip') {
    $script:messages = @()
    $script:skillFails = $scenario -in @('skill failure', 'explicit skip')
    $script:doctorExit = if ($scenario -eq 'doctor failure') { 1 } else { 0 }
    $script:doctorTimeout = $scenario -eq 'doctor timeout'
    $env:AGENT_BROWSER_NO_SKILL = if ($scenario -eq 'explicit skip') { '1' } else { $null }
    $env:AGENT_BROWSER_NO_SETUP = $null
    $interactive = $scenario -eq 'extension failure'
    $thrown = $false
    try { & $tail } catch { $thrown = $true }
    $expectedFailure = $scenario -in @('skill failure', 'extension failure', 'doctor failure', 'doctor timeout')
    Assert ($thrown -eq $expectedFailure) "wrong failure state: $scenario"
    $completed = @($script:messages | Where-Object { $_ -like 'CLI installation complete*' }).Count -gt 0
    Assert ($completed -ne $expectedFailure) "wrong completion message: $scenario"
    Write-Output "pass: $scenario"
  }
} finally {
  $env:CODEX_HOME = $savedCodex
  $env:CLAUDE_CONFIG_DIR = $savedClaude
  $env:AGENT_BROWSER_NO_SKILL = $savedSkip
  $env:AGENT_BROWSER_NO_SETUP = $savedSetup
  $resolved = [IO.Path]::GetFullPath($testRoot)
  $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
  if ($resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
      [IO.Path]::GetFileName($resolved).StartsWith('chrome-use-installer-test-')) {
    Remove-Item -LiteralPath $resolved -Recurse -Force
  }
}
# The extension-failure fixture sets LASTEXITCODE. CI shells forward it, so
# reset it only after all assertions and cleanup have succeeded.
$global:LASTEXITCODE = 0
