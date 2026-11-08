use crate::{
    abi::*,
    config::{self, Config},
    driver::{ChannelBuffer, Shared},
    ptp,
    timer::{self, Timer},
};
use inferno_aoip::device_server::{ControlRequest, DeviceServer, Settings};
use std::{
    cell::Cell,
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc::SyncSender,
        Arc,
    },
    time::{Duration, Instant},
};

const ASIO_RESET_REQUEST: i32 = 3;

/// Per-channel peak magnitudes since the last Dante Controller heartbeat.
struct Meters(Vec<AtomicI32>);

impl Meters {
    fn new(channels: usize) -> Arc<Self> {
        Arc::new(Self((0..channels).map(|_| AtomicI32::new(0)).collect()))
    }

    fn record(&self, channel: usize, samples: &[i32]) {
        if let Some(meter) = self.0.get(channel) {
            let peak = samples.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
            meter.fetch_max(peak, Ordering::Relaxed);
        }
    }

    /// Dante level bytes: -2 × dBFS, 0xc1 for silence (as Dante Virtual Soundcard reports).
    fn take_levels(&self) -> Vec<u8> {
        self.0
            .iter()
            .map(|meter| {
                let peak = meter.swap(0, Ordering::Relaxed);
                if peak <= 0 {
                    return 0xc1;
                }
                let level = -40.0 * (peak as f64 / i32::MAX as f64).log10();
                level.round().clamp(1.0, 0xc1 as f64) as u8
            })
            .collect()
    }
}

