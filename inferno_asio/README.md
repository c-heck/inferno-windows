# Inferno ARM ASIO - experimental

Windows ARM64 and ARM64EC user-mode ASIO device based on
[Inferno Windows](https://github.com/legopc/inferno-windows) and
[Inferno](https://github.com/teodly/inferno).
This is unofficial software, not Audinate Dante Virtual Soundcard, and is not
authorized, certified, or supported by Audinate. Do not use it for critical audio.

## Status and scope

**Experimental network status:** the previous ARM64EC release used unicast PTP
DelayReq packets and reproducibly caused a genuine DVS follower to lose clock
lock on the tested Yamaha-led network. The ARM64EC release has now been rebuilt
with multicast-only PTP DelayReq and no automatic unicast fallback. A 10-minute
clock-only coexistence test passed with both DVS clocks green and monitored
audio clean. The ASIO receive path still needs testing against Dante with this
build; this is not a certified or production-ready replacement.

The former ASIO placeholder has been replaced with a COM ASIO DLL, double-buffered
PCM input/output, software-clocked network integration, a diagnostic host, and
a native Win32 control panel.
An executable build or a passing loopback test does **not** establish Dante
interoperability or dropout-free performance. Real Dante devices and
DAW acceptance tests are still required.

Local validation completed for this prototype: ARM64 and ARM64EC release DLLs and
diagnostic and control-panel executables, 30 focused unit and socket-compatibility
tests per architecture (including synthetic loopback-UDP PTP lock/leader-loss
tests), 36 actual DLL audio-loopback profiles per architecture, and strict
Clippy checks for the ASIO crate. ARM64EC was also tested from a genuine x64
diagnostic host: another 36 profiles exercise mixed-ABI calls and audio callbacks.
The GUI smoke test exercises native controls,
save/reload, and an actual DLL loopback using the saved profile.
Live ARM64EC testing with Nuendo 15 and a Yamaha MY16 Dante transmitter at
48 kHz, four configured channels, 512 ASIO frames and 10 ms network buffering
produced green Controller routing check marks and user-confirmed Nuendo input
audio after the discovery fixes. Other rates/channel counts, transmit audio,
long-duration dropout performance and physical latency remain unverified.

| Sample rate | Maximum simultaneous inputs / outputs |
|---|---|
| 44.1 kHz | 64 / 64 |
| 48 kHz | 64 / 64 |
| 96 kHz | 32 / 32 |

- Network buffer: **20, 10, 6, or 4 ms**, default **20 ms**.
- ASIO buffer: **64, 128, 256, 512, 1024, or 2048 samples**. Select the preferred
  size in the control panel; default **512**. Hosts can still request other
  supported sizes.
- ASIO sample format: signed 32-bit little-endian PCM. Inferno currently
  advertises 24-bit network audio, left-justified in the ASIO sample words.
- Network and ASIO buffers are distinct. Neither setting is total round-trip
  latency. The DLL reports its local buffering in ASIO frame units; remote
  endpoint buffers and network delay are additional.
- Both network directions use the same local sample rate and network setting.
  Remote receivers control their own playout latency.
- The receive setting is a local minimum; a remote source may require a higher
  negotiated receive latency. The driver reports increases through ASIO latency
  notifications. Physical end-to-end delay still needs measurement.
- ASIO-only: no WDM/WASAPI endpoint, kernel driver, SYSVAD, VB-Cable, ASIO4ALL,
  test signing, or Secure Boot changes are required.
- Pure ARM64 DAWs need the pure ARM64 DLL. ARM64EC DAWs (including Nuendo 14
  for Windows on Arm) and x64-emulated hosts need the separate ARM64EC DLL.
  An ARM64EC/x64 process cannot load the pure ARM64 DLL. The ARM64EC build
  compiles the driver/engine for ARM64EC; it is not an x64-only driver relabeled
  as ARM64EC. Actual Nuendo operation still requires acceptance testing.
- One active network instance per machine/interface is supported. PTP UDP ports
  are bound exclusively; another PTP service can prevent startup.

## Build on Windows ARM64

Install native Rust (`aarch64-pc-windows-msvc`), Visual Studio 2022 Build Tools
with C++ ARM64 tools, and Windows 11 SDK ARM64 libraries. Then, from the repository:

```powershell
.\scripts\build-arm64-asio.ps1
```

The script runs focused tests and builds:

```text
target\release\inferno_asio.dll
target\release\asio_probe.exe
target\release\inferno_control.exe
```

It statically links the Microsoft C runtime. If a normal tool installation is
unavailable, `-AdditionalLibraryPaths` accepts already-provisioned ARM64 MSVC
library directories. Tool/library provisioning is not performed by the script.
Builds do not register the driver or modify networking.

### ARM64EC build for Nuendo 14 on Windows on Arm

Install both Visual Studio components **MSVC v143 ARM64 build tools**
(`Microsoft.VisualStudio.Component.VC.Tools.ARM64`) and **ARM64EC build tools**
(`Microsoft.VisualStudio.Component.VC.Tools.ARM64EC`), plus the Windows 11 SDK.
Use a current toolset with its hybrid ARM64 CRT/SDK libraries; ordinary x64 CRT
libraries alone do not provide the ARM64EC interop helpers.

```powershell
rustup target add arm64ec-pc-windows-msvc
.\scripts\build-arm64-asio.ps1 -Architecture ARM64EC
.\scripts\test-arm64-asio.ps1 -Architecture ARM64EC
```

The separate output directory is:

```text
target\arm64ec-pc-windows-msvc\release\inferno_asio.dll
target\arm64ec-pc-windows-msvc\release\asio_probe.exe
target\arm64ec-pc-windows-msvc\release\inferno_control.exe
```

Run the control panel in that directory for its matching offline diagnostic.
Both control-panel builds use the same per-user settings file.
ARM64EC images normally report machine `0x8664` with ARM64X/CHPE metadata;
checking only `0xAA64` would incorrectly reject them. The registration/test
scripts inspect the metadata and reject a mismatched architecture.

For mixed-ABI validation, the matrix also accepts `-Probe` pointing to an
x64 build of the diagnostic host; use `-Architecture ARM64EC` for the DLL.

The older mDNS stack depends on socket2's 0.5 API. Its narrowly patched
[vendored copy](../vendor/socket2/ARM64EC.md) uses ARM64EC-aware Windows bindings;
other Windows dependencies were updated to remove architecture-unaware bindings.
Network-state files remain under the same local application-data directory.

## Graphical control panel

Run `target\release\inferno_control.exe`, or use your DAW's ASIO control-panel
button. Keep it alongside `inferno_asio.dll` and `asio_probe.exe`. The panel
uses a compact dark layout (dark title bar on Windows 10 20H1+ / 11).

- Choose the Ethernet adapter's IPv4 address, sample rate, input/output count,
  and 20/10/6/4 ms network latency.
- Choose **ASIO buffer**: 64, 128, 256, 512, 1024, or 2048 samples.
  This is the preferred size reported to Nuendo and other ASIO hosts.
- **Dante device name**: empty uses the generated default; otherwise 1–31
  letters, digits or `-` (not at the start or end).
- **Switch to low-latency power plan while streaming**: on by default. When
  off, the driver leaves the current Windows power plan untouched.
- Select network operation or offline diagnostic loopback.
- **Save**. A streaming driver checks the settings file every 0.5 s and, on
  a change, sends the ASIO reset request; the DAW then reopens the device with
  the new settings, so no manual driver restart is needed. Hosts that ignore
  reset requests still need the ASIO device reopened.
- **Run offline audio test** checks the actual sibling DLL using the selected
  profile, without registration, network traffic, or changing your saved mode.

Settings are stored atomically in `%LOCALAPPDATA%\InfernoArmAsio\settings.json`.
The `asio_frames` field stores the preferred ASIO size; older settings files
without this field use the 512-sample default. Explicitly saved sizes are preserved.
`INFERNO_ASIO_CONFIG` can select another settings file. Existing
`INFERNO_ASIO_*` environment variables override the saved fields; remove them
if you want the GUI settings to take effect. A missing file uses defaults;
invalid saved settings stop driver initialization with an explicit error.
The GUI shows the load error and defaults, allowing an explicit save to repair
the file.

The panel does not display live engine/PTP lock status, route Dante channels,
install/register the driver, or configure the firewall. Network audio starts
inside the DAW; routing still requires an external controller.

For an automated GUI check (temporary settings only):

```powershell
.\target\release\inferno_control.exe --smoke-test
```

## Offline diagnostic test - no registration or network access

```powershell
.\scripts\test-arm64-asio.ps1
```

This loads the actual DLL with `LoadLibrary`, creates the COM ASIO instance,
and verifies all-channel bit-exact local loopback, time-info callbacks,
sample positions, and start/stop/restart. It exercises all twelve rate/network
setting combinations at 64, 256, and 2048 ASIO frames (36 profiles).
The matrix is isolated from saved GUI settings and restores environment overrides.
The configured network buffer is validated, but loopback bypasses networking:
these tests do not measure packet jitter, PTP accuracy, or network latency.

For one diagnostic profile:

```powershell
$env:INFERNO_ASIO_MODE = 'loopback'
$env:INFERNO_ASIO_RATE = '96000'
$env:INFERNO_ASIO_NETWORK_MS = '20'
.\target\release\asio_probe.exe "$PWD\target\release\inferno_asio.dll"
```

## Register for your DAW

Close audio hosts first. From **elevated 64-bit PowerShell**, explicitly run:

```powershell
.\scripts\register-arm64-asio.ps1
# Instead, for Nuendo 14 WoA / ARM64EC hosts:
.\scripts\register-arm64-asio.ps1 -Architecture ARM64EC
```

This writes only this device's ASIO and COM entries under 64-bit HKLM.
No registration is performed automatically. Keep the DLL at the registered path.
The builds share one driver name/CLSID: register only the one matching your DAW;
registering the other build switches the existing entry rather than installing
two independent devices. Use `-WhatIf` to inspect registration without changing it.
In the DAW, select **Inferno ARM ASIO**. To remove the registration:

```powershell
.\scripts\register-arm64-asio.ps1 -Action Unregister
```

### Rolling out to other Windows on Arm machines

Each machine needs these steps:

1. Copy `inferno_asio.dll`, `inferno_control.exe` and `scripts\` to a fixed
   path. Use the ARM64EC build for Nuendo/Cubase on Windows on Arm.
2. As administrator: `.\scripts\register-arm64-asio.ps1 -Architecture ARM64EC`
   (ASIO/COM registration) and `.\scripts\open-firewall-admin.ps1`.
3. As administrator: `.\scripts\tune-dante-nic.ps1`. It picks the adapter
   that owns the configured `bind_ip`, or the only wired adapter (otherwise
   pass `-Adapter`). It enlarges the receive and transmit queues using only
   the keywords that driver supports (Realtek USB, Intel and standard `*`
   keywords), clamped to their valid ranges. It disables receive coalescing,
   EEE, interrupt moderation, flow control and device power saving, and
   pre-creates the power plan.
4. Per user, in the control panel: select the wired adapter address, sample
   rate, channels and latency.

Nothing else is machine-specific. The power plan uses a fixed GUID,
`{6a3f5c1e-8d2b-4e7a-9c41-1f0d2b7e5a90}`, and the driver creates it on
demand if step 3 was skipped. Crash recovery (guard process and logon
entry) needs `inferno_control.exe` next to `inferno_asio.dll`, which step 1
provides. It runs per user without admin rights. It only touches standard
Windows power settings; settings a platform does not expose are ignored. For managed
deployment (Intune, SCCM, GPO startup script), steps 2–3 can run
unattended. Users can turn the power-plan switch off in the control panel
(`"power_plan": false` in `settings.json`), and `INFERNO_ASIO_POWER_PLAN=off` can be set as a machine
environment variable where IT policy owns power settings; the variable wins. A Dante-dedicated
wired adapter (PCIe or a good USB 3 adapter on its own port, not a dock) is
still recommended.

## Network configuration

Use the control panel above, or set overrides in the same shell
**before starting the DAW**; do not use `setx`.
Replace the example IP with the wired Ethernet adapter's own IPv4 address:

```powershell
$env:INFERNO_ASIO_MODE = 'network'
$env:INFERNO_ASIO_BIND_IP = '192.168.10.20'
$env:INFERNO_ASIO_RATE = '48000'
$env:INFERNO_ASIO_NETWORK_MS = '20'
# Optional reduced channel count; otherwise the saved GUI setting applies.
$env:INFERNO_ASIO_CHANNELS = '2'
# Launch the DAW matching your registered DLL architecture from this shell.
```

Do not leave `INFERNO_ASIO_MODE=loopback` set when testing Dante. Diagnostic
loopback intentionally does not announce a network device.

Network mode requires a wired Ethernet network, a working external PTPv1 leader,
and appropriate firewall permissions for the DAW process (the DLL runs inside it).
PTP uses UDP 319/320 and 224.0.1.129. Inferno also needs mDNS UDP 5353,
its Dante discovery/control ports, and negotiated media UDP ports; consult
the upstream networking documentation and your network administrator.
No firewall rule is created automatically. Other Inferno/DVS instances can
conflict with control ports. Start on an isolated test network at low monitor
levels, not on a production Dante installation.
The ASIO build uses the existing alternate-port mechanism: ARC 45000, CMC
45001, flow control 45002, and status requests 45003. Discovery advertises
these endpoints so Audinate Controller's local monitoring service can retain
the standard 8700/8800 ports. Allow these alternate ports for the DAW as well.
ARC property queries now report the unnamed audio clock domain, configured
receive/transmit latency, and supported transmit packet sizes, with real
record counts and packet-relative offsets. Unknown properties are explicitly
reported unavailable rather than replaced by a success-shaped zero payload.
These properties are read-only, except the ones below.

### Dante Controller integration

- **Rename** (Device Config → Rename Device) and **Device Latency** are
  accepted from Controller. They are written to `settings.json` (latency is
  snapped to 4/6/10/20 ms), and the driver then asks the DAW for an ASIO reset
  so the new name/latency take effect. **Reboot** also triggers a reset.
  Invalid names are rejected.
- **Latency indicator follows the driver.** The settings reply reports the
  running latency as default, configured and active (0x8204/0x8205/0x8301),
  with a 4–20 ms range (0x8306/0x8302). Controller caches these values across
  a device restart and does not re-read them on its own. So for 1, 4 and 10 s
  after each start, the driver multicasts a "Property Change" notification
  (id 262) to 224.0.0.231:8702, and Controller then re-reads them. Live test
  with Controller 4.18.1.1: before this, Controller sent no settings read
  after a restart. With it, each notification was followed by a read within
  10 ms. After `network_ms` was changed from 20 to 6 in `settings.json` and
  the driver restarted, Controller read 6 ms. Changes made in the Inferno
  panel therefore show up in Controller once the DAW has reset the driver.
- The status heartbeat now reports real TX/RX bandwidth, cumulative missing
  receive packets (Packet Errors), per-flow late packets (Latency status) and
  per-channel signal levels, which drive the green signal icons on the
  Transmit/Receive tabs. Levels use the Dante scale (-2 × dBFS, 0xc1 silence).

Controller-side sample-rate, encoding and
pull-up configuration remain unsupported; use the Inferno panel/DAW for the
supported local settings. A property response alone does not establish that
Controller can complete a subscription or that network audio is working.

Channel discovery now decodes mDNS NSEC records (which previously panicked
inside the DNS dependency) and accepts the legacy `dbcp` TXT key when `dbcp1`
is absent. A live Yamaha MY16 channel lookup returned its flow-control endpoint,
24-bit encoding, packet sizes and latency successfully. After rebuilding and
reopening Nuendo, the user confirmed green Controller routes and input audio.
The lookup alone does not verify flow acceptance or received PCM.
An opt-in diagnostic exercises the actual
channel resolver without changing transmitter subscriptions:

```powershell
$env:INFERNO_MDNS_TEST_INTERFACE = '192.168.69.61'
$env:INFERNO_MDNS_TEST_HOST = 'O2RSlot4'
$env:INFERNO_MDNS_TEST_CHANNEL = '01'
cargo test -p inferno_aoip --target arm64ec-pc-windows-msvc --lib live_channel_resolution -- --ignored --nocapture
```

Run from the same MSVC ARM64 build environment used for the driver. Normal
tests skip this network-dependent diagnostic.

Transmit flow negotiation now follows the request's packet-relative parameter
descriptor offset instead of assuming an inline layout. A captured legacy
Yamaha request places that descriptor after its receiver hostname; the old
parser read hostname bytes as packet size and flow-name offset. Regression
tests cover that request, the modern inline layout and invalid offsets.
After rebuilding ARM64EC and reopening Nuendo, the user confirmed green
Inferno-to-O2R routing check marks; a live ARC query also confirmed one active
transmit flow. Return PCM fidelity and long-duration performance remain unverified.

### Live clock diagnostic

Close the DAW first so UDP 319/320 are available. With network mode and the
Ethernet address saved in the panel, run from the matching build environment:

```powershell
cargo test -p inferno_asio --target arm64ec-pc-windows-msvc --lib live_network_clock_lock -- --ignored --nocapture
```

This opt-in test sends PTP DelayReq messages, waits up to 18 seconds for
clock lock, and checks that measurements stay current for another 30 seconds,
then releases its sockets. It does not start the Dante discovery
server or transmit audio. Debug output distinguishes incoming Sync/FollowUp,
DelayReq transmission, and received DelayResp measurements. Receiving Sync
alone is not sufficient for lock. Firewall permissions must cover the test
executable as well as the DAW when testing each.
For an approved longer coexistence test, set `INFERNO_PTP_TEST_SECONDS` to a
positive integer (for example, `600` for ten minutes). This changes only the
ignored diagnostic's stability duration; the default remains 30 seconds and
the initial lock timeout remains 18 seconds. Remove the variable afterward.
For destination comparison only, `INFERNO_PTP_TEST_DESTINATION` overrides the
ignored test's request destination (`IPv4:port`); it never changes production
driver settings. Unicast diagnostics have reproducibly disrupted another
receiver's clock lock. Run only in an approved, stopped-playback test window,
not on a production audio network.

The ASIO engine uses Inferno's realtime receive API, not its 50 ms periodic
WASAPI callback. Outgoing samples are positioned explicitly in the PTP media
timescale. Channel profiles change through ASIO sample-rate calls only when
buffers have been disposed; changing between 96 kHz and lower rates requires
channel re-enumeration and buffer recreation.

## Clock implementation and limits

The ASIO path supplies its own PTPv1 clock to the network engine; it does not
accept the upstream Windows system-clock stub as synchronized time.
The experimental slave supports the `_DFLT` domain, one-/two-step Sync and
FollowUp correlation, DelayReq/DelayResp matching, path-delay estimation,
and software phase/frequency correction. It never sets Windows time or elects
itself clock leader. Three accepted measurements are required before startup.
Sync/FollowUp are received on the multicast group. Source now sends DelayReq
to the default-domain multicast group on UDP 319, from an interface-bound
ephemeral source socket, at most once per second. Requests carry learned time,
grandmaster and parent datasets, a leader-epoch timestamp and slave-only clock
quality. No leader address is hardcoded and there is no automatic unicast fallback.

Clock-only isolation reproduced DVS unlock with the previous unicast packets and
again with corrected-dataset unicast packets. Captures showed genuine DVS
multicast DelayReq messages without matching leader responses during the old
unicast test; replies returned after it stopped. Earlier multicast tests did
not disturb DVS but received no matching DelayResp and timed out without lock.
The ARM64EC release DLL was subsequently rebuilt with the multicast-only
candidate; the live clock-only retest and initial ASIO receive test are
documented below.

An Npcap capture on Madi-PC, on the same switch as the ARM64 PC, confirmed
that Inferno multicast DelayReq packets reached that branch. It also captured
Yamaha multicast Sync and DelayResp packets addressed to Madi-PC, but no
DelayResp addressed to Inferno during the 18-second diagnostic. Madi-PC
remained synchronized. Yamaha is several switches downstream, so this does
not prove that Inferno's requests reached its port or were accepted by it.

A subsequent capture on a Linux host connected to the studio switch saw
neither Inferno's DelayReq packets nor Madi-PC's DelayReq packets and
DelayResp replies during the multicast diagnostic. Inferno's diagnostic log
did receive Madi-PC requests during that same interval, and the Linux capture
saw Madi-PC requests and Yamaha replies both before and after it. Madi-PC's
clock indicator stayed green. This shows that capture visibility changed,
but does not establish why, or whether Yamaha received those requests.
The Linux host was not joined to the PTP multicast group when checked afterward;
its capture port may therefore not receive all group traffic on a snooping switch.

After these captures, the request builder was changed to zero the reserved
bytes preceding the grandmaster variance while preserving its 16-bit value.
In a subsequent clock-only test with the Linux capture host temporarily joined
to the PTP multicast group, Inferno reported lock and maintained current
measurements for 30 seconds; the user reported that Madi-PC stayed locked.
The studio-side capture contained all 32 Inferno DelayReq messages and matching
Yamaha DelayResp messages, along with 18 Madi-PC requests and matching replies
during the test. The Linux capture again stopped seeing most Madi-PC requests
after the test, so capture visibility remains uncertain. The user also reported
that Madi-PC's Dante Controller status page was unresponsive even though audio
continued. The cause is unknown. The reserved-byte change and temporary group
membership were both different from the earlier test, so neither can be credited
as the cause of the successful lock. Longer coexistence and audio tests remain
necessary before any release deployment.

After moving Yamaha, Madi-PC and the Linux host onto the Aruba 2920, a repeat
without the Linux multicast join timed out after 18 seconds. ARM64 remained
behind the consumer Netgear switches, and the devices retained their original
general-LAN addresses rather than moving to the isolated Dante VLAN. The
diagnostic logged nine multicast requests, Yamaha Sync/FollowUp and Madi-PC
requests/replies, but no accepted Inferno delay measurements. The user reported
that Madi-PC stayed locked, Controller stayed responsive and audio continued
without a noticed interruption. At that stage, the earlier successful lock had
not reproduced without the temporary join.

On November 8, ARM64 was connected directly to Aruba port 3 using a USB Ethernet
adapter at 192.168.69.76 (Ethernet 4); Yamaha and Madi-PC were on ports 2 and 4.
Wi-Fi remained connected separately. Two multicast-only diagnostics, without
any Linux multicast join, acquired clock measurements and maintained them for
30 seconds each. Madi-PC stayed locked and Controller remained responsive in
both tests; audio was monitored during the second and no interruption was
noticed. The switch path, adapter, MAC and IP changed together, so these results
do not identify the earlier failure's cause. Longer clock coexistence and
Inferno audio tests remain necessary.

The subsequent ten-minute diagnostic stopped early after 176 seconds. Yamaha
Sync/FollowUp and matching Inferno DelayResp measurements continued up to the
failure. The last logged path estimate was 22.24 ms, above the servo's 20 ms
acceptance limit. The worker then published `None` when the servo rejected
a measurement, immediately clearing a previously valid clock overlay.
Source now retains the last valid overlay after an isolated path/interval
rejection or offset jump over 5 ms, without refreshing its timestamp. Three
seconds without accepted measurements clears the overlay and resets acquisition
even if Sync packets continue. Rejected samples are logged, and synthetic tests cover outlier retention,
recovery and expiration with continuing Sync but no accepted delay
measurements.
The 20 ms path limit is unchanged; the earlier failure is not evidence that
multicast replies stopped.
The user reported continuous monitored audio and responsive Controller, but
did not observe Madi-PC's clock indicator at the failure.

After isolating all four endpoints on VLAN 20, the corrected diagnostic passed
a 30-second stability test on ARM64 at 169.254.198.153 (port 21). Yamaha was
169.254.240.71 (port 22), Madi-PC 169.254.117.186 (port 23), and the additional
DVS host SFN-Madi 169.254.24.175 (port 24). The user monitored Yamaha through
SFN-Madi and back to Yamaha, reporting continuous clean audio, both DVS clocks
green and responsive Controller. This short test does not replace the pending
ten-minute run or establish production audio reliability.

The corrected ten-minute VLAN 20 run stopped after 263 seconds. Matching Yamaha
replies continued; the final measurement had a 7.59 ms path estimate and an
approximately 7.09 ms offset jump, triggering the servo's 5 ms discontinuity
reset. The user reported continuous clean SFN-Madi audio, both DVS clocks green
and responsive Controller. Raw timing logs and packet capture were then added.

The Wireshark capture on ARM64 Ethernet 4 had zero drops and correlated the next
failure. Yamaha Sync/FollowUp timestamps remained spaced about 250 ms apart,
but their observed Ethernet arrival intervals were about 280 ms then 220 ms.
The delayed Sync produced a 16.45 ms path estimate and a 15.96 ms phase-offset
jump; its matching multicast DelayResp arrived promptly. This identifies Sync
arrival jitter on the host's network/capture path as the trigger, but does not
distinguish switch forwarding from USB NIC/driver/Windows scheduling delay.
The servo now rejects an isolated >5 ms offset jump without discarding the last
valid overlay; its unchanged 3-second freshness timeout still clears the clock
if accepted measurements do not resume. Offline tests cover retention and
recovery.

The subsequent multicast-only live retest on Ethernet 4 passed: PTP lock
remained fresh for all 600 seconds (test runtime 612.44 seconds). The
corresponding 614.53-second capture contains 6,693 UDP 319/320 packets; a
capture-drop count was not available. The servo rejected one 37.93 ms path
sample and a separate 5.9852 ms offset jump, then maintained fresh lock through
the end of the test. This validates the outlier-retention behavior on the
tested network, not ASIO audio or end-to-end Dante compatibility. The user has
confirmed both DVS clocks remained green, Dante Controller stayed responsive
with no sync warnings or adverse log entries, and the Yamaha → SFN-Madi →
Yamaha audio path had no glitches.

In the first live ASIO receive attempt, Nuendo initially did not make Inferno
visible in Controller. Switching the active ASIO driver away and back to
Inferno caused it to appear. At 48 kHz, 512 ASIO frames, 10 ms network buffering
and 10 channels, the user subscribed Yamaha transmit channels 1–10 to Inferno
and observed audio in Nuendo, but could not yet monitor it through physical
outputs. MADI-PC disappeared temporarily from Controller, then returned; the
user reported all three Dante devices synchronized, no clock warnings and no
audio glitches. Controller was visually flashing and difficult to operate, so
the cause remains undetermined.

A 90-second Ethernet 4 capture during this active test recorded Inferno's
multicast PTP DelayReqs and matching Yamaha DelayResps (88 of each), its
`Inferno ARM ASIO` ARC/CMC and 10-channel mDNS advertisements, and zero capture
drops. This confirms discovery and clock exchanges on the tested interface;
it does not confirm audible receive monitoring, transmitted PCM, or the cause
of Controller's visual behavior.

On the subsequent round-trip attempt, the user reported fluctuating Yamaha
latency, orange Inferno subscriptions, red Inferno sync and an audio cutoff,
while SFN-Madi remained stable. The Inferno ASIO buffer was 512 samples and
the network buffer 20 ms. A 38-second Ethernet 4 capture observed all 37
Inferno multicast DelayReqs matched by Yamaha DelayResps; it did not show
missing PTP replies as the cause. The capture ended when audio stopped and is
too short to establish timestamp/servo stability or identify the dropout
mechanism. Further multichannel playback is paused pending a reduced-channel
diagnostic with PTP/servo logging.

Dante Controller's Latency tab on O2RSlot4 then reported Inferno's flow with
180 late packets in 2:28 (peak 67.8 ms, average 5.2 ms), while Madi-PC had no
late packets. Clock Status showed Inferno with red sync and v1 Multicast
`N/A`. Investigation found that the clock-status reply (`0x0020`, answering
query `0x0021`) is sent only when a PTP daemon has written
`clock-stats.<MAC>0000` to `/tmp`. On Windows, this file was never written.
The PTPv1 follower now writes the leader EUI-64 to the OS temp directory while
locked and removes it when lock is lost. `inferno_aoip` reads that directory
on Windows. This change has passed unit tests but has not been verified live.

After unsubscribing O2RSlot4 from Inferno, Controller could not resubscribe,
and Inferno's patch cells kept flashing between orange and green. On startup
`inferno_aoip` recreated saved unicast TX flows from `tx_flows.toml` under
handles the receiver had never seen, so the receiver could not tear them down.
Expired flows also stayed in the TX flow list until the next flow was added.
The ASIO engine now disables restoring unicast TX flows (receivers re-request
them), and the TX flow query drops expired unicast flows before replying. A
4-channel Inferno→O2R flow captured over 120 s had no sequence gaps or zeroed
samples, and packets left within about 0.75 ms of their PTP timestamp.

Live retesting still found unpatchable channels and a Controller 4.18.1.1
`IndexOutOfBoundsException` while resetting the RX routing model (row 160 in
a ten-channel list). The stack trace alone does not identify the offending
reply. Direct read-only ARC queries to O2RSlot4 showed routing properties
`0x0201`, `0x0303`, `0x0310`, and `0x0311` missing from Inferno's property
directory; these now have explicit responses. Channel pagination also
underreported the descriptor count by one when a page crossed the packet
size limit. Regression tests cover that boundary and complete ten- and
32-channel enumeration, but confirming the Controller crash and patching
are resolved still requires a live Controller/Nuendo retest.

Loopback captures then showed Controller resetting Inferno's session 2–3
times per second whenever Inferno reported any TX or RX flow. Disassembly of
`DanteController.dll` found the cause. Controller speaks ARC using the
`arcp_vers` advertised over mDNS and echoes it as the packet start code.
Inferno advertised `2.2.0`. Below version `0x2600`, Controller ignores the
max-TX/RX-flow fields of the `0x1000` capabilities reply, so it reads both
limits as zero. Every reported flow id then fails the "Invalid ID" check, and
Controller resets the router model. That reset produced the flashing patch
cells, the unpatchable channels and the `IndexOutOfBoundsException`.

Inferno now advertises `arcp_vers=2.7.41` (`0x2729`, as on the Yamaha DCM).
The TX (`0x2200`) and RX (`0x3200`) flow replies use the DCM's forward layout
with socket descriptor `0x0802`. The `0x2204` query is answered with result
`0x0030` and no body, and the Lock Status query `0x1008` is answered with
`0x1009`. In a 60-second loopback capture, Controller completed one query
pass followed only by periodic `0x3000` status polls. All ten subscribed RX
channels reported "receiving". A live Controller/Nuendo retest confirmed that
patching works and latency is reported.

### TX latency jitter and return-channel dropouts

The Yamaha Latency tab showed a wide, spiky distribution for the Inferno flow,
and the return channel had audible dropouts. The engine log showed
`tx lag ... dropout occurs!`. The TX sender thread had stalled for longer than
the TX latency, so it skipped ahead and created a timestamp discontinuity. The
TX thread had only `THREAD_PRIORITY_HIGHEST`. The ASIO engine thread and the
network runtime workers were unboosted, so they all competed with the DAW's
MMCSS Pro Audio threads.

Changes:

- The `flows_tx`/`flows_rx` threads, the ASIO engine thread and the engine's
  tokio workers (PTP follower, device server) now register with MMCSS
  "Pro Audio" at critical priority, falling back to the highest thread
  priority.
- The engine requests a 1 ms system timer resolution while it runs.
- The TX sender catches up after stalls of up to 20 ms instead of jumping
  ahead whenever a stall exceeded the TX latency.

These figures are from 48 kHz, 10 channels, 4 ms network latency, captured
on the wire against the Yamaha DCM:

| | before | after |
|---|---|---|
| `tx lag` dropouts | 8 in ~3 min | 0 in ~2.5 min |
| TX timestamp discontinuities | 2 / 30 s | 0 / 90 s |
| max send gap | 5.67 ms | 4.09 ms |
| send lateness p99.9 | 0.91 ms | 0.71 ms |

`Lost 512 samples (reorder buffer timeout)` messages on RX matched packets
that were also missing in an Npcap capture, so the losses happen below
Inferno. On the test machine, a Realtek USB GbE adapter stalled for 25–70 ms
and then dropped a burst of packets, because its default receive queues are
small (6 URBs, 16 buffers). `scripts\tune-dante-nic.ps1` (run as
administrator) enlarges these queues and disables USB selective suspend,
receive coalescing and device power saving. In 3-minute captures this cut the
RX gaps from 27 to 2.

The remaining 25–70 ms stalls came from platform power management. The
Balanced scheme on the Snapdragon X Elite test machine had PCIe ASPM at
maximum power savings and a 48 % minimum processor state. While audio
streams, the driver now switches to its own "Inferno Low Latency" plan
(`src/power.rs`). The plan has a fixed GUID and is created on demand without
admin rights as a copy of the active plan. It applies these settings on AC
and on battery:

- minimum processor state 100 %
- no core parking
- energy preference: performance
- PCIe ASPM, USB3 link power management and USB selective suspend off

The driver also selects the "Best performance" power mode. When the engine
stops, it restores the previous plan and power mode, so the user's own AC and
battery settings apply whenever no DAW is streaming. If the user selected
another plan during the session, the driver leaves that plan in place.

The previous plan and the owning host process are recorded in
`HKCU\Software\InfernoArmAsio`. Crash recovery works in two layers, so the
user's settings come back even if the DAW never stops the engine:

- **Host crash or kill.** When the plan is activated, the driver starts a
  detached, windowless `inferno_control.exe --power-guard <host pid>`. It
  waits on the host process. If the host exits while it still owns the plan,
  the guard restores the previous plan and power mode within about a second.
  It exits on its own after a normal stop.
- **Reboot, logoff or power loss.** While the plan is active, a per-user
  `HKCU\...\CurrentVersion\Run` entry `InfernoArmAsioPowerRestore` runs
  `inferno_control.exe --power-restore` at the next logon. It restores the
  plan unless the owning host is running again. The entry is deleted with
  every restore.

Live tests with `asio_probe` covered three cases: a hard kill
(`Process.Kill`), a normal stop, and a simulated reboot where host and guard
were both killed and the Run entry was then executed. Each time Balanced came
back and no guard, Run entry or saved state was left behind. The next engine
start also repairs a stale plan, and `scripts\inferno-power.ps1 -Off`
restores it manually. Set `INFERNO_ASIO_POWER_PLAN=off` to disable switching
entirely.

After this, a 3-minute capture showed no RX gaps, no log losses and no stall
longer than 5.3 ms. The previous run had 77 stalls over 4 ms, up to 70 ms.

The driver also opts its audio and packet threads out of EcoQoS power
throttling. It opts the host process out of timer-resolution throttling, so
Windows 11 honours the 1 ms timer while the DAW is minimized.

No leader, invalid measurements, or stale synchronization stop audio with an
ASIO error rather than falling back to free-running network timestamps.

Limitations include userspace timestamps, a basic experimental servo, no
hardware timestamping, no full BMCA/boundary-clock implementation, no PTPv2,
no Dante Domain Manager/security support, and no certified device compatibility.
Windows clock adjustments can force a restart. Lower buffer settings, packet
reordering, network loss, leader changes, sample-rate changes, and long-duration
clock drift require physical-network testing.

Errors are available via ASIO `getErrorMessage` and Windows debugger output
(`OutputDebugString`). The ASIO control-panel call launches the sibling
`inferno_control.exe`; launch failures are reported through ASIO errors.

## Required hardware acceptance tests

1. Verify device discovery and both routing directions in Dante Controller.
2. Verify two-channel bidirectional PCM and clock lock against a real endpoint.
3. Test full 64x64 at 44.1/48 kHz and 32x32 at 96 kHz.
4. Measure end-to-end latency and underruns at each requested network buffer.
5. Run long-duration drift tests and recover cleanly from cable/leader loss.
6. Repeat in the actual native ARM64 DAW and Ethernet adapter configuration.

Until these pass, this is a development prototype, not a validated DVS replacement.

## Licensing

The new ASIO implementation is GPL-3.0-or-later, consistent with Inferno AoIP's
GPLv3-or-later / AGPLv3-or-later dual licensing. Existing upstream files retain
their own notices. No proprietary Audinate SDK or Steinberg SDK source is bundled;
the ASIO ABI declarations are implemented directly. Distribution must preserve
the applicable licenses and provide the corresponding source as required.
ASIO and Dante are their respective owners' trademarks.
