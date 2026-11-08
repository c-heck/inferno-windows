use crate::{
    abi::*,
    config::{self, Config},
    engine, timer,
};
use std::{
    ffi::{c_char, c_void},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

pub struct ChannelBuffer {
    pub input: bool,
    pub channel: usize,
    pub data: [Box<[i32]>; 2],
}

#[derive(Default)]
pub struct Shared {
    sequence: AtomicU64,
    position: AtomicU64,
    timestamp: AtomicU64,
    pub xruns: AtomicU64,
    pub input_latency: AtomicU64,
    pub running: AtomicBool,
    pub started: AtomicBool,
    pub error: Mutex<String>,
}

impl Shared {
    pub fn set_position(&self, position: u64, timestamp: u64) {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        self.position.store(position, Ordering::SeqCst);
        self.timestamp.store(timestamp, Ordering::SeqCst);
        self.sequence.fetch_add(1, Ordering::SeqCst);
    }
    pub fn position(&self) -> (u64, u64) {
        loop {
            let first = self.sequence.load(Ordering::SeqCst);
            let position = self.position.load(Ordering::SeqCst);
            let timestamp = self.timestamp.load(Ordering::SeqCst);
            if first & 1 == 0 && first == self.sequence.load(Ordering::SeqCst) {
                return (position, timestamp);
            }
        }
    }
    pub fn fail(&self, message: impl Into<String>) {
        let message = message.into();
        timer::report(&message);
        if let Ok(mut error) = self.error.lock() {
            *error = message;
        }
    }
}

struct State {
    config: Option<Config>,
    channel_limit: usize,
    buffers: Vec<ChannelBuffer>,
    selected: Vec<(bool, usize)>,
    frames: usize,
    callbacks: Option<Callbacks>,
    time_info: bool,
    worker: Option<JoinHandle<Vec<ChannelBuffer>>>,
    stop: Arc<AtomicBool>,
    busy: bool,
}

#[repr(C)]
pub struct Driver {
    pub vtable: &'static AsioVtable,
    pub refs: AtomicU32,
    state: Mutex<State>,
    shared: Arc<Shared>,
}

impl Driver {
    pub fn new() -> Self {
        Self {
            vtable: &VTABLE,
            refs: AtomicU32::new(1),
            state: Mutex::new(State {
                config: None,
                channel_limit: 64,
                buffers: Vec::new(),
                selected: Vec::new(),
                frames: 256,
                callbacks: None,
                time_info: false,
                worker: None,
                stop: Arc::new(AtomicBool::new(false)),
                busy: false,
            }),
            shared: Arc::new(Shared::default()),
        }
    }

    fn with_state(&self, f: impl FnOnce(&mut State) -> Error) -> Error {
        match self.state.lock() {
            Ok(mut state) => f(&mut state),
            Err(_) => {
                self.shared.fail("ASIO state lock poisoned");
                HW_ERROR
            }
        }
    }

    fn start(&self) -> Error {
        if engine::in_callback() {
            return INVALID_MODE;
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let result = self.with_state(|s| {
            if s.busy || s.worker.is_some() || s.callbacks.is_none() {
                return INVALID_MODE;
            }
            let Some(config) = s.config.clone() else {
                return INVALID_MODE;
            };
            let Some(callbacks) = s.callbacks else {
                return INVALID_MODE;
            };
            s.stop.store(false, Ordering::Release);
            self.shared.running.store(true, Ordering::Release);
            self.shared.started.store(false, Ordering::Release);
            self.shared.xruns.store(0, Ordering::Release);
            self.shared.input_latency.store(
                (config.network_frames() + s.frames) as u64,
                Ordering::Release,
            );
            self.shared.set_position(0, 0);
            let (buffers_tx, buffers_rx) = std::sync::mpsc::sync_channel::<Vec<ChannelBuffer>>(1);
            let run = engine::Run {
                config,
                frames: s.frames,
                callbacks,
                time_info: s.time_info,
                shared: self.shared.clone(),
                stop: s.stop.clone(),
                ready: ready_tx.clone(),
            };
            // Return buffers even on an ordinary startup/streaming error so host pointers remain valid.
            let thread = std::thread::Builder::new()
                .name("Inferno ASIO".into())
                .spawn(move || {
                    let mut buffers = match buffers_rx.recv() {
                        Ok(buffers) => buffers,
                        Err(e) => {
                            run.shared.fail(format!("Audio buffer handoff failed: {e}"));
                            run.shared.running.store(false, Ordering::Release);
                            return Vec::new();
                        }
                    };
                    let result = run.run(&mut buffers);
                    run.shared.running.store(false, Ordering::Release);
                    if let Err(error) = result {
                        run.shared.fail(error.clone());
                        let _ = run.ready.try_send(Err(error));
                        if run.shared.started.swap(false, Ordering::AcqRel) {
                            engine::notify(callbacks, 3);
                        }
                    }
                    buffers
                });
            match thread {
                Ok(thread) => {
                    s.worker = Some(thread);
                    if let Err(e) = buffers_tx.send(std::mem::take(&mut s.buffers)) {
                        s.buffers = e.0;
                        self.shared
                            .fail("Audio worker terminated before buffer handoff");
                        return HW_ERROR;
                    }
                    OK
                }
                Err(e) => {
                    self.shared.running.store(false, Ordering::Release);
                    self.shared.fail(format!("Audio thread creation: {e}"));
                    HW_ERROR
                }
            }
        });
        if result != OK {
            return result;
        }
        drop(ready_tx);
        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => OK,
            outcome => {
                if let Ok(Err(message)) = outcome {
                    self.shared.fail(message);
                } else {
                    self.shared
                        .fail("Audio startup timed out or worker terminated");
                }
                let _ = self.stop();
                NO_CLOCK
            }
        }
    }

    fn stop(&self) -> Error {
        if engine::in_callback() {
            return INVALID_MODE;
        }
        let worker = match self.state.lock() {
            Ok(mut s) => {
                if s.busy {
                    return INVALID_MODE;
                }
                if s.worker
                    .as_ref()
                    .is_some_and(|worker| worker.thread().id() == std::thread::current().id())
                {
                    self.shared
                        .fail("ASIO stop/dispose must be called outside the audio callback");
                    return INVALID_MODE;
                }
                s.stop.store(true, Ordering::Release);
                s.busy = true;
                s.worker.take()
            }
            Err(_) => {
                self.shared.fail("ASIO state lock poisoned");
                return HW_ERROR;
            }
        };
        let result = worker.map(JoinHandle::join);
        self.shared.running.store(false, Ordering::Release);
        self.shared.input_latency.store(0, Ordering::Release);
        self.with_state(|s| {
            s.busy = false;
            match result {
                Some(Ok(buffers)) => {
                    s.buffers = buffers;
                    OK
                }
                Some(Err(_)) => {
                    self.shared
                        .fail("Audio worker panicked; recreate the ASIO device");
                    HW_ERROR
                }
                None => OK,
            }
        })
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

unsafe fn driver<'a>(this: *mut c_void) -> &'a Driver {
    unsafe { &*this.cast::<Driver>() }
}

unsafe extern "system" fn init(this: *mut c_void, _: *mut c_void) -> i32 {
    let d = unsafe { driver(this) };
    match config::load_configuration() {
        Ok((config, channel_limit)) => {
            (d.with_state(|s| {
                if s.busy {
                    return 0;
                }
                if s.worker.is_some() || s.callbacks.is_some() {
                    return 0;
                }
                s.channel_limit = channel_limit;
                s.config = Some(config);
                1
            }) == 1) as i32
        }
        Err(e) => {
            d.shared.fail(e);
            0
        }
    }
}
unsafe extern "system" fn name(_: *mut c_void, out: *mut c_char) {
    if !out.is_null() {
        unsafe {
            write_text(
                &mut *out.cast::<[c_char; 32]>(),
                "Inferno ARM ASIO (experimental)",
            );
        }
    }
}
unsafe extern "system" fn version(_: *mut c_void) -> i32 {
    1
}
unsafe extern "system" fn error(this: *mut c_void, out: *mut c_char) {
    if out.is_null() {
        return;
    }
    let d = unsafe { driver(this) };
    let text = match d.shared.error.lock() {
        Ok(e) => e.clone(),
        Err(_) => "ASIO error state lock poisoned".into(),
    };
    unsafe {
        write_text(&mut *out.cast::<[c_char; 124]>(), &text);
    }
}
unsafe extern "system" fn start(this: *mut c_void) -> Error {
    unsafe { driver(this) }.start()
}
unsafe extern "system" fn stop(this: *mut c_void) -> Error {
    unsafe { driver(this) }.stop()
}

unsafe extern "system" fn channels(this: *mut c_void, input: *mut i32, output: *mut i32) -> Error {
    if input.is_null() || output.is_null() {
        return INVALID_PARAMETER;
    }
    unsafe { driver(this) }.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        unsafe {
            *input = config.channels as i32;
            *output = config.channels as i32;
        }
        OK
    })
}
unsafe extern "system" fn latencies(this: *mut c_void, input: *mut i32, output: *mut i32) -> Error {
    if input.is_null() || output.is_null() {
        return INVALID_PARAMETER;
    }
    let d = unsafe { driver(this) };
    d.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        let frames = if config.loopback {
            s.frames
        } else {
            config.network_frames() + s.frames
        };
        unsafe {
            *input = if config.loopback {
                frames as i32
            } else {
                frames.max(d.shared.input_latency.load(Ordering::Acquire) as usize) as i32
            };
            *output = if config.loopback { 0 } else { frames as i32 };
        }
        OK
    })
}
unsafe extern "system" fn buffer_size(
    this: *mut c_void,
    min: *mut i32,
    max: *mut i32,
    preferred: *mut i32,
    granularity: *mut i32,
) -> Error {
    if min.is_null() || max.is_null() || preferred.is_null() || granularity.is_null() {
        return INVALID_PARAMETER;
    }
    unsafe { driver(this) }.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        unsafe {
            *min = 64;
            *max = 2048;
            *preferred = config.asio_frames;
            *granularity = -1;
        }
        OK
    })
}
fn supported_rate(rate: f64) -> bool {
    [44100.0, 48000.0, 96000.0].contains(&rate)
}
unsafe extern "system" fn can_rate(_: *mut c_void, rate: f64) -> Error {
    if supported_rate(rate) {
        OK
    } else {
        NO_CLOCK
    }
}
unsafe extern "system" fn get_rate(this: *mut c_void, rate: *mut f64) -> Error {
    if rate.is_null() {
        return INVALID_PARAMETER;
    }
    unsafe { driver(this) }.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        unsafe {
            *rate = config.sample_rate as f64;
        }
        OK
    })
}
unsafe extern "system" fn set_rate(this: *mut c_void, rate: f64) -> Error {
    if !supported_rate(rate) {
        return NO_CLOCK;
    }
    unsafe { driver(this) }.with_state(|s| {
        if s.busy || s.worker.is_some() {
            return INVALID_MODE;
        }
        let Some(config) = &mut s.config else {
            return INVALID_MODE;
        };
        if rate == config.sample_rate as f64 {
            return OK;
        }
        if s.callbacks.is_some() {
            return INVALID_MODE;
        }
        config.sample_rate = rate as u32;
        config.channels = s.channel_limit.min(if rate == 96000.0 { 32 } else { 64 });
        OK
    })
}
unsafe extern "system" fn clocks(
    this: *mut c_void,
    out: *mut ClockSource,
    count: *mut i32,
) -> Error {
    if out.is_null() || count.is_null() || unsafe { *count } < 1 {
        return INVALID_PARAMETER;
    }
    unsafe { driver(this) }.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        let mut clock = ClockSource {
            index: 0,
            associated_channel: -1,
            associated_group: -1,
            current: 1,
            name: [0; 32],
        };
        write_text(
            &mut clock.name,
            if config.loopback {
                "Diagnostic loopback (QPC)"
            } else {
                "Dante PTPv1 (software)"
            },
        );
        unsafe {
            *out = clock;
            *count = 1;
        }
        OK
    })
}
unsafe extern "system" fn set_clock(_: *mut c_void, index: i32) -> Error {
    if index == 0 {
        OK
    } else {
        INVALID_PARAMETER
    }
}
unsafe extern "system" fn position(
    this: *mut c_void,
    position: *mut Split64,
    stamp: *mut Split64,
) -> Error {
    if position.is_null() || stamp.is_null() {
        return INVALID_PARAMETER;
    }
    let d = unsafe { driver(this) };
    if !d.shared.running.load(Ordering::Acquire) {
        return NOT_RUNNING;
    }
    let (p, t) = d.shared.position();
    unsafe {
        *position = p.into();
        *stamp = t.into();
    }
    OK
}
unsafe extern "system" fn channel_info(this: *mut c_void, info: *mut ChannelInfo) -> Error {
    if info.is_null() {
        return INVALID_PARAMETER;
    }
    let info = unsafe { &mut *info };
    unsafe { driver(this) }.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        if info.channel < 0
            || info.channel as usize >= config.channels
            || ![0, 1].contains(&info.is_input)
        {
            return INVALID_PARAMETER;
        }
        info.active = s
            .selected
            .contains(&(info.is_input == 1, info.channel as usize)) as i32;
        info.group = 0;
        info.sample_type = 18; // ASIOSTInt32LSB, signed left-justified PCM.
        write_text(
            &mut info.name,
            &format!(
                "{} {:02}",
                if info.is_input == 1 {
                    "Network RX"
                } else {
                    "Network TX"
                },
                info.channel + 1
            ),
        );
        OK
    })
}
unsafe extern "system" fn create(
    this: *mut c_void,
    infos: *mut BufferInfo,
    count: i32,
    frames: i32,
    callbacks: *const Callbacks,
) -> Error {
    if infos.is_null()
        || callbacks.is_null()
        || !(1..=128).contains(&count)
        || !config::valid_buffer(frames)
    {
        return INVALID_PARAMETER;
    }
    if engine::in_callback() {
        return INVALID_MODE;
    }
    let callbacks = unsafe { *callbacks };
    let switch = callbacks.switch;
    let switch_time = callbacks.switch_time;
    if switch.is_none() {
        return INVALID_PARAMETER;
    }
    let d = unsafe { driver(this) };
    let mut time_info = false;
    if let Some(message) = callbacks.message {
        time_info = switch_time.is_some()
            && unsafe { message(7, 0, ptr::null_mut(), ptr::null_mut()) } == 1;
    }
    d.with_state(|s| {
        let Some(config) = &s.config else {
            return INVALID_MODE;
        };
        if s.busy || s.worker.is_some() || s.callbacks.is_some() {
            return INVALID_MODE;
        }
        let infos = unsafe { std::slice::from_raw_parts_mut(infos, count as usize) };
        let mut selected = Vec::with_capacity(infos.len());
        for info in infos.iter() {
            let key = (info.is_input == 1, info.channel as usize);
            if ![0, 1].contains(&info.is_input)
                || info.channel < 0
                || info.channel as usize >= config.channels
                || selected.contains(&key)
            {
                return INVALID_PARAMETER;
            }
            selected.push(key);
        }
        let mut buffers: Vec<_> = selected
            .iter()
            .map(|&(input, channel)| ChannelBuffer {
                input,
                channel,
                data: [
                    vec![0i32; frames as usize].into_boxed_slice(),
                    vec![0i32; frames as usize].into_boxed_slice(),
                ],
            })
            .collect();
        for (info, buffer) in infos.iter_mut().zip(&mut buffers) {
            info.buffers = [
                buffer.data[0].as_mut_ptr().cast(),
                buffer.data[1].as_mut_ptr().cast(),
            ];
        }
        s.buffers = buffers;
        s.selected = selected;
        s.frames = frames as usize;
        s.callbacks = Some(callbacks);
        s.time_info = time_info;
        OK
    })
}
unsafe extern "system" fn dispose(this: *mut c_void) -> Error {
    let d = unsafe { driver(this) };
    let result = d.stop();
    if result != OK {
        return result;
    }
    d.with_state(|s| {
        s.buffers.clear();
        s.selected.clear();
        s.callbacks = None;
        OK
    })
}
unsafe extern "system" fn panel(this: *mut c_void) -> Error {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW},
    };
    if engine::in_callback() {
        return INVALID_MODE;
    }
    let result = (|| {
        let mut module = std::ptr::null_mut();
        if unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                panel as *const () as *const u16,
                &mut module,
            )
        } == 0
        {
            return Err(format!(
                "Locate ASIO DLL: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut path = vec![0u16; 32768];
        let len =
            unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
        if len == 0 || len >= path.len() {
            return Err("ASIO DLL path is unavailable or too long".into());
        }
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&path[..len]));
        let exe = path
            .parent()
            .ok_or("ASIO DLL directory missing")?
            .join("inferno_control.exe");
        let application: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..unsafe { std::mem::zeroed() }
        };
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // Never leak DAW handles (including diagnostic-host pipes) into the panel.
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_NO_WINDOW,
                ptr::null(),
                ptr::null(),
                &startup,
                &mut process,
            )
        } == 0
        {
            return Err(format!(
                "Open {}: {}",
                exe.display(),
                std::io::Error::last_os_error()
            ));
        }
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        Ok::<(), String>(())
    })();
    match result {
        Ok(()) => OK,
        Err(e) => {
            unsafe { driver(this) }.shared.fail(e);
            NOT_PRESENT
        }
    }
}
unsafe extern "system" fn future(_: *mut c_void, selector: i32, _: *mut c_void) -> Error {
    if selector == 10 {
        SUCCESS
    } else {
        NOT_PRESENT
    } // kAsioCanTimeInfo
}
unsafe extern "system" fn output_ready(_: *mut c_void) -> Error {
    NOT_PRESENT
}

