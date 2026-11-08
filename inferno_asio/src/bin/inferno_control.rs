#![windows_subsystem = "windows"]
#![deny(unsafe_op_in_unsafe_fn)]

use inferno_asio::config::{max_channels, settings_path, valid_device_name, Settings, ASIO_BUFFERS};
use std::{
    cell::RefCell,
    ffi::OsStr,
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::{
            CreateFontW, CreateSolidBrush, DrawTextW, FillRect, FrameRect, GetStockObject,
            SetBkColor, SetBkMode, SetDCBrushColor, SetTextColor, CLEARTYPE_QUALITY, DC_BRUSH,
            DEFAULT_CHARSET, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HBRUSH, HDC,
            TRANSPARENT,
        },
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{BST_CHECKED, DRAWITEMSTRUCT, EM_LIMITTEXT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED},
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::*,
    },
};

const SAVE: u16 = 100;
const RELOAD: u16 = 101;
const REFRESH: u16 = 102;
const TEST: u16 = 103;
const RATE: u16 = 104;
const MODE: u16 = 105;
const POWER: u16 = 106;
const NAME: u16 = 107;
const SUMMARY: u16 = 108;
const SMOKE: u32 = WM_APP + 1;
const RATES: [u32; 3] = [44100, 48000, 96000];
const BUFFERS: [u32; 4] = [20, 10, 6, 4];

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}
const BACKGROUND: COLORREF = rgb(32, 32, 32);
const FIELD: COLORREF = rgb(48, 48, 48);
const BUTTON: COLORREF = rgb(58, 58, 58);
const PRESSED: COLORREF = rgb(80, 80, 80);
const BORDER: COLORREF = rgb(96, 96, 96);
const TEXT: COLORREF = rgb(230, 230, 230);
const DIMMED: COLORREF = rgb(140, 140, 140);
const ACCENT: COLORREF = rgb(96, 205, 255);
const CLIENT: (i32, i32) = (460, 436);

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn set_text(window: HWND, text: &str) {
    unsafe {
        SetWindowTextW(window, wide(text).as_ptr());
    }
}

fn select(window: HWND, index: usize) {
    unsafe {
        SendMessageW(window, CB_SETCURSEL, index, 0);
    }
}

fn selection(window: HWND) -> Result<usize, String> {
    let index = unsafe { SendMessageW(window, CB_GETCURSEL, 0, 0) };
    if index < 0 {
        Err("Select a value in every dropdown.".into())
    } else {
        Ok(index as usize)
    }
}

fn add(window: HWND, text: &str) -> Result<(), String> {
    let result = unsafe { SendMessageW(window, CB_ADDSTRING, 0, wide(text).as_ptr() as isize) };
    if result < 0 {
        Err("Could not populate a dropdown.".into())
    } else {
        Ok(())
    }
}

