<#
"Inferno Low Latency" power plan for Dante/ASIO sessions on Windows on Arm.

The ASIO driver switches to this plan automatically while audio streams and restores the
previous plan and power mode afterwards (opt out with INFERNO_ASIO_POWER_PLAN=off). This
script is for pre-provisioning, manual use and clean-up; no admin rights needed:

  .\scripts\inferno-power.ps1 -Install   # create/update the plan (copy of Balanced)
  .\scripts\inferno-power.ps1 -On        # switch to it + "Best performance" power mode
  .\scripts\inferno-power.ps1 -Off       # back to Balanced + "Balanced" power mode
  .\scripts\inferno-power.ps1 -Uninstall # delete the plan

The plan keeps cores unparked at full clock and disables PCIe ASPM, USB3 link power
management and USB selective suspend, on AC and battery. Waking from those states stalled
the USB Ethernet adapter for 25-70 ms on a Snapdragon X Elite, dropping Dante packets.
#>
param([switch]$Install, [switch]$On, [switch]$Off, [switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$Name = 'Inferno Low Latency'
$Plan = '6a3f5c1e-8d2b-4e7a-9c41-1f0d2b7e5a90' # fixed; must match inferno_asio/src/power.rs
$Balanced = '381b4222-f694-41f0-9685-ff5bb260df2e'
$BestPerformanceOverlay = 'ded574b5-45a0-4f42-8737-46345c09c238'
$BalancedOverlay = '00000000-0000-0000-0000-000000000000'
$Settings = @(
  @('54533251-82be-4824-96c1-47b60b740d00', '893dee8e-2bef-41e0-89c6-b55d0929964c', 100), # min processor state
  @('54533251-82be-4824-96c1-47b60b740d00', '0cc5b647-c1df-4637-891a-dec35c318583', 100), # core parking min cores
  @('54533251-82be-4824-96c1-47b60b740d00', '36687f9e-e3a5-4dbf-b1dc-15eb381c6863', 0),   # energy preference: performance
  @('501a4d13-42af-4429-9fd1-a8218c268e20', 'ee12f906-d277-404b-b6da-e5fa1a576df5', 0),   # PCIe ASPM off
  @('2a737441-1930-4402-8d77-b2bebba308a3', 'd4e98f31-5ffe-4ce1-be31-1b38b384c009', 0),   # USB3 link PM off
  @('2a737441-1930-4402-8d77-b2bebba308a3', '48e6b7a6-50f5-4782-a5d4-53bb8f07e226', 0)    # USB selective suspend off
)

function Test-Plan { (powercfg /list | Out-String) -match $Plan }

if ($Install) {
  if (-not (Test-Plan)) {
    powercfg /duplicatescheme $Balanced $Plan | Out-Null
    powercfg /changename $Plan $Name 'Low-latency settings for Dante/ASIO sessions'
  }
  foreach ($s in $Settings) {
    powercfg /setacvalueindex $Plan $s[0] $s[1] $s[2]
    powercfg /setdcvalueindex $Plan $s[0] $s[1] $s[2]
  }
  Write-Host "Installed '$Name' ($Plan)"
}
if ($On) {
  if (-not (Test-Plan)) { throw 'Run with -Install first.' }
  powercfg /setactive $Plan
  powercfg /overlaysetactive $BestPerformanceOverlay
  Write-Host "Active: $Name + Best performance"
}
if ($Off) {
  powercfg /setactive $Balanced
  powercfg /overlaysetactive $BalancedOverlay
  Remove-ItemProperty -Path 'HKCU:\Software\InfernoArmAsio' -Name PreviousPowerPlan, PowerPlanOwner -ErrorAction SilentlyContinue
  Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name InfernoArmAsioPowerRestore -ErrorAction SilentlyContinue
  Write-Host 'Active: Balanced'
}
if ($Uninstall) {
  if (Test-Plan) {
    if ((powercfg /getactivescheme) -match $Plan) { powercfg /setactive $Balanced }
    powercfg /delete $Plan
    Write-Host "Removed '$Name'"
  }
}
