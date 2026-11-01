# Controlled build inputs only. No runtime permission or browser operation.
$ErrorActionPreference = 'Stop'
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or !$installation) { throw 'MSVC installation missing' }
$setup = Join-Path $installation 'Common7\Tools\VsDevCmd.bat'
$environment = cmd.exe /d /c "call `"$setup`" -no_logo -arch=x64 >nul && set"
if ($LASTEXITCODE -ne 0) { throw 'MSVC environment setup failed' }
$environment | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
    }
}
if (!$env:VCToolsInstallDir) { throw 'MSVC tool directory missing' }
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = Join-Path $env:VCToolsInstallDir 'bin/Hostx64/x64/link.exe'
if (!(Test-Path $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER)) { throw 'Microsoft linker missing' }
cargo fetch --locked --manifest-path cli/Cargo.toml
if ($LASTEXITCODE -ne 0) { throw 'Dependency fetch failed' }
$env:CHROME_USE_MARKER_DEPENDENCIES = Join-Path $env:RUNNER_TEMP 'identity-dependencies.json'
cargo metadata --manifest-path cli/Cargo.toml --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc --features windows-build-identity-prototype,windows-tcp-owner-prototype > $env:CHROME_USE_MARKER_DEPENDENCIES
if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
$env:CHROME_USE_BUILD_GENERATION = [guid]::NewGuid().ToString('N')
foreach ($name in @('PATH', 'LIB', 'LIBPATH', 'INCLUDE', 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER', 'CHROME_USE_MARKER_DEPENDENCIES', 'CHROME_USE_BUILD_GENERATION')) {
    "$name=$([Environment]::GetEnvironmentVariable($name, 'Process'))" | Out-File $env:GITHUB_ENV -Append -Encoding utf8
}