fn show_error(window: HWND, error: &str) {
    unsafe {
        MessageBoxW(
            window,
            wide(error).as_ptr(),
            wide("Inferno ARM ASIO").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

struct TestJob {
    receiver: mpsc::Receiver<Result<String, String>>,
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

struct App {
    window: HWND,
    rate: HWND,
    channels: HWND,
    buffer: HWND,
    asio_buffer: HWND,
    interface: HWND,
    mode: HWND,
    power: HWND,
    device_name: HWND,
    status: HWND,
    test_button: HWND,
    summary: HWND,
    font: isize,
    path: PathBuf,
    interfaces: Vec<Option<std::net::Ipv4Addr>>,
    job: Option<TestJob>,
    smoke: bool,
    smoke_result: Result<(), String>,
}

impl App {
    fn new(path: PathBuf, smoke: bool) -> Self {
        Self {
            window: ptr::null_mut(),
            rate: ptr::null_mut(),
            channels: ptr::null_mut(),
            buffer: ptr::null_mut(),
            asio_buffer: ptr::null_mut(),
            interface: ptr::null_mut(),
            mode: ptr::null_mut(),
            power: ptr::null_mut(),
            device_name: ptr::null_mut(),
            status: ptr::null_mut(),
            test_button: ptr::null_mut(),
            summary: ptr::null_mut(),
            font: 0,
            path,
            interfaces: Vec::new(),
            job: None,
            smoke,
            smoke_result: Ok(()),
        }
    }

    fn control(
        &self,
        class: &str,
        text: &str,
        style: u32,
        bounds: (i32, i32, i32, i32),
        id: u16,
    ) -> Result<HWND, String> {
        let (x, y, width, height) = bounds;
        let window = unsafe {
            CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                width,
                height,
                self.window,
                id as usize as HMENU,
                GetModuleHandleW(ptr::null()),
                ptr::null(),
            )
        };
        if window.is_null() {
            return Err(format!(
                "Create {class}: {}",
                std::io::Error::last_os_error()
            ));
        }
        unsafe {
            SendMessageW(window, WM_SETFONT, self.font as usize, 1);
        }
        Ok(window)
    }

    fn label(&self, text: &str, x: i32, y: i32, width: i32, height: i32) -> Result<HWND, String> {
        self.control("STATIC", text, 0, (x, y, width, height), 0)
    }

    fn button(&self, text: &str, bounds: (i32, i32, i32, i32), id: u16) -> Result<HWND, String> {
        self.control("BUTTON", text, WS_TABSTOP | BS_OWNERDRAW as u32, bounds, id)
    }

    fn combo(&self, x: i32, y: i32, width: i32, id: u16) -> Result<HWND, String> {
        self.control(
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            (x, y, width, 260),
            id,
        )
    }

    fn initialize(&mut self) -> Result<(), String> {
        let face = wide("Segoe UI");
        self.font = unsafe {
            CreateFontW(
                -12, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0,
                CLEARTYPE_QUALITY as u32, 0, face.as_ptr(),
            )
        } as isize;
        let (left, right, width) = (12, 240, 208);
        self.label("Inferno ARM ASIO  \u{2022}  unofficial Dante-compatible ASIO driver", left, 10, 436, 18)?;
        self.label("Ethernet interface", left, 36, 300, 16)?;
        self.interface = self.combo(left, 53, 348, 0)?;
        self.button("Refresh", (368, 52, 80, 24), REFRESH)?;
        self.label("Sample rate", left, 86, width, 16)?;
        self.label("Inputs / outputs", right, 86, width, 16)?;
        self.rate = self.combo(left, 103, width, RATE)?;
        self.channels = self.combo(right, 103, width, 0)?;
        for text in ["44.1 kHz", "48 kHz", "96 kHz"] {
            add(self.rate, text)?;
        }
        self.label("Network latency", left, 136, width, 16)?;
        self.label("ASIO buffer", right, 136, width, 16)?;
        self.buffer = self.combo(left, 153, width, 0)?;
        self.asio_buffer = self.combo(right, 153, width, 0)?;
        for text in ["20 ms (recommended)", "10 ms", "6 ms", "4 ms"] {
            add(self.buffer, text)?;
        }
        for frames in ASIO_BUFFERS {
            add(self.asio_buffer, &format!("{frames} samples"))?;
        }
        self.label("Dante device name (empty = default)", left, 186, width, 16)?;
        self.label("Operating mode", right, 186, width, 16)?;
        self.device_name = self.control(
            "EDIT",
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            (left, 203, width, 23),
            NAME,
        )?;
        unsafe {
            SendMessageW(self.device_name, EM_LIMITTEXT, 31, 0);
        }
        self.mode = self.combo(right, 203, width, MODE)?;
        for text in ["Network (Dante / PTP)", "Offline diagnostic loopback"] {
            add(self.mode, text)?;
        }
        self.power = self.control(
            "BUTTON",
            "Switch to low-latency power plan while streaming (also on battery)",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            (left, 238, 436, 20),
            POWER,
        )?;
        self.summary = self.control("STATIC", "", 0, (left, 266, 436, 32), SUMMARY)?;
        self.button("Save", (left, 306, 100, 28), SAVE)?;
        self.button("Reload", (120, 306, 100, 28), RELOAD)?;
        self.test_button = self.button("Run offline audio test", (right, 306, width, 28), TEST)?;
        self.status = self.label("", left, 344, 436, 46)?;
        self.label("Saved changes apply automatically: a running driver asks the DAW to\r\nreset the ASIO device. INFERNO_ASIO_* environment variables still win.", left, 396, 436, 32)?;
        if let Err(e) = self.load() {
            self.apply(Settings::default())?;
            set_text(self.status, &format!("Could not load saved settings: {e}\r\nDefaults shown. Save settings to replace the invalid configuration."));
        }
        if unsafe { SetTimer(self.window, 1, 100, None) } == 0 {
            return Err("Cannot create control-panel status timer".into());
        }
        Ok(())
    }

    fn channel_options(&self, selected: Option<usize>) -> Result<(), String> {
        let rate = *RATES
            .get(selection(self.rate)?)
            .ok_or("Invalid sample rate")?;
        let max = max_channels(rate)?;
        unsafe {
            SendMessageW(self.channels, CB_RESETCONTENT, 0, 0);
        }
        add(self.channels, &format!("Maximum ({max} x {max})"))?;
        for count in 1..=max {
            add(self.channels, &format!("{count} x {count}"))?;
        }
        select(
            self.channels,
            selected.filter(|count| *count <= max).unwrap_or(0),
        );
        Ok(())
    }

    fn refresh(&mut self, current: Option<std::net::Ipv4Addr>) -> Result<(), String> {
        self.interfaces.clear();
        unsafe {
            SendMessageW(self.interface, CB_RESETCONTENT, 0, 0);
        }
        self.interfaces.push(None);
        add(self.interface, "Select a wired Ethernet IPv4 address...")?;
        for interface in netdev::get_interfaces() {
            let name = interface
                .friendly_name
                .as_deref()
                .unwrap_or(&interface.name);
            for network in &interface.ipv4 {
                let ip = network.addr();
                if ip.is_loopback() || ip.is_unspecified() {
                    continue;
                }
                add(self.interface, &format!("{name}  -  {ip}"))?;
                self.interfaces.push(Some(ip));
            }
        }
        if current.is_some() && !self.interfaces.contains(&current) {
            add(
                self.interface,
                &format!(
                    "Saved address {} (adapter unavailable)",
                    current.ok_or("Missing saved IP")?
                ),
            )?;
            self.interfaces.push(current);
        }
        select(
            self.interface,
            self.interfaces
                .iter()
                .position(|ip| *ip == current)
                .unwrap_or(0),
        );
        Ok(())
    }

    fn load(&mut self) -> Result<(), String> {
        let settings = Settings::load(&self.path)?;
        self.apply(settings)?;
        set_text(
            self.status,
            &format!("Settings loaded from:\r\n{}", self.path.display()),
        );
        Ok(())
    }

    fn apply(&mut self, settings: Settings) -> Result<(), String> {
        select(
            self.asio_buffer,
            ASIO_BUFFERS
                .iter()
                .position(|b| *b == settings.asio_frames)
                .ok_or("Invalid saved ASIO buffer")?,
        );
        select(
            self.rate,
            RATES
                .iter()
                .position(|r| *r == settings.sample_rate)
                .ok_or("Invalid saved rate")?,
        );
        select(
            self.buffer,
            BUFFERS
                .iter()
                .position(|b| *b == settings.network_ms)
                .ok_or("Invalid saved buffer")?,
        );
        select(self.mode, settings.loopback as usize);
        unsafe {
            SendMessageW(self.power, BM_SETCHECK, settings.power_plan as usize, 0);
        }
        set_text(self.device_name, settings.device_name.as_deref().unwrap_or(""));
        self.channel_options(settings.channels)?;
        self.refresh(settings.bind_ip)?;
        self.update_summary()
    }

    fn name_text(&self) -> String {
        let mut buffer = [0u16; 64];
        let length = unsafe { GetWindowTextW(self.device_name, buffer.as_mut_ptr(), 64) };
        String::from_utf16_lossy(&buffer[..length.max(0) as usize]).trim().to_owned()
    }

    fn read(&self) -> Result<Settings, String> {
        let name = self.name_text();
        if !name.is_empty() && !valid_device_name(&name) {
            return Err("Dante device name: 1-31 letters, digits or '-', not starting or ending with '-'.".into());
        }
        let settings = Settings {
            device_name: (!name.is_empty()).then_some(name),
            ..self.read_controls()?
        };
        settings.resolve()?;
        Ok(settings)
    }

    /// Everything except the free-text device name, which may be mid-edit.
    fn read_controls(&self) -> Result<Settings, String> {
        let selected = selection(self.channels)?;
        let settings = Settings {
            asio_frames: *ASIO_BUFFERS
                .get(selection(self.asio_buffer)?)
                .ok_or("Invalid ASIO buffer")?,
            sample_rate: *RATES
                .get(selection(self.rate)?)
                .ok_or("Invalid sample rate")?,
            network_ms: *BUFFERS
                .get(selection(self.buffer)?)
                .ok_or("Invalid network buffer")?,
            channels: (selected != 0).then_some(selected),
            bind_ip: *self
                .interfaces
                .get(selection(self.interface)?)
                .ok_or("Invalid interface")?,
            loopback: selection(self.mode)? == 1,
            power_plan: unsafe { SendMessageW(self.power, BM_GETCHECK, 0, 0) } == BST_CHECKED as isize,
            device_name: None,
        };
        settings.resolve()?;
        Ok(settings)
    }

    fn update_summary(&self) -> Result<(), String> {
        let config = self.read_controls()?.resolve()?;
        set_text(
            self.summary,
            &format!(
                "{} in + {} out  \u{2022}  {} Hz  \u{2022}  {} ms network  \u{2022}  {} samples ASIO\r\n{}",
                config.channels,
                config.channels,
                config.sample_rate,
                config.network_ms,
                config.asio_frames,
                if config.loopback {
                    "Diagnostic mode: no Dante network announcement or PTP."
                } else {
                    "Wired Ethernet and an external Dante PTPv1 leader required."
                }
            ),
        );
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        let settings = self.read()?;
        if !settings.loopback && settings.bind_ip.is_none() {
            return Err(
                "Select your wired Ethernet IPv4 address before saving network mode.".into(),
            );
        }
        settings.save(&self.path)?;
        set_text(self.status, "Settings saved. A streaming driver resets itself to apply them.");
        Ok(())
    }

    fn start_test(&mut self) -> Result<(), String> {
        if self.job.is_some() {
            return Err("An offline test is already running.".into());
        }
        let settings = self.read()?;
        let exe = std::env::current_exe().map_err(|e| format!("Locate control panel: {e}"))?;
        let directory = exe.parent().ok_or("No control-panel directory")?;
        let probe = directory.join("asio_probe.exe");
        let dll = directory.join("inferno_asio.dll");
        if !probe.is_file() || !dll.is_file() {
            return Err(
                "Keep inferno_control.exe, asio_probe.exe, and inferno_asio.dll together.".into(),
            );
        }
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("ASIO offline test".into())
            .spawn(move || {
                let result = offline_test(probe, dll, settings, cancelled);
                let _ = sender.send(result);
            })
            .map_err(|e| format!("Start offline test: {e}"))?;
        self.job = Some(TestJob {
            receiver,
            cancel,
            thread,
        });
        unsafe {
            EnableWindow(self.test_button, 0);
        }
        set_text(self.status, "Testing the local ARM64 ASIO DLL... No network audio or PTP traffic.\r\nThis test does not verify physical Dante interoperability.");
        Ok(())
    }

    fn poll_test(&mut self) {
        let result = self
            .job
            .as_ref()
            .and_then(|job| match job.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Offline test worker stopped unexpectedly".into()))
                }
            });
        if let Some(result) = result {
            if let Some(job) = self.job.take() {
                if job.thread.join().is_err() {
                    self.fail("Offline test worker panicked".into());
                    return;
                }
            }
            unsafe {
                EnableWindow(self.test_button, 1);
            }
            match result {
                Ok(text) => {
                    set_text(self.status, &format!("{}\r\nLocal loopback only; physical Dante compatibility remains unverified.", text.trim()));
                    if self.smoke {
                        unsafe {
                            DestroyWindow(self.window);
                        }
                    }
                }
                Err(e) => self.fail(e),
            }
        }
    }

    fn fail(&mut self, error: String) {
        set_text(self.status, &error);
        if self.smoke {
            self.smoke_result = Err(error);
            unsafe {
                DestroyWindow(self.window);
            }
        } else {
            show_error(self.window, &error);
        }
    }

    fn smoke_test(&mut self) -> Result<(), String> {
        select(self.rate, 2);
        self.channel_options(Some(64))?;
        if selection(self.channels)? != 0 {
            return Err("Rate change did not reset invalid channel count".into());
        }
        select(self.channels, 2);
        select(self.buffer, 2);
        select(self.asio_buffer, 3);
        select(self.mode, 1);
        unsafe {
            SendMessageW(self.power, BM_SETCHECK, 0, 0);
        }
        set_text(self.device_name, "Smoke-Test-1");
        self.update_summary()?;
        self.save()?;
        self.load()?;
        let saved = self.read()?;
        if saved.sample_rate != 96000
            || saved.network_ms != 6
            || saved.asio_frames != 512
            || saved.channels != Some(2)
            || !saved.loopback
            || saved.power_plan
            || saved.device_name.as_deref() != Some("Smoke-Test-1")
        {
            return Err("GUI control/save/reload roundtrip failed".into());
        }
        set_text(self.device_name, "-bad");
        if self.read().is_ok() {
            return Err("Invalid device name was accepted".into());
        }
        set_text(self.device_name, "Smoke-Test-1");
        self.start_test()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Release);
            if job.thread.join().is_err() {
                show_error(
                    ptr::null_mut(),
                    "Offline test worker panicked while closing.",
                );
            }
        }
    }
}

