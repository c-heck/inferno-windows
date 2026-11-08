//! Switches Windows to the "Inferno Low Latency" power plan while the engine streams.
//!
//! On Snapdragon laptops the default Balanced plan parks cores, lowers the minimum CPU state
//! and enables PCIe ASPM / USB link power management. Waking from those states stalled USB
//! Ethernet for 25-70 ms and dropped Dante packets. The plan has a fixed GUID so it is the same
//! on every machine, is created on demand without admin rights, and is only active while audio
//! runs (AC and battery). The previous plan and power mode are recorded in HKCU and restored
//! when the engine stops. If the host dies without stopping the engine, a detached
//! `inferno_control.exe --power-guard <pid>` process restores them as soon as the host exits;
//! a HKCU Run entry (`--power-restore`), present only while the plan is active, covers
//! reboots, logoff and power loss.
//! Set `INFERNO_ASIO_POWER_PLAN=off` to leave power settings alone.

use std::{env, ffi::c_void, path::PathBuf, ptr};

use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    },
};

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    Guid { d1, d2, d3, d4 }
}

/// {6a3f5c1e-8d2b-4e7a-9c41-1f0d2b7e5a90}
const PLAN: Guid = guid(0x6a3f5c1e, 0x8d2b, 0x4e7a, [0x9c, 0x41, 0x1f, 0x0d, 0x2b, 0x7e, 0x5a, 0x90]);
const BALANCED: Guid = guid(0x381b4222, 0xf694, 0x41f0, [0x96, 0x85, 0xff, 0x5b, 0xb2, 0x60, 0xdf, 0x2e]);
const BEST_PERFORMANCE_OVERLAY: Guid =
    guid(0xded574b5, 0x45a0, 0x4f42, [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38]);

const SUB_PROCESSOR: Guid = guid(0x54533251, 0x82be, 0x4824, [0x96, 0xc1, 0x47, 0xb6, 0x0b, 0x74, 0x0d, 0x00]);
const SUB_PCIE: Guid = guid(0x501a4d13, 0x42af, 0x4429, [0x9f, 0xd1, 0xa8, 0x21, 0x8c, 0x26, 0x8e, 0x20]);
const SUB_USB: Guid = guid(0x2a737441, 0x1930, 0x4402, [0x8d, 0x77, 0xb2, 0xbe, 0xbb, 0xa3, 0x08, 0xa3]);

/// (subgroup, setting, value) applied to both AC and DC.
const SETTINGS: [(Guid, Guid, u32); 6] = [
    // Minimum processor state 100 %.
    (SUB_PROCESSOR, guid(0x893dee8e, 0x2bef, 0x41e0, [0x89, 0xc6, 0xb5, 0x5d, 0x09, 0x29, 0x96, 0x4c]), 100),
    // Core parking: keep 100 % of cores unparked.
    (SUB_PROCESSOR, guid(0x0cc5b647, 0xc1df, 0x4637, [0x89, 0x1a, 0xde, 0xc3, 0x5c, 0x31, 0x85, 0x83]), 100),
    // Energy performance preference: performance.
    (SUB_PROCESSOR, guid(0x36687f9e, 0xe3a5, 0x4dbf, [0xb1, 0xdc, 0x15, 0xeb, 0x38, 0x1c, 0x68, 0x63]), 0),
    // PCIe link state power management off.
    (SUB_PCIE, guid(0xee12f906, 0xd277, 0x404b, [0xb6, 0xda, 0xe5, 0xfa, 0x1a, 0x57, 0x6d, 0xf5]), 0),
    // USB3 link power management off.
    (SUB_USB, guid(0xd4e98f31, 0x5ffe, 0x4ce1, [0xbe, 0x31, 0x1b, 0x38, 0xb3, 0x84, 0xc0, 0x09]), 0),
    // USB selective suspend off.
    (SUB_USB, guid(0x48e6b7a6, 0x50f5, 0x4782, [0xa5, 0xd4, 0x53, 0xbb, 0x8f, 0x07, 0xe2, 0x26]), 0),
];