pub static VTABLE: AsioVtable = AsioVtable {
    query: crate::com::driver_query,
    add_ref: crate::com::driver_add_ref,
    release: crate::com::driver_release,
    init,
    name,
    version,
    error,
    start,
    stop,
    channels,
    latencies,
    buffer_size,
    can_rate,
    get_rate,
    set_rate,
    clocks,
    set_clock,
    position,
    channel_info,
    create,
    dispose,
    panel,
    future,
    output_ready,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Driver {
        let d = Driver::new();
        d.with_state(|s| {
            s.config = Some(Config {
                asio_frames: 256,
                sample_rate: 48000,
                network_ms: 20,
                channels: 64,
                bind_ip: None,
                loopback: true,
                power_plan: true,
                device_name: None,
            });
            OK
        });
        d
    }

    unsafe extern "C" fn switch(_: i32, _: i32) {}

    #[test]
    fn reports_selected_asio_buffer_to_host() {
        let mut d = configured();
        for frames in config::ASIO_BUFFERS {
            d.with_state(|s| {
                s.config.as_mut().unwrap().asio_frames = frames;
                OK
            });
            let (mut min, mut max, mut preferred, mut granularity) = (0, 0, 0, 0);
            assert_eq!(
                unsafe {
                    buffer_size(
                        (&mut d as *mut Driver).cast(),
                        &mut min,
                        &mut max,
                        &mut preferred,
                        &mut granularity,
                    )
                },
                OK
            );
            assert_eq!((min, max, preferred, granularity), (64, 2048, frames, -1));
        }
    }

    #[test]
    fn rate_changes_restore_channel_profiles_and_reject_invalid_rates() {
        let mut d = configured();
        let this = (&mut d as *mut Driver).cast();
        for (rate, expected) in [(96000.0, 32), (44100.0, 64), (48000.0, 64)] {
            assert_eq!(unsafe { set_rate(this, rate) }, OK);
            let (mut input, mut output) = (0, 0);
            assert_eq!(unsafe { channels(this, &mut input, &mut output) }, OK);
            assert_eq!((input, output), (expected, expected));
        }
        for rate in [f64::NAN, f64::INFINITY, 48000.5, 0.0, 88200.0] {
            assert_eq!(unsafe { set_rate(this, rate) }, NO_CLOCK);
        }
    }

    #[test]
    fn validates_buffers_transactionally_and_blocks_rate_changes_until_dispose() {
        let mut d = configured();
        let this = (&mut d as *mut Driver).cast();
        let callbacks = Callbacks {
            switch: Some(switch),
            rate_changed: None,
            message: None,
            switch_time: None,
        };
        let mut infos = [
            BufferInfo {
                is_input: 1,
                channel: 0,
                buffers: [ptr::null_mut(); 2],
            },
            BufferInfo {
                is_input: 1,
                channel: 0,
                buffers: [ptr::null_mut(); 2],
            },
        ];
        assert_eq!(
            unsafe { create(this, infos.as_mut_ptr(), 2, 256, &callbacks) },
            INVALID_PARAMETER
        );
        assert!(infos[0].buffers[0].is_null());
        infos[1].is_input = 0;
        assert_eq!(
            unsafe { create(this, infos.as_mut_ptr(), 2, 65, &callbacks) },
            INVALID_PARAMETER
        );
        assert_eq!(
            unsafe { create(this, infos.as_mut_ptr(), 2, 256, &callbacks) },
            OK
        );
        assert!(!infos[0].buffers[0].is_null());
        let pointers = infos[0].buffers;
        assert_ne!(pointers[0], pointers[1]);
        assert_eq!(unsafe { set_rate(this, 96000.0) }, INVALID_MODE);
        assert_eq!(unsafe { dispose(this) }, OK);
        assert_eq!(unsafe { set_rate(this, 96000.0) }, OK);
        assert_eq!(
            unsafe { channels(this, ptr::null_mut(), ptr::null_mut()) },
            INVALID_PARAMETER
        );
    }

    #[test]
    fn network_start_without_interface_fails_closed_and_preserves_buffers() {
        let mut d = configured();
        d.with_state(|s| {
            if let Some(config) = &mut s.config {
                config.loopback = false;
            }
            OK
        });
        let this = (&mut d as *mut Driver).cast();
        let callbacks = Callbacks {
            switch: Some(switch),
            rate_changed: None,
            message: None,
            switch_time: None,
        };
        let mut info = BufferInfo {
            is_input: 1,
            channel: 0,
            buffers: [ptr::null_mut(); 2],
        };
        assert_eq!(unsafe { create(this, &mut info, 1, 256, &callbacks) }, OK);
        let pointers = info.buffers;
        assert_eq!(d.start(), NO_CLOCK);
        assert!(!d.shared.running.load(Ordering::Acquire));
        assert!(d
            .shared
            .error
            .lock()
            .unwrap()
            .contains("INFERNO_ASIO_BIND_IP"));
        d.with_state(|s| {
            assert_eq!(s.buffers.len(), 1);
            assert_eq!(
                s.buffers[0].data[0].as_mut_ptr().cast::<c_void>(),
                pointers[0]
            );
            OK
        });
        assert_eq!(unsafe { dispose(this) }, OK);
    }

    #[test]
    fn reports_higher_negotiated_receive_latency_and_resets_it_on_stop() {
        let mut d = configured();
        d.with_state(|s| {
            if let Some(config) = &mut s.config {
                config.loopback = false;
            }
            OK
        });
        let this = (&mut d as *mut Driver).cast();
        d.shared.input_latency.store(4096, Ordering::Release);
        let (mut input, mut output) = (0, 0);
        assert_eq!(unsafe { latencies(this, &mut input, &mut output) }, OK);
        assert_eq!((input, output), (4096, 1216));
        assert_eq!(d.stop(), OK);
        assert_eq!(unsafe { latencies(this, &mut input, &mut output) }, OK);
        assert_eq!((input, output), (1216, 1216));
    }
}
