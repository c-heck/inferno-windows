//! Loads the actual DLL without COM registration and verifies all-channel loopback.
use inferno_asio::abi::*;
use std::{
    ffi::{c_char, c_void, OsStr},
    os::windows::ffi::OsStrExt,
    ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

#[repr(C)]
struct Factory {
    table: *const FactoryTable,
}
#[repr(C)]
struct FactoryTable {
    query: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    create:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32,
    lock: unsafe extern "system" fn(*mut c_void, i32) -> i32,
}

struct Host {
    inputs: Vec<[usize; 2]>,
    outputs: Vec<[usize; 2]>,
    frames: usize,
    rate: f64,
    calls: usize,
    timestamp: u64,
}
static HOST: Mutex<Option<Host>> = Mutex::new(None);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static FAILURES: AtomicUsize = AtomicUsize::new(0);
/// Non-zero: diagnostic live-network run for this many seconds (silent output, no PCM checks).
static LIVE_SECONDS: AtomicUsize = AtomicUsize::new(0);

fn live_seconds_arg() -> Result<usize, String> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    match args.iter().position(|arg| arg == "--live-seconds") {
        Some(i) => args
            .get(i + 1)
            .and_then(|value| value.parse().ok())
            .filter(|seconds| *seconds > 0)
            .ok_or_else(|| "--live-seconds needs a positive number".to_string()),
        None => Ok(0),
    }
}

fn pattern(cycle: usize, channel: usize, frame: usize) -> i32 {
    ((cycle as i32 * 127 + channel as i32 * 4099 + frame as i32 + 1) & 0x7fffff) << 8
}

