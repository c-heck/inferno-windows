[CmdletBinding()]
param(
    [ValidateSet('ARM64', 'ARM64EC')][string]$Architecture = 'ARM64',
    [string[]]$AdditionalLibraryPaths = @()
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio Build Tools with C++ ARM64 tools and Windows 11 SDK.' }
$vs = & $vswhere -latest -products '*' -property installationPath
if (-not $vs) { throw 'Visual Studio Build Tools installation not found.' }
$version = (Get-Content -LiteralPath (Join-Path $vs 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt') -Raw).Trim()
$tools = Join-Path $vs "VC\Tools\MSVC\$version"
$linker = @('Hostarm64\arm64', 'Hostarm64\x64', 'Hostx64\arm64', 'Hostx64\x64') |
    ForEach-Object { Join-Path $tools "bin\$_" } |
    Where-Object { Test-Path -LiteralPath (Join-Path $_ 'link.exe') } |
    Select-Object -First 1
if (-not $linker) { throw 'MSVC linker not installed.' }
foreach ($directory in $AdditionalLibraryPaths) {
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) { throw "Missing library directory: $directory" }
    if ($directory.Contains('"')) { throw 'Library paths must not contain quotes.' }
}
if ((rustc -vV | Select-String '^host:').ToString() -notmatch 'aarch64-pc-windows-msvc') {
    throw 'Run this script with native aarch64-pc-windows-msvc Rust (rustup default stable-aarch64-pc-windows-msvc).'
}
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvarsall.bat'
$sdk = Get-ChildItem -LiteralPath (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Lib') -Directory |
    Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'um\arm64\Ws2_32.Lib') } |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $sdk) { throw 'Windows SDK ARM64 libraries not installed.' }
$libraries = @($AdditionalLibraryPaths) + @((Join-Path $tools 'lib\arm64'), (Join-Path $sdk.FullName 'um\arm64'), (Join-Path $sdk.FullName 'ucrt\arm64'))
$libraryPath = $libraries -join ';'
$targetArgs = ''
$output = Join-Path $root 'target\release'
$flags = @('-C', 'target-feature=+crt-static')
if ($Architecture -eq 'ARM64EC') {
    $target = 'arm64ec-pc-windows-msvc'
    $targetLib = & rustc --print target-libdir --target $target
    if (-not (Test-Path -LiteralPath $targetLib) -or -not (Get-ChildItem -LiteralPath $targetLib -Filter 'libstd-*.rlib')) {
        throw "Rust ARM64EC standard library missing. Run: rustup target add $target"
    }
    $component = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 Microsoft.VisualStudio.Component.VC.Tools.ARM64EC -property installationPath
    if ($component -ne $vs) {
        throw 'Install both MSVC ARM64 and ARM64EC build-tools components in the selected Visual Studio installation.'
    }
    # Current MSVC uses its hybrid ARM64 CRT and ARM64 SDK libraries for ARM64EC.
    $targetArgs = " --target $target"
    $output = Join-Path $root "target\$target\release"
}
$command = "call `"$vcvars`" arm64 >nul && set `"PATH=$linker;%PATH%`" && set `"LIB=$libraryPath`" && cd /d `"$root`" && cargo test -p inferno_asio -p usrvclock --lib$targetArgs && cargo test -p inferno_asio --test windows_socket_compat$targetArgs && cargo test -p inferno_aoip --lib asio_push_tests$targetArgs && cargo build --release -p inferno_asio$targetArgs"
$savedFlags = $env:CARGO_ENCODED_RUSTFLAGS
try {
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31
    $ErrorActionPreference = 'Continue'
    & $env:ComSpec /d /c $command
    $buildExitCode = $LASTEXITCODE
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $savedFlags
    $ErrorActionPreference = 'Stop'
}
if ($buildExitCode -ne 0) { throw "$Architecture build failed ($buildExitCode)." }
Write-Output "Built ($Architecture): $(Join-Path $output 'inferno_asio.dll')"
Write-Output "Built ($Architecture): $(Join-Path $output 'asio_probe.exe')"
Write-Output "Built ($Architecture): $(Join-Path $output 'inferno_control.exe')"
Write-Output 'No COM registration, system driver installation, or firewall changes were performed.'
