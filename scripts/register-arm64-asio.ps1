[CmdletBinding(SupportsShouldProcess)]
param(
    [ValidateSet('Register', 'Unregister')][string]$Action = 'Register',
    [ValidateSet('ARM64', 'ARM64EC')][string]$Architecture = 'ARM64',
    [string]$Dll
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'asio-binary.ps1')
if (-not $Dll) {
    $output = 'target\release'
    if ($Architecture -eq 'ARM64EC') { $output = 'target\arm64ec-pc-windows-msvc\release' }
    $Dll = Join-Path (Split-Path -Parent $PSScriptRoot) "$output\inferno_asio.dll"
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $WhatIfPreference -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this registration script from elevated 64-bit PowerShell. No kernel driver or test-signing is needed.'
}
$clsid = '{C4CD4CF3-37E1-4B6E-AB2F-90016AB0A640}'
$driverKey = 'SOFTWARE\ASIO\Inferno ARM ASIO'
$classKey = "SOFTWARE\Classes\CLSID\$clsid"
$registry = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
try {
    if ($Action -eq 'Register') {
        $Dll = (Resolve-Path -LiteralPath $Dll).Path
        $actual = Get-AsioBinaryArchitecture -Path $Dll
        if ($actual -ne $Architecture) { throw "Expected $Architecture DLL, found $actual. Select the matching build and -Architecture." }
        if ($PSCmdlet.ShouldProcess($Dll, "Register experimental $Architecture ASIO COM device in HKLM (replaces the selected Inferno build)")) {
            $asio = $registry.CreateSubKey($driverKey)
            try { $asio.SetValue('CLSID', $clsid); $asio.SetValue('Description', 'Inferno ARM ASIO (experimental, unofficial)') }
            finally { $asio.Dispose() }
            $class = $registry.CreateSubKey($classKey)
            try { $class.SetValue('', 'Inferno ARM ASIO') } finally { $class.Dispose() }
            $server = $registry.CreateSubKey("$classKey\InprocServer32")
            try { $server.SetValue('', $Dll); $server.SetValue('ThreadingModel', 'Both') }
            finally { $server.Dispose() }
        }
    } elseif ($PSCmdlet.ShouldProcess('Inferno ARM ASIO', 'Unregister only this ASIO device and COM class')) {
        $existing = $registry.OpenSubKey($driverKey)
        if ($existing) {
            try { if ($existing.GetValue('CLSID') -ne $clsid) { throw 'ASIO registry entry belongs to another device; refusing removal.' } }
            finally { $existing.Dispose() }
        }
        $registry.DeleteSubKeyTree($driverKey, $false)
        $registry.DeleteSubKeyTree($classKey, $false)
    }
} finally { $registry.Dispose() }