#[link(name = "powrprof")]
extern "system" {
    fn PowerGetActiveScheme(root: *mut c_void, scheme: *mut *mut Guid) -> u32;
    fn PowerSetActiveScheme(root: *mut c_void, scheme: *const Guid) -> u32;
    fn PowerDuplicateScheme(root: *mut c_void, source: *const Guid, dest: *mut *mut Guid) -> u32;
    fn PowerReadFriendlyName(
        root: *mut c_void,
        scheme: *const Guid,
        sub: *const Guid,
        setting: *const Guid,
        buffer: *mut u8,
        size: *mut u32,
    ) -> u32;
    fn PowerWriteFriendlyName(
        root: *mut c_void,
        scheme: *const Guid,
        sub: *const Guid,
        setting: *const Guid,
        buffer: *const u8,
        size: u32,
    ) -> u32;
    fn PowerWriteACValueIndex(root: *mut c_void, scheme: *const Guid, sub: *const Guid, setting: *const Guid, value: u32) -> u32;
    fn PowerWriteDCValueIndex(root: *mut c_void, scheme: *const Guid, sub: *const Guid, setting: *const Guid, value: u32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegSetKeyValueW(key: isize, sub: *const u16, value: *const u16, kind: u32, data: *const c_void, len: u32) -> i32;
    fn RegGetValueW(key: isize, sub: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, len: *mut u32) -> i32;
    fn RegDeleteKeyValueW(key: isize, sub: *const u16, value: *const u16) -> i32;
}

const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const REG_SZ: u32 = 1;
const REG_BINARY: u32 = 3;
const RRF_RT_REG_BINARY: u32 = 0x8;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn active_scheme() -> Option<Guid> {
    let mut p: *mut Guid = ptr::null_mut();
    if unsafe { PowerGetActiveScheme(ptr::null_mut(), &mut p) } != 0 || p.is_null() {
        return None;
    }
    let g = unsafe { *p };
    unsafe { LocalFree(p.cast()) };
    Some(g)
}

/// The power-mode slider ("overlay") API is exported by powrprof but not in the SDK headers.
fn overlay_fn(name: &[u8]) -> *mut c_void {
    let module = unsafe { GetModuleHandleW(wide("powrprof.dll").as_ptr()) };
    if module.is_null() {
        return ptr::null_mut();
    }
    unsafe { GetProcAddress(module, name.as_ptr()) }
}

fn get_overlay() -> Option<Guid> {
    let f = overlay_fn(b"PowerGetActualOverlayScheme\0");
    if f.is_null() {
        return None;
    }
    let f: unsafe extern "system" fn(*mut Guid) -> u32 = unsafe { std::mem::transmute(f) };
    let mut g = Guid::default();
    (unsafe { f(&mut g) } == 0).then_some(g)
}

fn set_overlay(g: &Guid) -> bool {
    let f = overlay_fn(b"PowerSetActiveOverlayScheme\0");
    if f.is_null() {
        return false;
    }
    let f: unsafe extern "system" fn(*const Guid) -> u32 = unsafe { std::mem::transmute(f) };
    unsafe { f(g) == 0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Previous {
    scheme: Guid,
    overlay: Option<Guid>,
}

const KEY: &str = "Software\\InfernoArmAsio";
const VALUE: &str = "PreviousPowerPlan";

impl Previous {
    fn encode(&self) -> [u8; 33] {
        let mut b = [0u8; 33];
        unsafe {
            ptr::write_unaligned(b.as_mut_ptr().cast::<Guid>(), self.scheme);
            ptr::write_unaligned(b.as_mut_ptr().add(16).cast::<Guid>(), self.overlay.unwrap_or_default());
        }
        b[32] = self.overlay.is_some() as u8;
        b
    }

    fn decode(b: &[u8; 33]) -> Self {
        let scheme = unsafe { ptr::read_unaligned(b.as_ptr().cast::<Guid>()) };
        let overlay = unsafe { ptr::read_unaligned(b.as_ptr().add(16).cast::<Guid>()) };
        Self { scheme, overlay: (b[32] != 0).then_some(overlay) }
    }

    fn load() -> Option<Self> {
        let mut b = [0u8; 33];
        let mut len = b.len() as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide(KEY).as_ptr(),
                wide(VALUE).as_ptr(),
                RRF_RT_REG_BINARY,
                ptr::null_mut(),
                b.as_mut_ptr().cast(),
                &mut len,
            )
        };
        (r == 0 && len == 33).then(|| Self::decode(&b))
    }

    fn store(&self) {
        let b = self.encode();
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), wide(VALUE).as_ptr(), REG_BINARY, b.as_ptr().cast(), 33)
        };
    }

    fn clear() {
        unsafe {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), wide(VALUE).as_ptr());
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), wide(OWNER).as_ptr());
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide(RUN_VALUE).as_ptr());
        }
    }
}

const OWNER: &str = "PowerPlanOwner";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "InfernoArmAsioPowerRestore";

