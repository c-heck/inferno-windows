#Requires -RunAsAdministrator
<#
Tunes the Dante network adapter for low-loss audio.

Small USB NIC receive queues (e.g. Realtek USB GbE defaults: 6 URBs / 16 buffers)
overflow when the USB stack stalls for a few tens of milliseconds, which drops
bursts of Dante packets and produces audible dropouts. This script enlarges the
queues, disables receive coalescing and device power saving, and installs the
"Inferno Low Latency" power plan (see inferno-power.ps1). The adapter restarts
briefly - close the DAW first.
.EXAMPLE
  .\scripts\tune-dante-nic.ps1                 # adapter of the configured bind_ip, or the only wired one
  .\scripts\tune-dante-nic.ps1 -Adapter 'Ethernet 4'
#>
param([string]$Adapter)

$ErrorActionPreference = 'Stop'

if (-not $Adapter) {
  $ip = $env:INFERNO_ASIO_BIND_IP
  $settings = Join-Path $env:LOCALAPPDATA 'InfernoArmAsio\settings.json'
  if (-not $ip -and (Test-Path $settings)) { $ip = (Get-Content $settings -Raw | ConvertFrom-Json).bind_ip }
  if ($ip) {
    $Adapter = (Get-NetIPAddress -IPAddress $ip -ErrorAction SilentlyContinue | Get-NetAdapter).Name
  }
  if (-not $Adapter) {
    $wired = @(Get-NetAdapter -Physical | Where-Object { $_.Status -eq 'Up' -and $_.NdisPhysicalMedium -eq 14 })
    if ($wired.Count -ne 1) {
      throw "Specify -Adapter; wired adapters: $(($wired | ForEach-Object Name) -join ', ')"
    }
    $Adapter = $wired[0].Name
  }
}
$nic = Get-NetAdapter -Name $Adapter
Write-Host "Tuning '$Adapter' ($($nic.InterfaceDescription))"

# Keyword support differs per vendor (Realtek USB: PendingReceives/ReceiveBufferLen,
# Intel/standard: *ReceiveBuffers); only present keywords are set, clamped to the valid range.
$wanted = [ordered]@{
  PendingReceives   = 64
  ReceiveBufferLen  = 256
  PendingTransmits  = 32
  TransmitBufferLen = 128
  '*ReceiveBuffers' = 1024
  '*TransmitBuffers' = 1024
  '*RscIPv4'        = 0
  '*RscIPv6'        = 0
  '*EEE'            = 0
  EEE               = 0
  AdvancedEEE       = 0
  EnableGreenEthernet = 0
  '*InterruptModeration' = 0
  '*FlowControl'    = 0
}
$props = Get-NetAdapterAdvancedProperty -Name $Adapter -AllProperties
foreach ($k in $wanted.Keys) {
  $prop = $props | Where-Object RegistryKeyword -eq $k | Select-Object -First 1
  if (-not $prop) { continue }
  $value = $wanted[$k]
  if ($prop.NumericParameterMaxValue -and $value -gt $prop.NumericParameterMaxValue) { $value = $prop.NumericParameterMaxValue }
  if ($prop.ValidRegistryValues -and ($prop.ValidRegistryValues -notcontains "$value")) { continue }
  Set-NetAdapterAdvancedProperty -Name $Adapter -RegistryKeyword $k -RegistryValue $value -NoRestart
  Write-Host "set $k = $value"
}

# "Allow the computer to turn off this device to save power"
$pnp = Get-CimInstance -Namespace root\wmi -ClassName MSPower_DeviceEnable -ErrorAction SilentlyContinue |
  Where-Object { $_.InstanceName -like "$($nic.PnPDeviceID)*" }
foreach ($p in $pnp) { $p.Enable = $false; Set-CimInstance -InputObject $p; Write-Host 'disabled device power saving' }

# The low-latency power plan is created on demand by the driver and only active while audio
# streams; pre-provisioning it here makes it visible in Power Options right away.
& (Join-Path $PSScriptRoot 'inferno-power.ps1') -Install

Restart-NetAdapter -Name $Adapter
Write-Host "Restarted $Adapter"
