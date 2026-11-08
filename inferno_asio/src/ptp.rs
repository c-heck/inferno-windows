//! Slave-only PTPv1, default domain, software timestamps. Never adjusts Windows time.
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    os::windows::io::AsRawSocket,
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{oneshot, watch},
};
use usrvclock::ClockOverlay;

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);
const DOMAIN: &[u8; 16] = b"_DFLT\0\0\0\0\0\0\0\0\0\0\0";
const EXPIRE: Duration = Duration::from_secs(5);
const DELAY_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

fn delay_request(sync: &[u8; 124], uuid: [u8; 6], sequence: u16, origin: i64) -> [u8; 124] {
    let mut request = [0u8; 124];
    request[..20].copy_from_slice(&sync[..20]);
    request[20] = 1;
    request[21] = 1;
    request[22..28].copy_from_slice(&uuid);
    request[28..30].copy_from_slice(&1u16.to_be_bytes());
    request[30..32].copy_from_slice(&sequence.to_be_bytes());
    request[32] = 1;
    request[35] = 8; // Follow-up capable, not a boundary clock or external timing source.
    request[40..44].copy_from_slice(&((origin / 1_000_000_000) as u32).to_be_bytes());
    request[44..48].copy_from_slice(&((origin % 1_000_000_000) as u32).to_be_bytes());
    request[48..72].copy_from_slice(&sync[48..72]);
    request[74..80].copy_from_slice(&sync[74..80]);
    request[82..84].copy_from_slice(&sync[82..84]);
    request[86..88].copy_from_slice(&i16::MAX.to_be_bytes());
    let steps = u16_at(sync, 90).saturating_add(1);
    request[90..92].copy_from_slice(&steps.to_be_bytes());
    request[95] = 255; // Slave-only clock stratum.
    request[96..100].copy_from_slice(b"DFLT");
    request[101..108].copy_from_slice(&sync[21..28]);
    request[110..112].copy_from_slice(&sync[28..30]);
    request[123] = sync[123];
    request
}

/// Publishes the leader clock ID where inferno_aoip answers Dante Controller clock-status queries.
struct ClockStats {
    path: std::path::PathBuf,
    leader: Option<[u8; 6]>,
}

impl ClockStats {
    fn new(uuid: [u8; 6]) -> Self {
        let name = format!("clock-stats.{}0000", hex(&uuid));
        Self {
            path: std::env::temp_dir().join(name),
            leader: None,
        }
    }

    fn publish(&mut self, leader: [u8; 6]) {
        if self.leader == Some(leader) {
            return;
        }
        let id = [
            leader[0], leader[1], leader[2], 0xff, 0xfe, leader[3], leader[4], leader[5],
        ];
        match std::fs::write(&self.path, hex(&id)) {
            Ok(()) => self.leader = Some(leader),
            Err(e) => log::warn!("PTP clock status publish failed: {e}"),
        }
    }