/// Host process that activated the plan; the creation time guards against PID reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owner {
    pid: u32,
    created: u64,
}

fn process_created(process: HANDLE) -> Option<u64> {
    let mut times = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
    let [c, e, k, u] = &mut times;
    (unsafe { GetProcessTimes(process, c, e, k, u) } != 0)
        .then(|| (c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64)
}

struct Process(HANDLE);

impl Process {
    fn open(pid: u32) -> Option<Self> {
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        (!h.is_null()).then_some(Self(h))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl Owner {
    fn current() -> Option<Self> {
        Some(Self { pid: std::process::id(), created: process_created(unsafe { GetCurrentProcess() })? })
    }

    fn load() -> Option<Self> {
        let mut b = [0u8; 12];
        let mut len = b.len() as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide(KEY).as_ptr(),
                wide(OWNER).as_ptr(),
                RRF_RT_REG_BINARY,
                ptr::null_mut(),
                b.as_mut_ptr().cast(),
                &mut len,
            )
        };
        (r == 0 && len == 12).then(|| Self {
            pid: u32::from_le_bytes(b[..4].try_into().unwrap()),
            created: u64::from_le_bytes(b[4..].try_into().unwrap()),
        })
    }

    fn store(&self) {
        let mut b = [0u8; 12];
        b[..4].copy_from_slice(&self.pid.to_le_bytes());
        b[4..].copy_from_slice(&self.created.to_le_bytes());
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, wide(KEY).as_ptr(), wide(OWNER).as_ptr(), REG_BINARY, b.as_ptr().cast(), 12)
        };
    }

    fn is_alive(&self) -> bool {
        let Some(p) = Process::open(self.pid) else { return false };
        process_created(p.0) == Some(self.created) && unsafe { WaitForSingleObject(p.0, 0) } == WAIT_TIMEOUT
    }
}

/// Directory of the loaded ASIO DLL (where `inferno_control.exe` is installed).
fn module_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    let mut module = ptr::null_mut();
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            module_dir as *const () as *const u16,
            &mut module,
        )
    };
    if ok == 0 {
        return None;
    }
    let mut path = vec![0u16; 32768];
    let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
    if len == 0 || len >= path.len() {
        return None;
    }
    PathBuf::from(std::ffi::OsString::from_wide(&path[..len])).parent().map(Into::into)
}

/// Registers a logon-time restore and starts a detached guard for the current host process.
fn arm_crash_recovery() {
    let Some(exe) = module_dir().map(|d| d.join("inferno_control.exe")).filter(|e| e.exists()) else {
        log::warn!("power plan: inferno_control.exe not found; crash recovery relies on the next engine start");
        return;
    };
    let run = wide(&format!("\"{}\" --power-restore", exe.display()));
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(RUN_VALUE).as_ptr(),
            REG_SZ,
            run.as_ptr().cast(),
            (run.len() * 2) as u32,
        )
    };
    if let Err(e) = spawn_guard(&exe) {
        log::warn!("power plan: guard process: {e}");
    }
}

fn spawn_guard(exe: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let application: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let startup = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..unsafe { std::mem::zeroed() } };
    // Break away from the host's job (if allowed) so killing the host's job does not kill the guard.
    for flags in [CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW] {
        let mut command = wide(&format!("\"{}\" --power-guard {}", exe.display(), std::process::id()));
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                flags,
                ptr::null(),
                ptr::null(),
                &startup,
                &mut process,
            )
        };
        if ok != 0 {
            unsafe {
                CloseHandle(process.hThread);
                CloseHandle(process.hProcess);
            }
            return Ok(());
        }
    }
    Err(std::io::Error::last_os_error().to_string())
}

/// Restores the recorded plan and power mode if the low-latency plan is still active.
fn restore(previous: &Previous) {
    if active_scheme() == Some(PLAN) {
        unsafe { PowerSetActiveScheme(ptr::null_mut(), &previous.scheme) };
        if let Some(overlay) = previous.overlay {
            set_overlay(&overlay);
        }
    }
    Previous::clear();
}

/// `inferno_control.exe --power-guard <pid>`: waits for the host and restores the user's plan
/// if it exits while still owning the low-latency plan. Returns once the engine restored the
/// plan itself, another host took over, or the host exited.
pub fn guard(pid: u32) {
    let Some(host) = Process::open(pid) else {
        restore_orphaned();
        return;
    };
    let created = process_created(host.0);
    loop {
        let exited = unsafe { WaitForSingleObject(host.0, 1000) } == WAIT_OBJECT_0;
        match (Owner::load(), Previous::load()) {
            (Some(o), Some(previous)) if o.pid == pid && Some(o.created) == created => {
                if exited {
                    restore(&previous);
                    return;
                }
            }
            _ => return,
        }
    }
}