fn offline_test(
    probe: PathBuf,
    dll: PathBuf,
    settings: Settings,
    cancelled: Arc<AtomicBool>,
) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("inferno-offline-{}", std::process::id()));
    std::fs::create_dir(&path).map_err(|e| format!("Create offline test directory: {e}"))?;
    let config_path = path.join("settings.json");
    let result = (|| {
        let mut settings = settings;
        settings.loopback = true;
        settings.save(&config_path)?;
        let mut child = Command::new(probe)
            .arg(dll)
            .env("INFERNO_ASIO_CONFIG", &config_path)
            .env_remove("INFERNO_ASIO_MODE")
            .env_remove("INFERNO_ASIO_RATE")
            .env_remove("INFERNO_ASIO_CHANNELS")
            .env_remove("INFERNO_ASIO_NETWORK_MS")
            .env_remove("INFERNO_ASIO_BIND_IP")
            .env_remove("INFERNO_PROBE_FRAMES")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("Launch offline test: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                child
                    .kill()
                    .map_err(|e| format!("Stop offline test: {e}"))?;
                child
                    .wait()
                    .map_err(|e| format!("Reap offline test: {e}"))?;
                return Err("Offline test cancelled or timed out.".into());
            }
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    child
                        .kill()
                        .map_err(|kill| format!("Test status: {e}; stop test: {kill}"))?;
                    child
                        .wait()
                        .map_err(|wait| format!("Test status: {e}; reap test: {wait}"))?;
                    return Err(format!("Test status: {e}"));
                }
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|e| format!("Collect offline test: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Offline test failed:\r\n{}\r\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let config = settings.resolve()?;
        let expected = format!(
            "{}x{}, {} Hz, {} frames",
            config.channels, config.channels, config.sample_rate, config.asio_frames
        );
        if !text.contains(&expected) {
            return Err(format!(
                "DLL did not report the saved profile ({expected}): {text}"
            ));
        }
        Ok(text)
    })();
    if config_path.exists() {
        std::fs::remove_file(&config_path)
            .map_err(|e| format!("{result:?}; clean offline settings: {e}"))?;
    }
    std::fs::remove_dir(&path).map_err(|e| format!("{result:?}; clean offline directory: {e}"))?;
    result
}