/// Polls settings.json while streaming; a change asks the host to reset the driver,
/// which reloads the configuration (no manual restart needed). Stops when the guard drops.
fn watch_settings(running: Config, reset: Arc<AtomicBool>) -> StopOnDrop {
    let done = Arc::new(AtomicBool::new(false));
    let stop = StopOnDrop(done.clone());
    let spawned = std::thread::Builder::new()
        .name("Inferno ASIO settings".into())
        .spawn(move || {
            while !done.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(500));
                if let Ok((config, _)) = config::load_configuration() {
                    if config != running {
                        log::info!("settings changed; requesting ASIO reset");
                        reset.store(true, Ordering::Release);
                        return;
                    }
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("settings watcher: {e}");
    }
    stop
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn control_hook(reset: Arc<AtomicBool>) -> inferno_aoip::device_server::ControlHook {
    Arc::new(move |request| {
        let result = match request {
            ControlRequest::Rename(name) => {
                if name.as_deref().is_some_and(|n| !config::valid_device_name(n)) {
                    return false;
                }
                log::info!("Dante Controller rename: {name:?}");
                config::update_saved(|s| s.device_name = name)
            }
            ControlRequest::SetLatencyNs(ns) => {
                let ms = config::snap_network_ms(ns);
                log::info!("Dante Controller latency: {ns} ns -> {ms} ms");
                config::update_saved(|s| s.network_ms = ms)
            }
            ControlRequest::Reboot => {
                log::info!("Dante Controller reboot request");
                Ok(())
            }
        };
        match result {
            Ok(()) => {
                reset.store(true, Ordering::Release);
                true
            }
            Err(e) => {
                log::warn!("Dante Controller change rejected: {e}");
                false
            }
        }
    })
}

thread_local! { static IN_CALLBACK: Cell<bool> = const { Cell::new(false) }; }
pub fn in_callback() -> bool {
    IN_CALLBACK.with(Cell::get)
}

pub fn notify(callbacks: Callbacks, selector: i32) {
    if let Some(message) = callbacks.message {
        IN_CALLBACK.with(|flag| flag.set(true));
        unsafe {
            message(selector, 0, std::ptr::null_mut(), std::ptr::null_mut());
        }
        IN_CALLBACK.with(|flag| flag.set(false));
    }
}

fn callback(
    callbacks: Callbacks,
    time_info: bool,
    index: usize,
    position: u64,
    stamp: u64,
    rate: u32,
) {
    IN_CALLBACK.with(|flag| flag.set(true));
    if time_info {
        let mut time = Time {
            reserved: [0; 4],
            info: TimeInfo {
                speed: 1.0,
                system_time: stamp.into(),
                sample_position: position.into(),
                sample_rate: rate as f64,
                flags: 15,
                reserved: [0; 12],
            },
            code: TimeCode {
                speed: 0.0,
                samples: Split64::default(),
                flags: 0,
                future: [0; 64],
            },
        };
        if let Some(switch) = callbacks.switch_time {
            unsafe {
                switch(&mut time, index as i32, 1);
            }
        }
    } else if let Some(switch) = callbacks.switch {
        unsafe {
            switch(index as i32, 1);
        }
    }
    IN_CALLBACK.with(|flag| flag.set(false));
}

pub struct Run {
    pub config: Config,
    pub frames: usize,
    pub callbacks: Callbacks,
    pub time_info: bool,
    pub shared: Arc<Shared>,
    pub stop: Arc<AtomicBool>,
    pub ready: SyncSender<Result<(), String>>,
}

impl Run {
    pub fn run(&self, buffers: &mut [ChannelBuffer]) -> Result<(), String> {
        let timer = Timer::new()?;
        let _resolution = timer::TimerResolution::request();
        let _audio_thread = timer::AudioThread::register();
        if self.config.loopback {
            return self.loopback(buffers, &timer);
        }
        crate::logfile::install();
        log::info!(
            "engine start: {} Hz, {} ch, {} ASIO frames, {} ms network",
            self.config.sample_rate,
            self.config.channels,
            self.frames,
            self.config.network_ms
        );
        thread_local! {
            static NETWORK_AUDIO_THREAD: std::cell::RefCell<Option<timer::AudioThread>> =
                const { std::cell::RefCell::new(None) };
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            // PTP timestamps are taken in user space; scheduling delay becomes clock error.
            .on_thread_start(|| {
                NETWORK_AUDIO_THREAD.with(|t| *t.borrow_mut() = Some(timer::AudioThread::register()))
            })
            .on_thread_stop(|| NETWORK_AUDIO_THREAD.with(|t| drop(t.borrow_mut().take())))
            .enable_all()
            .build()
            .map_err(|e| format!("Network runtime: {e}"))?;
        let interface = self
            .config
            .bind_ip
            .ok_or("Set INFERNO_ASIO_BIND_IP to the wired Ethernet IPv4 address")?;
        let mut parameters = BTreeMap::from([
            // Audinate Controller's local monitoring service owns the standard management ports.
            ("ALT_PORT".into(), "45000".into()),
            ("SAMPLE_RATE".into(), self.config.sample_rate.to_string()),
            ("RX_CHANNELS".into(), self.config.channels.to_string()),
            ("TX_CHANNELS".into(), self.config.channels.to_string()),
            (
                "RX_LATENCY_NS".into(),
                (self.config.network_ms * 1_000_000).to_string(),
            ),
            (
                "TX_LATENCY_NS".into(),
                (self.config.network_ms * 1_000_000).to_string(),
            ),
            ("USE_SAFE_CLOCK".into(), "false".into()),
        ]);
        if let Some(name) = &self.config.device_name {
            parameters.insert("NAME".into(), name.clone());
        }
        let mut settings =
            Settings::new("Inferno ARM ASIO", "ArmASIO", Some(interface), &parameters);
        if settings.self_info.mac_address.octets() == [0; 6] {
            return Err("The selected IPv4 address does not belong to an Ethernet adapter with a MAC address".into());
        }
        // Before PTP lock-in, which also suffers from wake-up latency.
        let _power = crate::power::LowLatencyPower::activate(self.config.power_plan);
        let reset = Arc::new(AtomicBool::new(false));
        settings.control_hook = Some(control_hook(reset.clone()));
        settings.rx_buffer_samples = 32768;
        settings.latency_ref_samples = self.config.network_frames() / 2;
        // Restored unicast flows get new handles the receivers don't know, so they can't unsubscribe.
        settings.restore_tx_flows = false;
        let (ptp, server, mut receiver, mut transmitter) = runtime.block_on(async {
            let session = ptp::Session::start(interface, settings.self_info.mac_address.octets())?;
            let deadline = Instant::now() + Duration::from_secs(12);
            while session.updates.borrow().is_none() {
                if self.stop.load(Ordering::Acquire) { return Err("Startup cancelled".into()); }
                if Instant::now() >= deadline {
                    session.shutdown().await?;
                    return Err("No stable PTPv1 leader: check wired Ethernet, UDP 319/320, and Dante clock leader".into());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let clock = usrvclock::AsyncClient::from_updates(session.updates.clone());
            let mut server = DeviceServer::start_with_clock(settings, clock).await;
            let receiver = server.receive_realtime().await;
            let overlay = (*session.updates.borrow()).ok_or("PTP clock lost during startup")?;
            let timestamp = (overlay.now_ns() as u128 * self.config.sample_rate as u128 / 1_000_000_000) as usize;
            let transmitter = server.transmit_with_push_at(self.config.channels, timestamp).await;
            Ok::<_, String>((session, server, receiver, transmitter))
        })?;
        let rx_meters = Meters::new(self.config.channels);
        let tx_meters = Meters::new(self.config.channels);
        {
            let (rx, tx) = (rx_meters.clone(), tx_meters.clone());
            // After transmit_with_push_at, which installs its own (ring-wide) supplier.
            server.set_level_suppliers(
                Some(Box::new(move || rx.take_levels())),
                Some(Box::new(move || tx.take_levels())),
            );
        }
        let _watcher = watch_settings(self.config.clone(), reset.clone());
        let mut reset_sent = false;
        let mut tx = vec![vec![0; self.frames]; self.config.channels];
        let origin = Instant::now();
        let start = loop {
            if let Some(timestamp) = receiver
                .clock()
                .now_in_timebase(self.config.sample_rate as u64)
            {
                break timestamp;
            }
            if origin.elapsed() > Duration::from_secs(1) {
                runtime.block_on(server.shutdown());
                runtime.block_on(ptp.shutdown())?;
                return Err("PTP clock did not reach the audio engine".into());
            }
            timer.sleep_ns(1_000_000)?;
        };
        let performance_start = timer::performance_ns()?;
        let system_start = timer::system_timestamp_ns();
        let result = (|| {
            self.ready
                .send(Ok(()))
                .map_err(|_| "ASIO host cancelled startup")?;
            self.shared.started.store(true, Ordering::Release);
            let mut position = 0u64;
            let mut index = 0usize;
            let mut late_blocks = 0;
            while !self.stop.load(Ordering::Acquire) {
                let end = start
                    .wrapping_add(position)
                    .wrapping_add(self.frames as u64);
                loop {
                    if self.stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let overlay = (*ptp.updates.borrow())
                        .ok_or("PTP synchronization lost; stopping audio")?;
                    if ptp::now_ns().wrapping_sub(overlay.last_sync).abs() > 5_000_000_000 {
                        return Err("PTP delay measurements expired; stopping audio".into());
                    }
                    let now = receiver
                        .clock()
                        .now_in_timebase(self.config.sample_rate as u64)
                        .ok_or("Media clock unavailable")?;
                    let delta = end as i64 - now as i64;
                    if delta <= 0 {
                        if delta < -(self.frames as i64) {
                            late_blocks += 1;
                            if late_blocks > 8 {
                                return Err("Audio scheduling repeatedly missed its deadline; increase ASIO buffer".into());
                            }
                        } else {
                            late_blocks = 0;
                        }
                        break;
                    }
                    timer.sleep_ns(
                        (delta as u64 * 1_000_000_000 / self.config.sample_rate as u64)
                            .min(2_000_000),
                    )?;
                }
                let mut receive_latency = self.config.network_frames();
                for buffer in buffers.iter_mut().filter(|b| b.input) {
                    if let Some(latency) = receiver.channel_latency_samples(buffer.channel) {
                        receive_latency = receive_latency.max(latency);
                    }
                    if !receiver.get_samples(end as usize, buffer.channel, &mut buffer.data[index])
                    {
                        self.shared.xruns.fetch_add(1, Ordering::Relaxed);
                    }
                    rx_meters.record(buffer.channel, &buffer.data[index]);
                }
                let latency = (receive_latency + self.frames) as u64;
                if self.shared.input_latency.swap(latency, Ordering::AcqRel) != latency {
                    notify(self.callbacks, 6);
                }
                if !reset_sent && reset.load(Ordering::Acquire) {
                    reset_sent = true;
                    notify(self.callbacks, ASIO_RESET_REQUEST);
                }
                let stamp =
                    system_start + timer::performance_ns()?.saturating_sub(performance_start);
                self.shared.set_position(position, stamp);
                callback(
                    self.callbacks,
                    self.time_info,
                    index,
                    position,
                    stamp,
                    self.config.sample_rate,
                );
                for channel in &mut tx {
                    channel.fill(0);
                }
                for buffer in buffers.iter().filter(|b| !b.input) {
                    tx[buffer.channel].copy_from_slice(&buffer.data[index]);
                    tx_meters.record(buffer.channel, &buffer.data[index]);
                }
                transmitter.push_channels_at(end.wrapping_add(self.frames as u64) as usize, &tx);
                position = position.wrapping_add(self.frames as u64);
                index ^= 1;
            }
            Ok(())
        })();
        runtime.block_on(server.shutdown());
        let shutdown = runtime.block_on(ptp.shutdown());
        result.and(shutdown)
    }

    fn loopback(&self, buffers: &mut [ChannelBuffer], timer: &Timer) -> Result<(), String> {
        let mut previous = vec![vec![0; self.frames]; self.config.channels];
        let performance_start = timer::performance_ns()?;
        let system_start = timer::system_timestamp_ns();
        self.ready
            .send(Ok(()))
            .map_err(|_| "ASIO host cancelled startup")?;
        self.shared.started.store(true, Ordering::Release);
        let mut position = 0u64;
        let mut index = 0usize;
        while !self.stop.load(Ordering::Acquire) {
            let due = (position + self.frames as u64) as u128 * 1_000_000_000
                / self.config.sample_rate as u128;
            let elapsed = timer::performance_ns()?.saturating_sub(performance_start);
            timer.sleep_ns((due as u64).saturating_sub(elapsed))?;
            for buffer in buffers.iter_mut().filter(|b| b.input) {
                buffer.data[index].copy_from_slice(&previous[buffer.channel]);
            }
            let stamp = system_start + timer::performance_ns()?.saturating_sub(performance_start);
            self.shared.set_position(position, stamp);
            callback(
                self.callbacks,
                self.time_info,
                index,
                position,
                stamp,
                self.config.sample_rate,
            );
            for channel in &mut previous {
                channel.fill(0);
            }
            for buffer in buffers.iter().filter(|b| !b.input) {
                previous[buffer.channel].copy_from_slice(&buffer.data[index]);
            }
            position = position.wrapping_add(self.frames as u64);
            index ^= 1;
        }
        Ok(())
    }
}
