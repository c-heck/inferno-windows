[CmdletBinding()]
param(
    [ValidateSet('ARM64', 'ARM64EC')][string]$Architecture = 'ARM64',
    [string]$Dll,
    [string]$Probe
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'asio-binary.ps1')
$output = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'
if ($Architecture -eq 'ARM64EC') { $output = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\arm64ec-pc-windows-msvc\release' }
if (-not $Dll) { $Dll = Join-Path $output 'inferno_asio.dll' }
if (-not $Probe) { $Probe = Join-Path $output 'asio_probe.exe' }
$Dll = (Resolve-Path -LiteralPath $Dll).Path
$Probe = (Resolve-Path -LiteralPath $Probe).Path
$actual = Get-AsioBinaryArchitecture -Path $Dll
if ($actual -ne $Architecture) { throw "Expected $Architecture DLL, found $actual." }
$names = @('INFERNO_ASIO_MODE', 'INFERNO_ASIO_RATE', 'INFERNO_ASIO_NETWORK_MS', 'INFERNO_ASIO_CHANNELS', 'INFERNO_ASIO_BIND_IP', 'INFERNO_ASIO_CONFIG', 'INFERNO_PROBE_FRAMES')
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    $env:INFERNO_ASIO_MODE = 'loopback'
    $env:INFERNO_ASIO_CONFIG = Join-Path ([IO.Path]::GetTempPath()) ("inferno-matrix-" + [Guid]::NewGuid().ToString() + '.json')
    Remove-Item -LiteralPath Env:\INFERNO_ASIO_BIND_IP -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath Env:\INFERNO_ASIO_CHANNELS -ErrorAction SilentlyContinue
    foreach ($rate in @(44100, 48000, 96000)) {
        $env:INFERNO_ASIO_RATE = "$rate"
        foreach ($ms in @(20, 10, 6, 4)) {
            $env:INFERNO_ASIO_NETWORK_MS = "$ms"
            foreach ($frames in @(64, 256, 2048)) {
                $env:INFERNO_PROBE_FRAMES = "$frames"
                Write-Output "Loopback profile: $rate Hz / $ms ms configured / $frames ASIO frames"
                & $Probe $Dll
                if ($LASTEXITCODE -ne 0) { throw "ASIO loopback profile failed ($LASTEXITCODE)." }
            }
        }
    }
} finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
Write-Output "PASS: 36 $Architecture DLL loopback profiles. This is not a Dante network interoperability test."