fn dc_brush(dc: HDC, color: COLORREF) -> LRESULT {
    unsafe {
        SetDCBrushColor(dc, color);
        GetStockObject(DC_BRUSH) as LRESULT
    }
}

/// Dark colours for the classic (unthemed) child controls.
fn control_color(message: u32, dc: HDC, control: HWND) -> LRESULT {
    let field = matches!(message, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX);
    let background = if field { FIELD } else { BACKGROUND };
    let text = if unsafe { GetDlgCtrlID(control) } == SUMMARY as i32 { ACCENT } else { TEXT };
    unsafe {
        SetTextColor(dc, text);
        SetBkColor(dc, background);
    }
    dc_brush(dc, background)
}

fn draw_button(item: &DRAWITEMSTRUCT) {
    let dc = item.hDC;
    let rect = item.rcItem;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let disabled = item.itemState & ODS_DISABLED != 0;
    let focused = item.itemState & ODS_FOCUS != 0;
    let mut text = [0u16; 64];
    let length = unsafe { GetWindowTextW(item.hwndItem, text.as_mut_ptr(), 64) };
    unsafe {
        FillRect(dc, &rect, dc_brush(dc, if pressed { PRESSED } else { BUTTON }) as HBRUSH);
        FrameRect(dc, &rect, dc_brush(dc, if focused { ACCENT } else { BORDER }) as HBRUSH);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, if disabled { DIMMED } else { TEXT });
        let mut bounds = rect;
        DrawTextW(dc, text.as_ptr(), length, &mut bounds, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
            return control_color(message, wparam as HDC, lparam as HWND);
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
            draw_button(item);
            return 1;
        }
        WM_CLOSE => {
            unsafe {
                DestroyWindow(window);
            }
            return 0;
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(window, 1);
                PostQuitMessage(0);
            }
            return 0;
        }
        WM_COMMAND | WM_TIMER | SMOKE => {}
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut RefCell<App>;
    if !pointer.is_null() {
        // MessageBox and control updates may synchronously re-enter the window procedure.
        let Ok(mut app) = (unsafe { &*pointer }).try_borrow_mut() else {
            return 0;
        };
        match message {
            WM_COMMAND => {
                let id = (wparam & 0xffff) as u16;
                let notification = ((wparam >> 16) & 0xffff) as u32;
                let result = match id {
                    SAVE => app.save(),
                    RELOAD => app.load(),
                    REFRESH => app.read().and_then(|s| app.refresh(s.bind_ip)),
                    TEST => app.start_test(),
                    RATE if notification == CBN_SELCHANGE => {
                        let selected = selection(app.channels).ok().filter(|n| *n != 0);
                        app.channel_options(selected)
                            .and_then(|_| app.update_summary())
                    }
                    MODE if notification == CBN_SELCHANGE => app.update_summary(),
                    _ if notification == CBN_SELCHANGE => app.update_summary(),
                    _ => Ok(()),
                };
                if let Err(e) = result {
                    app.fail(e);
                }
                return 0;
            }
            WM_TIMER => {
                app.poll_test();
                return 0;
            }
            SMOKE => {
                if let Err(e) = app.smoke_test() {
                    app.fail(e);
                }
                return 0;
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn run() -> Result<(), String> {
    let smoke = std::env::args().any(|arg| arg == "--smoke-test");
    let path = if smoke {
        std::env::temp_dir()
            .join(format!("inferno-gui-smoke-{}", std::process::id()))
            .join("settings.json")
    } else {
        settings_path()?
    };
    let app = Box::new(RefCell::new(App::new(path.clone(), smoke)));
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let name = wide("InfernoArmAsioControl");
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { CreateSolidBrush(BACKGROUND) },
        lpszClassName: name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(format!(
            "Register window: {}",
            std::io::Error::last_os_error()
        ));
    }
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let mut bounds = RECT {
        left: 0,
        top: 0,
        right: CLIENT.0,
        bottom: CLIENT.1,
    };
    if unsafe { AdjustWindowRectEx(&mut bounds, style, 0, 0) } == 0 {
        return Err("Calculate window bounds failed".into());
    }
    let window = unsafe {
        CreateWindowExW(
            0,
            name.as_ptr(),
            wide("Inferno ARM ASIO - Control Panel").as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if window.is_null() {
        return Err(format!(
            "Create control panel: {}",
            std::io::Error::last_os_error()
        ));
    }
    app.borrow_mut().window = window;
    // Dark title bar (Windows 10 20H1+ / 11); older builds just keep the light caption.
    let dark: i32 = 1;
    unsafe {
        DwmSetWindowAttribute(window, 20, (&dark as *const i32).cast(), 4);
    }
    unsafe {
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            (&*app as *const RefCell<App>) as isize,
        );
    }
    if let Err(e) = app.borrow_mut().initialize() {
        unsafe {
            DestroyWindow(window);
        }
        return Err(e);
    }
    unsafe {
        ShowWindow(window, SW_SHOW);
    }
    if smoke {
        unsafe {
            PostMessageW(window, SMOKE, 0, 0);
        }
    }
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if result == -1 {
            return Err(format!(
                "Window message loop: {}",
                std::io::Error::last_os_error()
            ));
        }
        if result == 0 {
            break;
        }
        if unsafe { IsDialogMessageW(window, &message) } == 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    if smoke {
        std::fs::remove_file(&path).map_err(|e| format!("Clean smoke settings: {e}"))?;
        std::fs::remove_dir(path.parent().ok_or("Smoke settings directory missing")?)
            .map_err(|e| format!("Clean smoke directory: {e}"))?;
    }
    let result = app.borrow().smoke_result.clone();
    result
}

fn main() {
    // Power-plan crash recovery modes started by the ASIO driver / HKCU Run entry; no UI.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--power-guard") {
        if let Some(pid) = args.get(i + 1).and_then(|p| p.parse().ok()) {
            inferno_asio::power::guard(pid);
        }
        return;
    }
    if args.iter().any(|a| a == "--power-restore") {
        inferno_asio::power::restore_orphaned();
        return;
    }
    if let Err(e) = run() {
        if std::env::args().any(|arg| arg == "--smoke-test") {
            eprintln!("{e}");
        } else {
            show_error(ptr::null_mut(), &e);
        }
        std::process::exit(1);
    }
}
