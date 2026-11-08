#Requires -RunAsAdministrator
<#
Inbound firewall rules for the Inferno ARM ASIO driver (used by the installer).

The driver runs inside the DAW process. Its fixed ports are opened for the local subnet
(Dante uses link-local / same-subnet addressing). Receive audio arrives on ephemeral ports,
so installed Steinberg hosts (Nuendo / Cubase) also get a UDP program rule.
All rules share the group below, so -Remove deletes exactly what -Add created.
.EXAMPLE
  .\firewall.ps1 -Add
  .\firewall.ps1 -Add -Program 'C:\Program Files\REAPER (ARM64)\reaper.exe'
  .\firewall.ps1 -Remove
#>
param([switch]$Add, [switch]$Remove, [string[]]$Program = @())

$ErrorActionPreference = 'Stop'
$Group = 'Inferno ARM ASIO'

if ($Add -eq $Remove) { throw 'Specify exactly one of -Add or -Remove.' }

Get-NetFirewallRule -Group $Group -ErrorAction SilentlyContinue | Remove-NetFirewallRule
if ($Remove) {
  Write-Host "Removed firewall rules in group '$Group'."
  return
}

$ports = @(
  @{ Name = 'Dante ARC/CMC/flow control/info (45000-45003)'; Port = @('45000-45003') },
  @{ Name = 'PTP event/general (319-320)'; Port = @('319-320') },
  @{ Name = 'Dante discovery mDNS (5353)'; Port = @('5353') },
  @{ Name = 'Dante monitoring multicast (8702, 8708)'; Port = @('8702', '8708') }
)
foreach ($p in $ports) {
  New-NetFirewallRule -Group $Group -DisplayName "Inferno ARM ASIO - $($p.Name)" `
    -Direction Inbound -Action Allow -Protocol UDP -LocalPort $p.Port `
    -RemoteAddress LocalSubnet -Profile Any | Out-Null
}

$hosts = @($Program)
$steinberg = Join-Path $env:ProgramFiles 'Steinberg'
if (Test-Path -LiteralPath $steinberg) {
  $hosts += Get-ChildItem -LiteralPath $steinberg -Directory | ForEach-Object {
    Get-ChildItem -LiteralPath $_.FullName -File -Filter '*.exe' |
      Where-Object { $_.BaseName -match '^(Nuendo|Cubase)\d*$' } |
      Select-Object -ExpandProperty FullName
  }
}
foreach ($exe in ($hosts | Where-Object { $_ } | Sort-Object -Unique)) {
  New-NetFirewallRule -Group $Group -DisplayName "Inferno ARM ASIO - $(Split-Path -Leaf $exe)" `
    -Direction Inbound -Action Allow -Protocol UDP -Program $exe `
    -RemoteAddress LocalSubnet -Profile Any | Out-Null
  Write-Host "Allowed host: $exe"
}
Write-Host "Firewall rules added in group '$Group'."
