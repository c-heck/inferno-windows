use std::{ffi::c_void, io, ptr};

#[link(name = "kernel32")]
extern "system" {
    fn CreateWaitableTimerExW(
        attrs: *const c_void,
        name: *const u16,
        flags: u32,
        access: u32,
    ) -> *mut c_void;
    fn SetWaitableTimer(
        timer: *mut c_void,
        due: *const i64,
        period: i32,
        callback: *const c_void,
        arg: *const c_void,
        resume: i32,
    ) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn QueryPerformanceCounter(value: *mut i64) -> i32;
    fn QueryPerformanceFrequency(value: *mut i64) -> i32;
    fn GetTickCount64() -> u64;
    fn OutputDebugStringA(text: *const u8);
}

pub struct Timer(*mut c_void);

#[link(name = "avrt")]
extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> *mut c_void;
    fn AvSetMmThreadPriority(handle: *mut c_void, priority: i32) -> i32;
    fn AvRevertMmThreadCharacteristics(handle: *mut c_void) -> i32;
}

#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(period_ms: u32) -> u32;
    fn timeEndPeriod(period_ms: u32) -> u32;
}

/// Registers the current thread with MMCSS "Pro Audio" at critical priority, like DAW audio
/// threads, so they cannot preempt PTP timestamping or packet pacing for milliseconds.
pub struct AudioThread(*mut c_void);

impl AudioThread {
    pub fn register() -> Self {
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
        if handle.is_null() {
            log::warn!("MMCSS Pro Audio registration failed: {}", io::Error::last_os_error());
        } else {
            unsafe { AvSetMmThreadPriority(handle, 2) }; // AVRT_PRIORITY_CRITICAL
        }
        disable_thread_power_throttling();
        Self(handle)
    }
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { AvRevertMmThreadCharacteristics(self.0) };
        }
    }
}

/// Requests 1 ms system timer resolution (tokio and Sleep-based waits) while audio runs.
pub struct TimerResolution(bool);

impl TimerResolution {
    pub fn request() -> Self {
        disable_process_power_throttling();
        Self(unsafe { timeBeginPeriod(1) } == 0)
    }
}

#[repr(C)]
struct PowerThrottlingState {
    version: u32,
    control_mask: u32,
    state_mask: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn GetCurrentProcess() -> *mut c_void;
    fn SetThreadInformation(thread: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
    fn SetProcessInformation(process: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
}

/// Opts the current thread out of EcoQoS, which on Snapdragon schedules throttled threads on
/// low-frequency operating points and lengthens wake-up latency.
pub fn disable_thread_power_throttling() {
    // THREAD_POWER_THROTTLING_EXECUTION_SPEED controlled and off.
    let state = PowerThrottlingState { version: 1, control_mask: 1, state_mask: 0 };
    let ok = unsafe {
        SetThreadInformation(
            GetCurrentThread(),
            3, // ThreadPowerThrottling
            &state as *const _ as *const c_void,
            std::mem::size_of::<PowerThrottlingState>() as u32,
        )
    };
    if ok == 0 {
        log::warn!("thread power throttling opt-out failed: {}", io::Error::last_os_error());
    }
}

/// Windows 11 ignores timer resolution requests from processes without a visible window unless
/// the process opts out; the host DAW can be minimized while audio runs.
fn disable_process_power_throttling() {
    // EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION controlled and off.
    let state = PowerThrottlingState { version: 1, control_mask: 1 | 4, state_mask: 0 };
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            4, // ProcessPowerThrottling
            &state as *const _ as *const c_void,
            std::mem::size_of::<PowerThrottlingState>() as u32,
        )
    };
    if ok == 0 {
        log::warn!("process power throttling opt-out failed: {}", io::Error::last_os_error());
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.0 {
            unsafe { timeEndPeriod(1) };
        }
    }
}

impl Timer {
    pub fn new() -> Result<Self, String> {
        let handle = unsafe { CreateWaitableTimerExW(ptr::null(), ptr::null(), 2, 0x1f0003) };
        if handle.is_null() {
            return Err(format!(
                "High-resolution timer: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self(handle))
    }

    pub fn sleep_ns(&self, nanos: u64) -> Result<(), String> {
        if nanos == 0 {
            return Ok(());
        }
        let due = -(nanos.div_ceil(100).min(i64::MAX as u64) as i64);
        if unsafe { SetWaitableTimer(self.0, &due, 0, ptr::null(), ptr::null(), 0) } == 0 {
            return Err(format!("SetWaitableTimer: {}", io::Error::last_os_error()));
        }
        if unsafe { WaitForSingleObject(self.0, 1000) } != 0 {
            return Err("Audio timer wait failed or timed out".into());
        }
        Ok(())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub fn system_timestamp_ns() -> u64 {
    // ASIO timestamps use the system-uptime timescale.
    unsafe { GetTickCount64() }.saturating_mul(1_000_000)
}

pub fn performance_ns() -> Result<u64, String> {
    let (mut value, mut frequency) = (0, 0);
    if unsafe { QueryPerformanceCounter(&mut value) } == 0
        || unsafe { QueryPerformanceFrequency(&mut frequency) } == 0
        || frequency <= 0
    {
        return Err("QueryPerformanceCounter is unavailable".into());
    }
    Ok((value as u128 * 1_000_000_000 / frequency as u128) as u64)
}

pub fn report(message: &str) {
    log::error!("{message}");
    let text = std::ffi::CString::new(format!("Inferno ASIO: {}\n", message.replace('\0', "?")));
    if let Ok(text) = text {
        unsafe {
            OutputDebugStringA(text.as_ptr().cast());
        }
    }
}
