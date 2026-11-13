# Builds the release exe and the installer dist\LiveTyper-Setup-<version>.exe.
# Needs: the Rust MSVC toolchain, Visual Studio Build Tools (also for vc_redist.x64.exe), and
# Inno Setup 6 (https://jrsoftware.org/isinfo.php).

param([switch]$CpuOnly)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# The rustup MSVC toolchain first (not, for example, the scoop rust-gnu shim).
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$buildArgs = @('build', '--release', '--locked', '--target-dir', (Join-Path $root 'target'))
if ($CpuOnly) { $buildArgs += '--no-default-features' }
cargo @buildArgs
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches.Groups[1].Value

$vsRoots = @("${env:ProgramFiles(x86)}\Microsoft Visual Studio", "$env:ProgramFiles\Microsoft Visual Studio")
$vcRedist = Get-ChildItem $vsRoots -Recurse -Filter vc_redist.x64.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\VC\\Redist\\MSVC\\\d' } |
    Sort-Object { $_.VersionInfo.ProductVersion } -Descending | Select-Object -First 1
if (-not $vcRedist) { throw 'vc_redist.x64.exe not found in the Visual Studio Build Tools' }

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
          "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
          "$env:ProgramFiles\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) not found' }

& $iscc /Qp "/DAppVersion=$version" "/DVcRedist=$($vcRedist.FullName)" installer\LiveTyper.iss
if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }
Get-Item "dist\LiveTyper-Setup-$version.exe"
