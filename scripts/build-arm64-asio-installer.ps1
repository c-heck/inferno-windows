<#
Builds the Windows on Arm installer: dist\InfernoArmAsio-<version>-arm64-setup.exe

  .\scripts\build-arm64-asio-installer.ps1                 # build ARM64EC driver + installer
  .\scripts\build-arm64-asio-installer.ps1 -IncludeNative  # also offer the native ARM64 build
  .\scripts\build-arm64-asio-installer.ps1 -SkipBuild      # package existing release builds
  .\scripts\build-arm64-asio-installer.ps1 -Release        # refuse uncommitted/unpushed source

The installer links to the source repository (GNU AGPL v3) and records the exact commit in
SOURCE.txt. -SourceUrl defaults to the "origin" remote. For a distributed installer, build
from a committed and pushed tree (-Release enforces this) so that the published source
matches the binaries.

Requires Inno Setup 6.3+ (winget install JRSoftware.InnoSetup). Close DAWs that have the
development DLL loaded before building, or the release link step cannot replace it.
#>
[CmdletBinding()]
param([switch]$IncludeNative, [switch]$SkipBuild, [switch]$Release, [string]$SourceUrl)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'asio-binary.ps1')

$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source),
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found. Run: winget install JRSoftware.InnoSetup' }

$version = (Select-String -LiteralPath (Join-Path $root 'inferno_asio\Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value

function Invoke-Git { & git -C $root @args 2>$null }
if (-not $SourceUrl) { $SourceUrl = Invoke-Git remote get-url origin }
if (-not $SourceUrl) { throw 'No "origin" remote; pass -SourceUrl <public repository URL>.' }
$SourceUrl = ($SourceUrl.Trim() -replace '^git@github\.com:', 'https://github.com/') -replace '\.git$', ''
$commit = (Invoke-Git rev-parse HEAD)
if (-not $commit) { throw 'Not a git checkout; the installer must reference the exact source commit.' }
$commit = $commit.Trim()
$dirty = [bool](Invoke-Git status --porcelain --untracked-files=no)
$pushed = [bool](Invoke-Git branch -r --contains $commit)
$problems = @()
if ($dirty) { $problems += 'the working tree has uncommitted changes' }
if (-not $pushed) { $problems += "commit $commit is not on any remote branch" }
if ($problems) {
    $message = "Published source will not match this build: $($problems -join '; ')."
    if ($Release) { throw "$message Commit and push first." }
    Write-Warning "$message Do not distribute this installer; use -Release for distribution builds."
}
$sourceDir = Join-Path $root 'target\installer'
New-Item -ItemType Directory -Force -Path $sourceDir | Out-Null
$sourceInfo = Join-Path $sourceDir 'SOURCE.txt'
@(
    "Inferno ARM ASIO $version - corresponding source code (GNU AGPL v3)",
    '',
    "Repository: $SourceUrl",
    "Commit:     $commit",
    "Browse:     $SourceUrl/tree/$commit",
    "Get it:     git clone $SourceUrl && git -C inferno-windows checkout $commit",
    $(if ($problems) { "`r`nWARNING: development build - $($problems -join '; ')." })
) | Set-Content -LiteralPath $sourceInfo -Encoding UTF8

$builds = @(@{ Arch = 'ARM64EC'; Dir = Join-Path $root 'target\arm64ec-pc-windows-msvc\release'; Define = 'EcDir' })
if ($IncludeNative) { $builds += @{ Arch = 'ARM64'; Dir = Join-Path $root 'target\release'; Define = 'NativeDir' } }

$defines = @("/DAppVersion=$version", "/DRepoRoot=$root", "/DSourceUrl=$SourceUrl", "/DSourceInfo=$sourceInfo")
foreach ($b in $builds) {
    if (-not $SkipBuild) {
        & (Join-Path $PSScriptRoot 'build-arm64-asio.ps1') -Architecture $b.Arch
    }
    foreach ($file in 'inferno_asio.dll', 'inferno_control.exe') {
        $path = Join-Path $b.Dir $file
        if (-not (Test-Path -LiteralPath $path)) { throw "Missing $path - build $($b.Arch) first." }
    }
    $actual = Get-AsioBinaryArchitecture -Path (Join-Path $b.Dir 'inferno_asio.dll')
    if ($actual -ne $b.Arch) { throw "$($b.Dir)\inferno_asio.dll is $actual, expected $($b.Arch)." }
    $defines += "/D$($b.Define)=$($b.Dir)"
}

& $iscc /Qp @defines (Join-Path $root 'installer\inferno-arm-asio.iss')
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed ($LASTEXITCODE)." }
$setup = Join-Path $root "dist\InfernoArmAsio-$version-arm64-setup.exe"
Write-Output "Built installer: $setup"