/// `inferno_control.exe --power-restore` (logon): restores the user's plan unless the host that
/// activated the low-latency plan is still running.
pub fn restore_orphaned() {
    let Some(previous) = Previous::load() else {
        Previous::clear();
        return;
    };
    if Owner::load().is_some_and(|o| o.is_alive()) {
        return;
    }
    restore(&previous);
}

fn ensure_plan() -> Result<(), String> {
    let mut size = 0u32;
    let exists = unsafe {
        PowerReadFriendlyName(ptr::null_mut(), &PLAN, ptr::null(), ptr::null(), ptr::null_mut(), &mut size)
    } == 0;
    if !exists {
        let source = active_scheme().filter(|g| *g != PLAN).unwrap_or(BALANCED);
        let mut target = PLAN;
        let mut dest: *mut Guid = &mut target;
        let r = unsafe { PowerDuplicateScheme(ptr::null_mut(), &source, &mut dest) };
        if r != 0 {
            return Err(format!("PowerDuplicateScheme failed: {r}"));
        }
        let name: Vec<u8> = wide("Inferno Low Latency").iter().flat_map(|c| c.to_le_bytes()).collect();
        unsafe {
            PowerWriteFriendlyName(ptr::null_mut(), &PLAN, ptr::null(), ptr::null(), name.as_ptr(), name.len() as u32)
        };
        log::info!("created power plan 'Inferno Low Latency'");
    }
    for (sub, setting, value) in &SETTINGS {
        // Some settings are absent on some platforms; failures are not fatal.
        unsafe {
            PowerWriteACValueIndex(ptr::null_mut(), &PLAN, sub, setting, *value);
            PowerWriteDCValueIndex(ptr::null_mut(), &PLAN, sub, setting, *value);
        }
    }
    Ok(())
}

/// Active low-latency power plan; restores the previous plan and power mode on drop.
pub struct LowLatencyPower(Option<Previous>);

impl LowLatencyPower {
    /// `enabled` is the control-panel switch; `INFERNO_ASIO_POWER_PLAN=off` always wins.
    pub fn activate(enabled: bool) -> Self {
        if !enabled
            || env::var("INFERNO_ASIO_POWER_PLAN").is_ok_and(|v| v.eq_ignore_ascii_case("off"))
        {
            return Self(None);
        }
        let Some(active) = active_scheme() else {
            log::warn!("power plan: cannot read active scheme");
            return Self(None);
        };
        let previous = if active == PLAN {
            // Left active by a crashed host, or another host is streaming.
            Previous::load().unwrap_or(Previous { scheme: BALANCED, overlay: None })
        } else {
            Previous { scheme: active, overlay: get_overlay() }
        };
        previous.store();
        let Some(owner) = Owner::current() else {
            log::warn!("power plan: cannot identify host process");
            Previous::clear();
            return Self(None);
        };
        owner.store();
        if let Err(e) = ensure_plan() {
            log::warn!("power plan: {e}");
            Previous::clear();
            return Self(None);
        }
        let r = unsafe { PowerSetActiveScheme(ptr::null_mut(), &PLAN) };
        if r != 0 {
            log::warn!("power plan: PowerSetActiveScheme failed: {r}");
            Previous::clear();
            return Self(None);
        }
        set_overlay(&BEST_PERFORMANCE_OVERLAY);
        arm_crash_recovery();
        log::info!("power plan: Inferno Low Latency active (previous {:?})", previous.scheme);
        Self(Some(previous))
    }
}

impl Drop for LowLatencyPower {
    fn drop(&mut self) {
        let Some(previous) = self.0.take() else { return };
        // Another host took over the plan; it restores when it stops.
        if Owner::load() != Owner::current() {
            return;
        }
        // restore() respects a plan the user selected manually during the session.
        restore(&previous);
        log::info!("power plan: restored {:?}", previous.scheme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_round_trips() {
        for p in [
            Previous { scheme: BALANCED, overlay: Some(BEST_PERFORMANCE_OVERLAY) },
            Previous { scheme: PLAN, overlay: None },
        ] {
            assert_eq!(Previous::decode(&p.encode()), p);
        }
    }
}