unsafe extern "C" fn switch(index: i32, _: i32) {
    let Ok(mut guard) = HOST.lock() else {
        FAILURES.fetch_add(1, Ordering::SeqCst);
        return;
    };
    let Some(host) = guard.as_mut() else {
        FAILURES.fetch_add(1, Ordering::SeqCst);
        return;
    };
    if LIVE_SECONDS.load(Ordering::Relaxed) != 0 {
        for buffers in &host.outputs {
            unsafe {
                std::slice::from_raw_parts_mut(buffers[index as usize & 1] as *mut i32, host.frames)
                    .fill(0);
            }
        }
        host.calls += 1;
        CALLS.store(host.calls, Ordering::Release);
        return;
    }
    if index != (host.calls & 1) as i32 {
        FAILURES.fetch_add(1, Ordering::SeqCst);
        return;
    }
    for (channel, buffers) in host.inputs.iter().enumerate() {
        let samples = unsafe {
            std::slice::from_raw_parts(buffers[index as usize] as *const i32, host.frames)
        };
        for (frame, sample) in samples.iter().enumerate() {
            let expected = if host.calls == 0 {
                0
            } else {
                pattern(host.calls - 1, channel, frame)
            };
            if *sample != expected {
                FAILURES.fetch_add(1, Ordering::SeqCst);
                break;
            }
        }
    }
    for (channel, buffers) in host.outputs.iter().enumerate() {
        let samples = unsafe {
            std::slice::from_raw_parts_mut(buffers[index as usize] as *mut i32, host.frames)
        };
        for (frame, sample) in samples.iter_mut().enumerate() {
            *sample = pattern(host.calls, channel, frame);
        }
    }
    host.calls += 1;
    CALLS.store(host.calls, Ordering::Release);
}
unsafe extern "C" fn switch_time(time: *mut Time, index: i32, direct: i32) -> *mut Time {
    if time.is_null() {
        FAILURES.fetch_add(1, Ordering::SeqCst);
        return time;
    }
    {
        let Ok(mut guard) = HOST.lock() else {
            FAILURES.fetch_add(1, Ordering::SeqCst);
            return time;
        };
        if let Some(host) = guard.as_mut().filter(|_| LIVE_SECONDS.load(Ordering::Relaxed) == 0) {
            let info = unsafe { &(*time).info };
            let position = (info.sample_position.hi as u64) << 32 | info.sample_position.lo as u64;
            let stamp = (info.system_time.hi as u64) << 32 | info.system_time.lo as u64;
            if info.flags & 7 != 7
                || info.sample_rate != host.rate
                || position != (host.calls * host.frames) as u64
                || stamp <= host.timestamp
            {
                FAILURES.fetch_add(1, Ordering::SeqCst);
            }
            host.timestamp = stamp;
        }
    }
    unsafe {
        switch(index, direct);
    }
    time
}
unsafe extern "C" fn rate_changed(_: f64) {
    FAILURES.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn message(selector: i32, _: i32, _: *mut c_void, _: *mut f64) -> i32 {
    if selector == 7 {
        1
    } else {
        0
    }
}

unsafe fn exercise(device: *mut c_void, table: &AsioVtable) -> Result<(), String> {
    fn check(result: Error, operation: &str) -> Result<(), String> {
        if result == OK {
            Ok(())
        } else {
            Err(format!("{operation} returned {result}"))
        }
    }
    if unsafe { (table.init)(device, ptr::null_mut()) } != 1 {
        return Err("ASIO init failed".into());
    }
    if std::env::args().skip(2).any(|arg| arg == "--control-panel") {
        check(unsafe { (table.panel)(device) }, "controlPanel")?;
    }
    let (mut inputs, mut outputs) = (0, 0);
    check(
        unsafe { (table.channels)(device, &mut inputs, &mut outputs) },
        "getChannels",
    )?;
    let mut rate = 0.0;
    check(
        unsafe { (table.get_rate)(device, &mut rate) },
        "getSampleRate",
    )?;
    if inputs != outputs || inputs == 0 || inputs > if rate == 96000.0 { 32 } else { 64 } {
        return Err("Invalid channel profile".into());
    }
    let (mut min, mut max, mut preferred, mut granularity) = (0, 0, 0, 0);
    check(
        unsafe {
            (table.buffer_size)(device, &mut min, &mut max, &mut preferred, &mut granularity)
        },
        "getBufferSize",
    )?;
    let expected = inferno_asio::config::Config::from_env()?.asio_frames;
    if (min, max, preferred, granularity) != (64, 2048, expected, -1) {
        return Err("Driver did not report the configured ASIO buffer preference".into());
    }
    let frames: usize = match std::env::var("INFERNO_PROBE_FRAMES") {
        Ok(value) => value.parse().map_err(|_| "Invalid INFERNO_PROBE_FRAMES")?,
        Err(std::env::VarError::NotPresent) => preferred as usize,
        Err(e) => return Err(e.to_string()),
    };
    let mut infos: Vec<_> = (0..2)
        .flat_map(|direction| {
            (0..inputs).map(move |channel| BufferInfo {
                is_input: 1 - direction,
                channel,
                buffers: [ptr::null_mut(); 2],
            })
        })
        .collect();
    let callbacks = Callbacks {
        switch: Some(switch),
        rate_changed: Some(rate_changed),
        message: Some(message),
        switch_time: Some(switch_time),
    };
    check(
        unsafe {
            (table.create)(
                device,
                infos.as_mut_ptr(),
                infos.len() as i32,
                frames as i32,
                &callbacks,
            )
        },
        "createBuffers",
    )?;
    let input_ptrs = infos
        .iter()
        .filter(|i| i.is_input == 1)
        .map(|i| [i.buffers[0] as usize, i.buffers[1] as usize])
        .collect();
    let output_ptrs = infos
        .iter()
        .filter(|i| i.is_input == 0)
        .map(|i| [i.buffers[0] as usize, i.buffers[1] as usize])
        .collect();
    *HOST.lock().map_err(|_| "Probe state poisoned")? = Some(Host {
        inputs: input_ptrs,
        outputs: output_ptrs,
        frames,
        rate,
        calls: 0,
        timestamp: 0,
    });
    let live = LIVE_SECONDS.load(Ordering::Relaxed);
    if live != 0 {
        check(unsafe { (table.start)(device) }, "start")?;
        std::thread::sleep(Duration::from_secs(live as u64));
        check(unsafe { (table.stop)(device) }, "stop")?;
        check(unsafe { (table.dispose)(device) }, "disposeBuffers")?;
        println!(
            "LIVE: {inputs}x{outputs}, {rate} Hz, {frames} frames, {} callbacks in {live} s",
            CALLS.load(Ordering::Acquire)
        );
        return Ok(());
    }
    for _ in 0..2 {
        CALLS.store(0, Ordering::SeqCst);
        {
            let mut guard = HOST.lock().map_err(|_| "Probe state poisoned")?;
            if let Some(host) = guard.as_mut() {
                host.calls = 0;
                host.timestamp = 0;
            }
        }
        check(unsafe { (table.start)(device) }, "start")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while CALLS.load(Ordering::Acquire) < 12 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        check(unsafe { (table.stop)(device) }, "stop")?;
        if CALLS.load(Ordering::Acquire) < 12 {
            return Err("Audio callbacks did not advance".into());
        }
        if FAILURES.load(Ordering::Acquire) != 0 {
            return Err(format!(
                "{} loopback/time-info failures",
                FAILURES.load(Ordering::Acquire)
            ));
        }
    }
    check(unsafe { (table.dispose)(device) }, "disposeBuffers")?;
    println!("PASS: {inputs}x{outputs}, {rate} Hz, {frames} frames, time-info, all-channel PCM, start/stop/restart");
    Ok(())
}

fn run() -> Result<(), String> {
    LIVE_SECONDS.store(live_seconds_arg()?, Ordering::Relaxed);
    if LIVE_SECONDS.load(Ordering::Relaxed) == 0 && !inferno_asio::config::Config::from_env()?.loopback {
        return Err(
            "Probe requires loopback mode in settings or INFERNO_ASIO_MODE (or --live-seconds N for a live network run)".into(),
        );
    }
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Usage: asio_probe.exe <absolute DLL path>")?;
    let path: Vec<u16> = OsStr::new(&path).encode_wide().chain(Some(0)).collect();
    let module = unsafe { LoadLibraryW(path.as_ptr()) };
    if module.is_null() {
        return Err(format!("LoadLibrary: {}", std::io::Error::last_os_error()));
    }
    let result = (|| {
        let get_class = unsafe { GetProcAddress(module, c"DllGetClassObject".as_ptr()) };
        let unload = unsafe { GetProcAddress(module, c"DllCanUnloadNow".as_ptr()) };
        if get_class.is_null() || unload.is_null() {
            return Err("DLL COM exports missing".into());
        }
        let get_class: unsafe extern "system" fn(
            *const Guid,
            *const Guid,
            *mut *mut c_void,
        ) -> i32 = unsafe { std::mem::transmute(get_class) };
        let unload: extern "system" fn() -> i32 = unsafe { std::mem::transmute(unload) };
        let mut factory = ptr::null_mut();
        if unsafe { get_class(&CLSID, &ICLASS_FACTORY, &mut factory) } != OK {
            return Err("DllGetClassObject failed".into());
        }
        let factory_table = unsafe { &*(*factory.cast::<Factory>()).table };
        let mut device = ptr::null_mut();
        let created =
            unsafe { (factory_table.create)(factory, ptr::null_mut(), &CLSID, &mut device) };
        unsafe {
            (factory_table.release)(factory);
        }
        if created != OK || device.is_null() {
            return Err("COM CreateInstance failed".into());
        }
        let table = unsafe { &**device.cast::<*const AsioVtable>() };
        let result = unsafe { exercise(device, table) };
        let _ = unsafe { (table.stop)(device) };
        let _ = unsafe { (table.dispose)(device) };
        unsafe {
            (table.release)(device);
        }
        if unload() != OK {
            return Err("DLL retains live COM objects or workers".into());
        }
        result
    })();
    unsafe {
        FreeLibrary(module);
    }
    result
}

fn main() {
    if let Err(e) = run() {
        eprintln!("FAIL: {e}");
        std::process::exit(1);
    }
}