    fn clear(&mut self) {
        if self.leader.take().is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for ClockStats {
    fn drop(&mut self) {
        self.clear();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[link(name = "ws2_32")]
extern "system" {
    fn setsockopt(socket: usize, level: i32, name: i32, value: *const u8, len: i32) -> i32;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Source {
    uuid: [u8; 6],
    port: u16,
}

#[derive(Clone, Copy)]
struct Header {
    source: Source,
    sequence: u16,
    control: u8,
    two_step: bool,
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}
fn timestamp(data: &[u8], offset: usize) -> Option<i64> {
    let seconds = u32::from_be_bytes(data.get(offset..offset + 4)?.try_into().ok()?) as i64;
    let nanos = u32::from_be_bytes(data.get(offset + 4..offset + 8)?.try_into().ok()?) as i64;
    (nanos < 1_000_000_000).then_some(seconds * 1_000_000_000 + nanos)
}
fn header(data: &[u8]) -> Option<Header> {
    if data.len() < 40
        || u16_at(data, 0) != 1
        || u16_at(data, 2) != 1
        || &data[4..20] != DOMAIN
        || data[21] != 1
    {
        return None;
    }
    let control = data[32];
    if data[20] != if control <= 1 { 1 } else { 2 } {
        return None;
    }
    Some(Header {
        source: Source {
            uuid: data[22..28].try_into().ok()?,
            port: u16_at(data, 28),
        },
        sequence: u16_at(data, 30),
        control,
        two_step: data[35] & 8 != 0,
    })
}

pub fn now_ns() -> i64 {
    ClockOverlay {
        clock_id: 0,
        last_sync: 0,
        shift: 0,
        freq_scale: 0.0,
    }
    .now_underlying_ns()
}

fn socket(interface: Ipv4Addr, port: u16) -> Result<UdpSocket, String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| format!("PTP socket: {e}"))?;
    let exclusive = 1i32;
    if unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            0xffff,
            -5,
            (&exclusive as *const i32).cast(),
            4,
        )
    } != 0
    {
        return Err(format!(
            "PTP exclusive socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    socket
        .bind(&std::net::SocketAddrV4::new(
            if port == 0 { interface } else { Ipv4Addr::UNSPECIFIED },
            port,
        ).into())
        .map_err(|e| {
            format!("PTP port {port} unavailable (another PTP service may own it): {e}")
        })?;
    if port != 0 {
        socket
            .join_multicast_v4(&GROUP, &interface)
            .map_err(|e| format!("Join PTP group: {e}"))?;
    }
    socket
        .set_multicast_if_v4(&interface)
        .map_err(|e| format!("PTP interface: {e}"))?;
    socket
        .set_multicast_ttl_v4(1)
        .map_err(|e| format!("PTP TTL: {e}"))?;
    socket
        .set_multicast_loop_v4(false)
        .map_err(|e| format!("PTP loopback: {e}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| format!("PTP nonblocking: {e}"))?;
    UdpSocket::from_std(socket.into()).map_err(|e| format!("PTP Tokio socket: {e}"))
}

#[derive(Default)]
struct Servo {
    previous: Option<(i64, f64)>,
    shift: f64,
    frequency: f64,
    good: usize,
}

impl Servo {
    fn update(
        &mut self,
        local_ns: i64,
        offset: f64,
        path_ns: f64,
        source: Source,
    ) -> Option<ClockOverlay> {
        if !(0.0..=20_000_000.0).contains(&path_ns) || !offset.is_finite() {
            log::warn!("PTP rejected measurement: offset={offset} ns path={path_ns} ns");
            return None;
        }
        let desired_shift = -offset;
        if let Some((previous_time, previous_offset)) = self.previous {
            let dt = (local_ns - previous_time) as f64;
            if dt < 100_000_000.0 {
                log::warn!("PTP rejected measurement: interval={dt} ns");
                return None;
            }
            if (offset - previous_offset).abs() > 5_000_000.0 {
                log::warn!(
                    "PTP rejected discontinuous offset sample: previous={previous_offset} ns current={offset} ns path={path_ns} ns"
                );
                return None;
            }
            let rate = (-(offset - previous_offset) / dt).clamp(-0.0005, 0.0005);
            self.frequency = self.frequency * 0.9 + rate * 0.1;
            let predicted = self.shift + dt * self.frequency;
            self.shift = predicted + (desired_shift - predicted) * 0.25;
        } else {
            self.shift = desired_shift;
        }
        self.previous = Some((local_ns, offset));
        self.good += 1;
        if self.good < 3 {
            return None;
        }
        let mut identity = [0; 8];
        identity[2..].copy_from_slice(&source.uuid);
        Some(ClockOverlay {
            clock_id: i64::from_be_bytes(identity),
            last_sync: local_ns,
            shift: self.shift.round() as i64,
            freq_scale: self.frequency,
        })
    }
}

struct Exchange {
    sequence: u16,
    sent: i64,
    forward: i64,
    sync_sequence: u16,
    sync_origin: i64,
    sync_received: i64,
    created: Instant,
}
struct Forward {
    sequence: u16,
    origin: i64,
    received: i64,
    created: Instant,
}
struct Sync {
    source: Source,
    sequence: u16,
    received: i64,
    created: Instant,
}

pub struct Session {
    pub updates: watch::Receiver<Option<ClockOverlay>>,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

impl Session {
    pub fn start(interface: Ipv4Addr, uuid: [u8; 6]) -> Result<Self, String> {
        Self::start_to(interface, uuid, SocketAddrV4::new(GROUP, 319))
    }

    fn start_to(
        interface: Ipv4Addr,
        uuid: [u8; 6],
        destination: SocketAddrV4,
    ) -> Result<Self, String> {
        let event = socket(interface, 319)?;
        let general = socket(interface, 320)?;
        let requests = socket(interface, 0)?;
        let (tx, updates) = watch::channel(None);
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = run(event, general, requests, uuid, destination, tx.clone(), stop_rx).await;
            tx.send_replace(None);
            if let Err(e) = &result {
                crate::timer::report(e);
            }
            result
        });
        Ok(Self {
            updates,
            stop,
            task,
        })
    }

    pub async fn shutdown(self) -> Result<(), String> {
        let _ = self.stop.send(());
        self.task.await.map_err(|e| format!("PTP thread: {e}"))?
    }
}

async fn run(
    event: UdpSocket,
    general: UdpSocket,
    requests: UdpSocket,
    uuid: [u8; 6],
    destination: SocketAddrV4,
    updates: watch::Sender<Option<ClockOverlay>>,
    mut stop: oneshot::Receiver<()>,
) -> Result<(), String> {
    let mut event_data = [0u8; 2048];
    let mut general_data = [0u8; 2048];
    let mut master: Option<(Source, Ipv4Addr, Instant)> = None;
    let mut pending: Option<Sync> = None;
    let mut early_follow: Option<(Source, u16, i64, Instant)> = None;
    let mut exchange: Option<Exchange> = None;
    let mut last_forward: Option<Forward> = None;
    let mut leader_sync = [0u8; 124];
    let mut last_request: Option<Instant> = None;
    let mut sequence = 0u16;
    let mut servo = Servo::default();
    let mut clock_stats = ClockStats::new(uuid);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        let (data, received, sender) = tokio::select! {
            _ = &mut stop => return Ok(()),
            _ = tick.tick() => {
                let stale = updates.borrow().is_some_and(|clock| {
                    now_ns().saturating_sub(clock.last_sync) >= 3_000_000_000
                });
                if stale {
                    clock_stats.clear();
                    servo = Servo::default();
                    updates.send_replace(None);
                    crate::timer::report("PTP accepted measurements timed out; audio must stop");
                }
                if master.is_some_and(|(_, _, seen)| seen.elapsed() > EXPIRE) {
                    master = None;
                    clock_stats.clear();
                    pending = None;
                    early_follow = None;
                    exchange = None;
                    last_forward = None;
                    servo = Servo::default();
                    updates.send_replace(None);
                    crate::timer::report("PTP leader timed out; audio must stop");
                }
                continue;
            },
            packet = event.recv_from(&mut event_data) => {
                let (length, sender) = packet.map_err(|e| format!("PTP event receive: {e}"))?;
                (&event_data[..length], now_ns(), sender)
            },
            packet = general.recv_from(&mut general_data) => {
                let (length, sender) = packet.map_err(|e| format!("PTP general receive: {e}"))?;
                (&general_data[..length], now_ns(), sender)
            },
        };
        let Some(h) = header(data) else {
            log::debug!(
                "PTP rejected header ({} bytes): {:02x?}",
                data.len(),
                &data[..data.len().min(64)]
            );
            continue;
        };
        log::debug!(
            "PTP received control={} sequence={} length={} source={:?}",
            h.control,
            h.sequence,
            data.len(),
            h.source
        );
        if h.source.uuid == uuid {
            continue;
        }
        let std::net::SocketAddr::V4(sender) = sender else {
            continue;
        };
        if master.is_some_and(|(source, address, _)| source != h.source || address != *sender.ip())
        {
            continue;
        }
        if h.control == 0 && data.len() >= 124 {
            leader_sync.copy_from_slice(&data[..124]);
            master = Some((h.source, *sender.ip(), Instant::now()));
            let origin = if h.two_step {
                pending = Some(Sync {
                    source: h.source,
                    sequence: h.sequence,
                    received,
                    created: Instant::now(),
                });
                early_follow.take().and_then(|(source, seq, origin, seen)| {
                    (source == h.source
                        && seq == h.sequence
                        && seen.elapsed() < Duration::from_secs(1))
                    .then_some(origin)
                })
            } else {
                timestamp(data, 40)
            };
            if let Some(origin) = origin {
                last_forward = Some(Forward {
                    sequence: h.sequence,
                    origin,
                    received,
                    created: Instant::now(),
                });
                pending = None;
            }
        } else if h.control == 2 && data.len() >= 52 {
            let Some(origin) = timestamp(data, 44) else {
                continue;
            };
            let seq = u16_at(data, 42);
            if let Some(sync) = pending.take() {
                if sync.source == h.source
                    && sync.sequence == seq
                    && sync.created.elapsed() < Duration::from_secs(1)
                {
                    last_forward = Some(Forward {
                        sequence: sync.sequence,
                        origin,
                        received: sync.received,
                        created: Instant::now(),
                    });
                } else {
                    pending = Some(sync);
                    early_follow = Some((h.source, seq, origin, Instant::now()));
                }
            } else {
                early_follow = Some((h.source, seq, origin, Instant::now()));
            }
        } else if h.control == 3 && data.len() >= 60 {
            if !master.is_some_and(|(source, _, _)| source == h.source)
                || data[49] != 1
                || data[50..56] != uuid
                || u16_at(data, 56) != 1
            {
                continue;
            }
            if let Some(request) = exchange.take() {
                if u16_at(data, 58) != request.sequence
                    || request.created.elapsed() > Duration::from_secs(2)
                {
                    exchange = Some(request);
                    continue;
                }
                if let Some(master_received) = timestamp(data, 40) {
                    let backward = master_received - request.sent;
                    let offset = (request.forward as f64 - backward as f64) / 2.0;
                    let path = (request.forward as f64 + backward as f64) / 2.0;
                    log::debug!(
                        "PTP exchange sequence={} sync_sequence={} sync_origin_ns={} sync_receive_ns={} delayreq_send_ns={} delayresp_local_receive_ns={} delayresp_master_receive_ns={} sync_forward_ns={} delay_backward_ns={} offset_ns={offset} path_ns={path}",
                        request.sequence,
                        request.sync_sequence,
                        request.sync_origin,
                        request.sync_received,
                        request.sent,
                        received,
                        master_received,
                        request.forward,
                        backward,
                    );
                    let overlay = servo.update(received, offset, path, h.source);
                    if overlay.is_some() || servo.good < 3 {
                        if overlay.is_some() {
                            clock_stats.publish(h.source.uuid);
                        } else {
                            clock_stats.clear();
                        }
                        updates.send_replace(overlay);
                    }
                }
            }
        }
        if let Some(sync) = last_forward.take() {
            if sync.created.elapsed() > Duration::from_secs(1) {
                continue;
            }
            if exchange
                .as_ref()
                .is_some_and(|e| e.created.elapsed() < Duration::from_secs(2))
            {
                continue;
            }
            if last_request.is_some_and(|sent| sent.elapsed() < DELAY_REQUEST_INTERVAL) {
                continue;
            }
            sequence = sequence.wrapping_add(1);
            let sent = now_ns();
            // The wire timestamp uses the learned leader epoch; measurements retain local time.
            let request = delay_request(
                &leader_sync,
                uuid,
                sequence,
                sent - (sync.received - sync.origin),
            );
            let target = destination;
            let request_socket = if target.ip().is_multicast() { &requests } else { &event };
            request_socket
                .send_to(&request, target)
                .await
                .map_err(|e| format!("PTP DelayReq send: {e}"))?;
            log::debug!("PTP sent DelayReq sequence={sequence} destination={target}");
            last_request = Some(Instant::now());
            exchange = Some(Exchange {
                sequence,
                sent,
                forward: sync.received - sync.origin,
                sync_sequence: sync.sequence,
                sync_origin: sync.origin,
                sync_received: sync.received,
                created: Instant::now(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires a real PTPv1 leader and exclusive UDP 319/320 access"]
    fn live_network_clock_lock() {
        struct DiagnosticLogger;
        impl log::Log for DiagnosticLogger {
            fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
                metadata.target().contains("::ptp")
            }
            fn log(&self, record: &log::Record<'_>) {
                if self.enabled(record.metadata()) {
                    eprintln!("{}", record.args());
                }
            }
            fn flush(&self) {}
        }
        static LOGGER: DiagnosticLogger = DiagnosticLogger;
        log::set_logger(&LOGGER).expect("Install diagnostic logger");
        log::set_max_level(log::LevelFilter::Debug);
        let stability_seconds: u64 = match std::env::var("INFERNO_PTP_TEST_SECONDS") {
            Ok(value) => value
                .parse()
                .expect("INFERNO_PTP_TEST_SECONDS must be a positive integer"),
            Err(std::env::VarError::NotPresent) => 30,
            Err(error) => panic!("Read diagnostic stability duration: {error}"),
        };
        assert!(
            stability_seconds > 0,
            "INFERNO_PTP_TEST_SECONDS must be greater than zero"
        );
        let config = crate::config::Config::from_env().expect("Load network configuration");
        assert!(!config.loopback, "Select network mode for this diagnostic");
        let interface = config.bind_ip.expect("Select an Ethernet IPv4 address");
        let adapter = netdev::get_interfaces()
            .into_iter()
            .find(|adapter| {
                adapter
                    .ipv4
                    .iter()
                    .any(|address| address.addr() == interface)
            })
            .expect("Find selected Ethernet adapter");
        let uuid = adapter.mac_addr.expect("Ethernet MAC address").octets();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let destination = match std::env::var("INFERNO_PTP_TEST_DESTINATION") {
                Ok(value) => value.parse().expect("Invalid diagnostic PTP destination"),
                Err(std::env::VarError::NotPresent) => SocketAddrV4::new(GROUP, 319),
                Err(error) => panic!("Read diagnostic PTP destination: {error}"),
            };
            let mut session =
                Session::start_to(interface, uuid, destination).expect("Start live PTP session");
            let locked = tokio::time::timeout(Duration::from_secs(18), async {
                loop {
                    if session.updates.borrow().is_some() {
                        break;
                    }
                    session.updates.changed().await.expect("PTP worker stopped");
                }
            })
            .await;
            let mut failure = locked
                .err()
                .map(|_| format!("No PTPv1 clock lock within 18 seconds on {interface}"));
            if failure.is_none() {
                println!("PTPv1 clock locked on {interface}; checking stability for {stability_seconds} seconds.");
                for _ in 0..stability_seconds {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let overlay = *session.updates.borrow();
                    if !overlay.is_some_and(|clock| {
                        now_ns().saturating_sub(clock.last_sync) < 3_000_000_000
                    }) {
                        failure = Some("PTPv1 lock lost or measurements became stale".into());
                        break;
                    }
                }
            }
            session.shutdown().await.expect("Stop live PTP session");
            assert!(failure.is_none(), "{}", failure.unwrap());
            println!("PTPv1 lock remained current for {stability_seconds} seconds; no audio was transmitted.");
        });
    }

    #[test]
    fn validates_v1_wire_header_and_timestamps() {
        let mut packet = [0u8; 124];
        packet[0..2].copy_from_slice(&1u16.to_be_bytes());
        packet[2..4].copy_from_slice(&1u16.to_be_bytes());
        packet[4..20].copy_from_slice(DOMAIN);
        packet[20] = 1;
        packet[21] = 1;
        packet[35] = 8;
        packet[30..32].copy_from_slice(&65535u16.to_be_bytes());
        let h = header(&packet).unwrap();
        assert_eq!(h.sequence, 65535);
        assert!(h.two_step);
        packet[40..44].copy_from_slice(&123u32.to_be_bytes());
        packet[44..48].copy_from_slice(&456u32.to_be_bytes());
        assert_eq!(timestamp(&packet, 40), Some(123_000_000_456));
        packet[44..48].copy_from_slice(&1_000_000_000u32.to_be_bytes());
        assert_eq!(timestamp(&packet, 40), None);
        assert!(header(&packet[..39]).is_none());
        packet[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(header(&packet).is_none());
    }

    #[test]
    fn clock_stats_publishes_leader_eui64_and_clears() {
        let uuid = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let mut stats = ClockStats::new(uuid);
        stats.publish([0x00, 0x1d, 0xc1, 0x11, 0x22, 0x33]);
        assert_eq!(
            std::fs::read_to_string(&stats.path).unwrap(),
            "001dc1fffe112233"
        );
        stats.clear();
        assert!(!stats.path.exists());
    }

    #[test]
    fn delay_request_advertises_slave_and_learned_grandmaster() {
        let mut sync = [0u8; 124];
        sync[..2].copy_from_slice(&1u16.to_be_bytes());
        sync[2..4].copy_from_slice(&1u16.to_be_bytes());
        sync[4..20].copy_from_slice(DOMAIN);
        sync[21] = 1;
        sync[22..28].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        sync[28..30].copy_from_slice(&3u16.to_be_bytes());
        for (index, byte) in sync[48..80].iter_mut().enumerate() {
            *byte = index as u8 + 1;
        }
        sync[72..76].copy_from_slice(&[0xff, 0xff, 0xf0, 0x60]);
        sync[82..84].copy_from_slice(&(-2i16).to_be_bytes());
        sync[90..92].copy_from_slice(&4u16.to_be_bytes());
        sync[123] = 1;
        let uuid = [2, 8, 9, 10, 11, 12];
        let request = delay_request(&sync, uuid, 42, 123_456_789_012);
        let h = header(&request).unwrap();
        assert_eq!(h.source.uuid, uuid);
        assert_eq!(h.sequence, 42);
        assert_eq!(h.control, 1);
        assert_eq!(request[35], 8);
        assert_eq!(timestamp(&request, 40), Some(123_456_789_012));
        assert_eq!(&request[48..72], &sync[48..72]);
        assert_eq!(&request[72..76], &[0, 0, 0xf0, 0x60]);
        assert_eq!(&request[76..80], &sync[76..80]);
        assert_eq!(u16_at(&request, 90), 5);
        assert_eq!(request[95], 255);
        assert_eq!(&request[101..108], &sync[21..28]);
        assert_eq!(u16_at(&request, 110), 3);
        assert_eq!(&request[82..84], &(-2i16).to_be_bytes());
    }

    #[test]
    fn servo_sign_lock_and_path_validation() {
        let mut servo = Servo::default();
        let source = Source {
            uuid: [1; 6],
            port: 1,
        };
        assert!(servo
            .update(1_000_000_000, 10_000_000.0, 100_000.0, source)
            .is_none());
        assert!(servo
            .update(2_000_000_000, 10_000_000.0, 100_000.0, source)
            .is_none());
        let overlay = servo
            .update(3_000_000_000, 10_000_000.0, 100_000.0, source)
            .unwrap();
        assert_eq!(
            overlay.underlying_to_overlay_ns(3_000_000_000),
            2_990_000_000
        );
        assert!(servo
            .update(4_000_000_000, 10_000_000.0, -1.0, source)
            .is_none());
        assert_eq!(servo.good, 3, "invalid path must preserve the servo state");
        assert!(servo
            .update(4_000_000_000, 30_000_000.0, 100_000.0, source)
            .is_none());
        assert_eq!(servo.good, 3, "an isolated offset jump must not reset lock");
        let recovered = servo
            .update(5_000_000_000, 10_100_000.0, 100_000.0, source)
            .unwrap();
        assert_eq!(recovered.last_sync, 5_000_000_000);
    }

    fn test_packet(control: u8, uuid: [u8; 6], sequence: u16, length: usize) -> Vec<u8> {
        let mut data = vec![0; length];
        data[0..2].copy_from_slice(&1u16.to_be_bytes());
        data[2..4].copy_from_slice(&1u16.to_be_bytes());
        data[4..20].copy_from_slice(DOMAIN);
        data[20] = if control <= 1 { 1 } else { 2 };
        data[21] = 1;
        data[22..28].copy_from_slice(&uuid);
        data[28..30].copy_from_slice(&1u16.to_be_bytes());
        data[30..32].copy_from_slice(&sequence.to_be_bytes());
        data[32] = control;
        data
    }

    fn put_time(data: &mut [u8], offset: usize, ns: i64) {
        data[offset..offset + 4].copy_from_slice(&((ns / 1_000_000_000) as u32).to_be_bytes());
        data[offset + 4..offset + 8].copy_from_slice(&((ns % 1_000_000_000) as u32).to_be_bytes());
    }

    #[tokio::test]
    async fn synthetic_two_step_leader_locks_and_expires_without_external_network() {
        let event = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let general = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let requests = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let leader = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let leader_events = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let event_address = event.local_addr().unwrap();
        let general_address = general.local_addr().unwrap();
        let std::net::SocketAddr::V4(leader_address) = leader.local_addr().unwrap() else {
            panic!("IPv4 expected");
        };
        let destination = leader_address;
        let slave_id = [2, 0, 0, 0, 0, 1];
        let leader_id = [2, 0, 0, 0, 0, 2];
        let (tx, mut updates) = watch::channel(None);
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run(event, general, requests, slave_id, destination, tx, stop_rx));
        for sequence in 1..=5u16 {
            let mut sync = test_packet(0, leader_id, sequence, 124);
            sync[35] = 8;
            let origin = now_ns() + 20_000_000;
            leader_events.send_to(&sync, event_address).await.unwrap();
            let mut follow = test_packet(2, leader_id, sequence, 52);
            follow[42..44].copy_from_slice(&sequence.to_be_bytes());
            put_time(&mut follow, 44, origin);
            leader_events
                .send_to(&follow, general_address)
                .await
                .unwrap();
            let mut request = [0; 256];
            let (n, _) =
                tokio::time::timeout(Duration::from_secs(1), leader.recv_from(&mut request))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(n, 124);
            assert_eq!(header(&request[..n]).unwrap().control, 1);
            let mut response = test_packet(3, leader_id, sequence, 60);
            let timestamp = now_ns() + 20_000_000 + if sequence == 4 { 50_000_000 } else { 0 };
            put_time(&mut response, 40, timestamp);
            response[49] = 1;
            response[50..56].copy_from_slice(&slave_id);
            response[56..58].copy_from_slice(&1u16.to_be_bytes());
            response[58..60].copy_from_slice(&request[30..32]);
            leader.send_to(&response, general_address).await.unwrap();
            tokio::time::sleep(Duration::from_millis(1100)).await;
            if sequence == 3 {
                assert!(updates.borrow().is_some(), "three valid samples must lock");
            } else if sequence == 4 {
                assert!(
                    updates.borrow().is_some(),
                    "one outlier must not clear lock"
                );
            }
        }
        let overlay =
            (*updates.borrow()).expect("synthetic leader must lock after three measurements");
        assert!((overlay.shift - 20_000_000).abs() < 3_000_000);
        assert!(overlay.freq_scale.abs() <= 0.0005);
        assert!(
            now_ns().saturating_sub(overlay.last_sync) < 2_000_000_000,
            "a valid sample after the outlier must refresh synchronization"
        );
        for sequence in 6..=9 {
            let mut sync = test_packet(0, leader_id, sequence, 124);
            put_time(&mut sync, 40, now_ns() + 20_000_000);
            leader_events.send_to(&sync, event_address).await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(
            updates.borrow().is_none(),
            "Sync alone must not keep the clock valid without accepted delay measurements"
        );
        tokio::time::timeout(Duration::from_secs(7), async {
            while updates.borrow().is_some() {
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("leader loss must invalidate the clock, not keep free-running");
        stop.send(()).unwrap();
        assert!(task.await.unwrap().is_ok());
    }
}
