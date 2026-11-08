use crate::byte_utils::*;
use super::channels_subscriber::ChannelsSubscriber;
use crate::common::*;
use crate::net_utils::UdpSocketWrapper;
use crate::protocol::mcast::{make_packet, MulticastMessage};
use crate::media_clock::MediaClock;
use crate::{byte_utils::write_str_to_buffer, device_info::DeviceInfo};
use bytebuffer::ByteBuffer;
use std::sync::atomic::Ordering;
use std::sync::RwLock;
use std::{
  net::{IpAddr, Ipv4Addr, SocketAddr},
  sync::Arc,
  time::Duration,
};
use tokio::sync::watch;
use tokio::time::interval;
use tokio::{
  select,
  sync::{broadcast::Receiver as BroadcastReceiver, mpsc},
  time::MissedTickBehavior,
};

const SEND_BUFFER_SIZE: usize = 1500;
const DST_PORT_HEARTBEAT: u16 = 8708;
const DST_PORT_DEVICE_INFO: u16 = 8702;

pub type PeaksCallback = Box<dyn FnMut() -> (Vec<u8>, Vec<u8>) + Send + Sync>;

struct Multicaster<'s> {
  self_info: &'s DeviceInfo,
  pub server: UdpSocketWrapper,
  seqnum: u16,
  vendor: [u8; 8],
  firmware_version_bytes: [u8; 4],
  product_version_bytes: [u8; 4],
  device_info_destination: SocketAddr,
  heartbeat_destination: SocketAddr,
  send_buffer: [u8; SEND_BUFFER_SIZE],
  clock: Arc<RwLock<MediaClock>>,
  channels_subscriber: Option<Arc<ChannelsSubscriber>>,
  get_peaks: PeaksCallback,
  // Each block group has its own seqnum counter, matching the Shure MXWANI8 pattern.
  util_block_seqnum: u16,    // for 0x8000+0x8001 packets
  latency_block_seqnum: u16, // for 0x8003+0x8004 packets
  last_traffic: Option<(std::time::Instant, u64, u64)>,
}

impl<'s> Multicaster<'s> {
  pub fn new(
    self_info: &'s DeviceInfo,
    server: UdpSocketWrapper,
    clock: Arc<RwLock<MediaClock>>,
    get_peaks: PeaksCallback,
  ) -> Multicaster {
    let patch_version = env!("CARGO_PKG_VERSION_PATCH").parse::<u16>().unwrap();
    let mut r = Multicaster {
      self_info,
      server,
      seqnum: 1,
      vendor: [32; 8],
      firmware_version_bytes: [
        env!("CARGO_PKG_VERSION_MAJOR").parse::<u8>().unwrap(),
        env!("CARGO_PKG_VERSION_MINOR").parse::<u8>().unwrap(),
        H(patch_version),
        L(patch_version),
      ],
      product_version_bytes: [
        env!("CARGO_PKG_VERSION_MAJOR").parse::<u8>().unwrap(),
        env!("CARGO_PKG_VERSION_MINOR").parse::<u8>().unwrap(),
        H(patch_version),
        L(patch_version),
      ],
      device_info_destination: SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 231)),
        DST_PORT_DEVICE_INFO,
      ),
      heartbeat_destination: SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 233)),
        DST_PORT_HEARTBEAT,
      ),
      send_buffer: [0; SEND_BUFFER_SIZE],
      clock,
      channels_subscriber: None,
      get_peaks,
      util_block_seqnum: 1,
      latency_block_seqnum: 1,
      last_traffic: None,
    };
    write_str_to_buffer(&mut r.vendor, 0, 8, &self_info.vendor_string);
    return r;
  }

  pub fn should_work(&self) -> bool {
    return self.server.should_work();
  }

  async fn send(&mut self, dst: SocketAddr, start_code: u16, opcode: [u8; 8], content: &[u8]) {
    let pkt = make_packet(
      &mut self.send_buffer,
      start_code,
      self.seqnum,
      self.self_info.process_id,
      self.self_info.factory_device_id,
      self.vendor,
      opcode,
      content,
    );
    self.seqnum = self.seqnum.wrapping_add(1);
    match pkt {
      Ok(pkt) => self.server.send(&dst, pkt).await,
      Err(e) => tracing::warn!("Failed to build info multicast packet: {}", e),
    }
  }

  async fn send_board_info(&mut self) {
    let mut content = [0u8; 200];
    // Firmware version:
    content[0..4].copy_from_slice(&[4, 1, 0, 6]);
    content[0x23] = 2;
    // Hardware version:
    content[4..8].copy_from_slice(&[4, 1, 0, 3]);
    content[0x27] = 1;
    // Boot version:
    content[0x28..0x2c].copy_from_slice(&[1, 0, 0, 0]);

    // flags of supported features:
    // 0x14: AES67, Device Lock
    //       0x01 - ??? (was 1, od 0xd)
    //       0x04 - supports AES67
    //       0x08 - is lockable
    // 0x15: ??? (was 0x50)
    // 0x16:
    //       0x10 - has Manufacturer name
    //       0x40 - Network is configurable (supports static addressing)
    // 0x17: Identify device, Sample rate & encoding configuration, Reboot, Factory reset (was 0xdb)
    content[0x14] = 0;
    content[0x15] = 0;
    content[0x16] = 0x10;
    content[0x17] = 0;

    content[0xbb] = 0x1f; // if 0, device is flooded with info multicast requests around 1 per second
    content[0xbf] = 5;
    content[0xc3] = 3;
    content[0xc7] = 3;
    write_str_to_buffer(&mut content, 12, 8, &self.self_info.board_name);
    write_str_to_buffer(&mut content, 0x38, 16, &self.self_info.board_name);

    self.send(self.device_info_destination, 0xffff, [0x07, 0x2a, 0x00, 0x60, 0, 0, 0, 0], &content).await;
  }

  async fn send_product_info(&mut self) {
    let mut content = [0; 336];
    write_str_to_buffer(&mut content, 0, 8, &self.self_info.manufacturer);
    write_str_to_buffer(&mut content, 8, 8, &self.self_info.board_name);
    write_str_to_buffer(&mut content, 0x2c, 16, &self.self_info.manufacturer);
    write_str_to_buffer(&mut content, 0xac, 16, &self.self_info.model_name);
    // product version:
    content[0x12c..0x130].copy_from_slice(&self.product_version_bytes);

    // firmware version:
    content[0x1c..0x20].copy_from_slice(&self.product_version_bytes);

    // 0x18..0x1b - software version
    // 0x24..0x26 - software patch version, u32
    // 0x28..0x2b - firmware patch version, u32

    self.send(self.device_info_destination, 0xffff, [0x07, 0x2a, 0x00, 0xc0, 0, 0, 0, 0], &content).await;
  }

  fn get_freq_offset_ppb(&self) -> Option<i32> {
    self
      .clock
      .read()
      .unwrap()
      .get_overlay()
      .as_ref()
      .map(|clkovl| {
        let freq_offset_f = (clkovl.freq_scale_including_hw() * 1_000_000_000f64).round();
        if i32::MIN as f64 <= freq_offset_f && freq_offset_f <= i32::MAX as f64 {
          Some(freq_offset_f as i32)
        } else {
          None
        }
      })
      .flatten()
  }

  /// Bytes per second since the previous heartbeat, and the cumulative receive error count.
  fn traffic_rates(&mut self) -> (u32, u32, u32) {
    let now = std::time::Instant::now();
    let (tx, rx, errors) = super::traffic::snapshot();
    let (last_time, last_tx, last_rx) = self.last_traffic.replace((now, tx, rx)).unwrap_or((now, tx, rx));
    let elapsed = now.duration_since(last_time).as_secs_f64();
    let rate = |delta: u64| if elapsed > 0.0 { (delta as f64 / elapsed).min(u32::MAX as f64) as u32 } else { 0 };
    (rate(tx.wrapping_sub(last_tx)), rate(rx.wrapping_sub(last_rx)), errors.min(u32::MAX as u64) as u32)
  }

  async fn send_heartbeat(&mut self) {
    let freq_offset_opt = self.get_freq_offset_ppb();

    if let Some(freq_offset) = freq_offset_opt {
      // Packet 1: [0x8000 (utilization) + 0x8001 (clock offset)]
      // Packet 2: [0x8003 (rx latency) + 0x8004 (missed packets)]
      // Shure MXWANI8 sends both every second as two separate UDP datagrams.
      // Each group has its own block seqnum counter (independent of the packet seqnum).
      {
        let ctr = self.util_block_seqnum;
        let mut bytes = ByteBuffer::new();
        bytes.set_endian(bytebuffer::Endian::BigEndian);

        bytes.write_u16(36);
        bytes.write_u16(0x8000);
        bytes.write_u16(4);
        bytes.write_u16(4);
        bytes.write_u16(ctr);
        bytes.write_u16(0);
        bytes.write_u16(0x0010); // constant sub-header from Shure capture
        bytes.write_u16(0x0000);
        bytes.write_u16(0x0001);
        bytes.write_u16(0x0010);
        let (tx_rate, rx_rate, rx_errors) = self.traffic_rates();
        bytes.write_u32(tx_rate); // TX bytes/sec
        bytes.write_u32(rx_rate); // RX bytes/sec
        bytes.write_u32(0); // TX errors
        bytes.write_u32(rx_errors); // RX errors (missing audio packets, cumulative)

        // 0x8002 signal presence: one level byte per channel, Dante scale -2 × dBFS.
        let (rx_levels, tx_levels) = (self.get_peaks)();
        if !rx_levels.is_empty() || !tx_levels.is_empty() {
          let levels = tx_levels.len() + rx_levels.len();
          let record_len = 24 + levels.div_ceil(4) * 4;
          bytes.write_u16(record_len as u16);
          bytes.write_u16(0x8002);
          bytes.write_u16(4);
          bytes.write_u16(record_len as u16 - 12);
          bytes.write_u16(ctr);
          bytes.write_u16(0);
          bytes.write_u16(tx_levels.len() as u16);
          bytes.write_u16(0);
          bytes.write_u16(rx_levels.len() as u16);
          bytes.write_u16(0);
          bytes.write_u16(24);
          bytes.write_u16(0);
          bytes.write_bytes(&tx_levels);
          bytes.write_bytes(&rx_levels);
          for _ in levels..record_len - 24 {
            bytes.write_u8(0);
          }
        }

        bytes.write_u16(16);
        bytes.write_u16(0x8001);
        bytes.write_u16(4);
        bytes.write_u16(4);
        bytes.write_u16(ctr);
        bytes.write_u16(0);
        bytes.write_i32(freq_offset);

        self.util_block_seqnum = self.util_block_seqnum.wrapping_add(1);
        let content = bytes.as_bytes().to_vec();
        self.send(self.heartbeat_destination, 0xfffe, [0, 8, 0, 1, 0x10, 0, 0, 0], &content).await;
      }

      if let Some(chsub) = self.channels_subscriber.as_ref() {
        let ctr = self.latency_block_seqnum;

        let content = {
          let mut bytes = ByteBuffer::new();
          bytes.set_endian(bytebuffer::Endian::BigEndian);

          let flows_info = chsub.flows_info();
          let flows_info = flows_info.read().unwrap();
          let flows_count = flows_info.len() as u16;

          bytes.write_u16(24 + flows_count * 4);
          bytes.write_u16(0x8003);
          bytes.write_u16(4);
          bytes.write_u16(12 + flows_count * 4);
          bytes.write_u16(ctr);
          bytes.write_u16(0);
          bytes.write_u16(flows_count);
          bytes.write_u16(0);
          bytes.write_u16(24);
          bytes.write_u16(0);
          bytes.write_u32(self.self_info.sample_rate);
          for opt in flows_info.iter() {
            let latency =
              opt.as_ref().map(|fi| fi.actual_latency_samples.swap(0, Ordering::Relaxed)).unwrap_or(0);
            bytes.write_u32(latency.clamp(0, i32::MAX) as u32);
          }

          bytes.write_u16(20 + flows_count * 4);
          bytes.write_u16(0x8004);
          bytes.write_u16(4);
          bytes.write_u16(8 + flows_count * 4);
          bytes.write_u16(ctr);
          bytes.write_u16(0);
          bytes.write_u16(flows_count);
          bytes.write_u16(0);
          bytes.write_u16(20);
          bytes.write_u16(0);
          for opt in flows_info.iter() {
            // Cumulative, like Dante hardware reports it.
            bytes.write_u32(opt.as_ref().map(|fi| fi.late_packets.load(Ordering::Relaxed)).unwrap_or(0));
          }
          // lock guard dropped here, before the await
          bytes.as_bytes().to_vec()
        };

        self.latency_block_seqnum = self.latency_block_seqnum.wrapping_add(1);
        self.send(self.heartbeat_destination, 0xfffe, [0, 8, 0, 1, 0x10, 0, 0, 0], &content).await;
      }
    } else {
      debug!("no clock available");
    }

    // this is probably response to 0738008100000064
    /* self.send(
      self.device_info_destination, 0xffff, [0x07, 0x2a, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00],
      &[0x00, 0x18, 0x00, 0x04, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x02, 0x00, 0x00,
      // supported sample rates:
      0x00, 0x00, /* 44100: */ 0xac, 0x44, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x01, 0x58, 0x88, 0x00, 0x01, 0x77, 0x00]
    ).await; */

    /* self.send(
      self.device_info_destination, 0xffff, [0x07, 0x2a, 0x10, 0x07, 0, 0, 0, 0],
      &[0, 0, 0, 0]
    ).await; */

    // this is probably response to 0738008300000064
    /* self.send(
      self.device_info_destination, 0xffff, [0x07, 0x2a, 0x00, 0x82, 0x00, 0x00, 0x00, 0x00],
    &[
      0x00, 0x18, 0x00, 0x03, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x18, 0x00, 0x02, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x20
    ]).await; */

    /* self.send(
    self.device_info_destination, 0xffff, [0x07, 0x2a, 0x10, 0x09, 0x00, 0x00, 0x00, 0x00],
    &[
      0x00, 0x00, 0x00, 0x04, 0x00, 0x02, 0x00, 0x08, 0x00, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      /* clock source: */0x00, 0x1d, 0xc1, 0xff, 0xfe, 0x11, 0x11, 0x33,
      /* transmitting to us??? : */ 0x00, 0x1d, 0xc1, 0xff, 0xfe, 0x11, 0x66, 0x33,
    ]).await; */
  }

  async fn send_clock_stats(&mut self) {
    let freq_offset = if let Some(f) = self.get_freq_offset_ppb() {
      f
    } else {
      return;
    };
    let required_prefix = format!("clock-stats.{}0000", hex::encode(self.self_info.mac_address.octets()));
    let mut master_clock = None;
    #[cfg(windows)]
    let stats_dir = std::env::temp_dir();
    #[cfg(not(windows))]
    let stats_dir = std::path::PathBuf::from("/tmp");
    if let Ok(readdir) = std::fs::read_dir(stats_dir) {
      for entry in readdir {
        if let Ok(entry) = entry {
          if entry.file_name().to_string_lossy().starts_with(&required_prefix) {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
              let content = content.trim_ascii();
              if content.len() >= 12 {
                if let Ok(master_id) = hex::decode(&content[0..16]) {
                  master_clock = Some(master_id);
                  break;
                }
              }
            }
          }
        }
      }
    }
    if let Some(mc) = master_clock {
      assert_eq!(mc.len(), 8);
      let record = clock_status_record(freq_offset, self.self_info.mac_address.octets(), &mc);
      self
        .send(
          self.device_info_destination,
          0xffff,
          [
            (CLOCK_STATUS_REVISION >> 8) as u8, CLOCK_STATUS_REVISION as u8,
            0x00, 0x20, 0x00, 0x00, 0x00, 0x00,
          ],
          &record,
        )
        .await;
    }
  }

  async fn send_network_info(&mut self) {
    let mut bytes = ByteBuffer::new();
    bytes.set_endian(bytebuffer::Endian::BigEndian);
    bytes.write_bytes(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    bytes.write_u16(self.self_info.link_speed);
    bytes.write_u16(1);
    bytes.write_bytes(&self.self_info.mac_address.octets());
    bytes.write_bytes(&self.self_info.ip_address.octets());
    bytes.write_bytes(&self.self_info.netmask.octets());
    bytes.write_bytes(&self.self_info.gateway.octets());
    bytes.write_bytes(&self.self_info.gateway.octets()); // DNS? doesn't really matter.
    bytes.write_bytes(&[
      0x00, 0x18, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);

    self
      .send(
        self.device_info_destination,
        0xffff,
        [0x07, 0x2a, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00],
        bytes.as_bytes(),
      )
      .await;
  }
}

// Clocking Status (0x0020) record revision. 0x072e is the oldest revision whose extension
// flags carry "preferred leader locked", so Dante Controller shows this device as Follower Only
// (like DVS) instead of offering it as a PTP leader, which Inferno cannot be.
const CLOCK_STATUS_REVISION: u16 = 0x072e;
const CLOCK_STATUS_EXTENSION_OFFSET: u16 = 0x34;
const CLOCK_CAP_NO_PREFERRED_LEADER: u16 = 0x0100;
const CLOCK_FLAG_FOLLOWER_ONLY: u16 = 0x0004;
const CLOCK_FLAG_PREFERRED_LEADER_LOCKED: u16 = 0x1000;
const PTP_PORT_STATE_FOLLOWER: u16 = 9;
const WORD_CLOCK_NONE: u16 = 1;

// Body following the 8-byte revision/opcode word; offsets in comments are relative to the
// revision word, which is how the record is laid out on the wire.
fn clock_status_record(freq_offset: i32, mac: [u8; 6], master: &[u8]) -> Vec<u8> {
  let mut bytes = ByteBuffer::new();
  bytes.set_endian(bytebuffer::Endian::BigEndian);
  bytes.write_bytes(&[
    0x00, 0x03, 0x00, 0x03, /* 0x08 clock state disciplined, servo synchronized */
    0x00, 0x00, 0x00, 0x9f, /* 0x0c clock source, preferred leader = no, stratum */
  ]);
  bytes.write_i32(freq_offset); // 0x10
  bytes.write_bytes(&mac); // 0x14 own PTP identity
  bytes.write_u16(0);
  bytes.write_bytes(master); // 0x1c master
  bytes.write_bytes(master); // 0x24 grandmaster
  bytes.write_u16(1); // 0x2c port count
  bytes.write_u16(CLOCK_STATUS_EXTENSION_OFFSET); // 0x2e
  bytes.write_u16(PTP_PORT_STATE_FOLLOWER); // 0x30 port 1 state
  bytes.write_u16(0);
  // 0x34 extension (44 bytes for revisions >= 0x0726)
  bytes.write_u16(CLOCK_CAP_NO_PREFERRED_LEADER);
  bytes.write_u16(0);
  bytes.write_u32(100_000); // max drift ppb
  bytes.write_u8(0);
  bytes.write_u8(0); // PTP domain
  bytes.write_u16(CLOCK_FLAG_FOLLOWER_ONLY | CLOCK_FLAG_PREFERRED_LEADER_LOCKED);
  bytes.write_bytes(&[0u8; 16]); // subdomain
  bytes.write_u16(0); // mute flags
  bytes.write_u16(WORD_CLOCK_NONE);
  bytes.write_bytes(&[0u8; 12]); // no port vector / global block
  bytes.into_vec()
}

#[cfg(test)]
mod clock_status_tests {
  use super::*;

  #[test]
  fn clock_status_advertises_follower_only() {
    let master = [0x00, 0x1d, 0xc1, 0xff, 0xfe, 0x03, 0x53, 0x66];
    let body = clock_status_record(-1234, [1, 2, 3, 4, 5, 6], &master);
    let mut r = vec![(CLOCK_STATUS_REVISION >> 8) as u8, CLOCK_STATUS_REVISION as u8, 0, 0x20, 0, 0, 0, 0];
    r.extend_from_slice(&body);
    let u16_at = |o: usize| u16::from_be_bytes([r[o], r[o + 1]]);
    assert!(u16_at(0) >= 0x072e);
    assert_eq!(r[14], 0, "must not claim preferred leader");
    assert_eq!(i32::from_be_bytes(r[16..20].try_into().unwrap()), -1234);
    assert_eq!(&r[0x14..0x1a], &[1, 2, 3, 4, 5, 6]);
    assert_eq!(&r[0x1c..0x24], &master);
    assert_eq!(u16_at(0x2c), 1);
    assert_eq!(u16_at(0x30), PTP_PORT_STATE_FOLLOWER);
    let e = u16_at(0x2e) as usize;
    assert_eq!(e, 0x34);
    assert_eq!(r.len(), e + 44);
    assert_ne!(u16_at(e) & 0x0120, 0);
    let flags = u16_at(e + 10);
    assert_ne!(flags & 0x0004, 0, "follower only");
    assert_ne!(flags & 0x1000, 0, "preferred leader locked");
    assert!(r[e + 12..e + 28].contains(&0));
    assert_eq!(u16_at(e + 28), 0, "not muted");
    assert_eq!(u16_at(e + 30), WORD_CLOCK_NONE);
    assert_eq!(u16_at(e + 34), 0, "no port vector");
  }
}

pub async fn run_server(
  self_info: Arc<DeviceInfo>,
  mut rx: mpsc::Receiver<MulticastMessage>,
  clock: Arc<RwLock<MediaClock>>,
  mut channels_sub_rx: watch::Receiver<Option<Arc<ChannelsSubscriber>>>,
  get_peaks: PeaksCallback,
  shutdown: BroadcastReceiver<()>,
) {
  let server =
    UdpSocketWrapper::new(Some(self_info.ip_address), self_info.info_request_port, shutdown).await;
  let mut recv_buff = crate::net_utils::ReceiveBuffer::new();
  let mut mcaster = Multicaster::new(self_info.as_ref(), server, clock, get_peaks);
  mcaster.send_board_info().await;
  mcaster.send_product_info().await;
  let mut heartbeat_interval = interval(Duration::from_secs(1));
  heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  while mcaster.should_work() {
    select! {
      r = mcaster.server.recv(&mut recv_buff) => {
        let (_src, request_buf) = match r {
          Some(v) => v,
          None => continue
        };
        if request_buf.len() < 32 {
          error!("too short packet received: {}", hex::encode(request_buf));
          continue;
        }
        let opcode = &request_buf[24..32];
        match opcode {
          [0x07, _, 0, 0x61, 0, 0, 0, 0] => {
            mcaster.send_board_info().await;
          },
          [0x07, _, 0, 0xc1, 0, 0, 0, 0] => {
            mcaster.send_product_info().await;
          },
          [0x07, _ /* was 0x38 */, 0, 0x21, 0, 0, 0, _ /* was 0x64 */] => {
            mcaster.send_clock_stats().await;
          },
          [0x07, _, 0, 0x13, 0, 0, 0, _] => {
            mcaster.send_network_info().await;
          }
          [0x07, _, 0, 0x77, 0, 0, 0, _]=> {
            mcaster.send(
              mcaster.device_info_destination, 0xffff, [0x07, 0x2a, 0x00, 0x78, 0, 0, 0, 0],
              &[0, 0, 0, 3, 0, 0, 0, 0]
            ).await;
          }
          [0x07, _, 0x10, 0x08, 0, 0, 0, _] => {
            // Lock Status query from conmon. Real devices (SFN-MADI, O2R) multicast 0x1009
            // with a 16-byte body; all zeros = unlocked. Unanswered, Dante Controller logs
            // "Timed out 3 times awaiting a reply to message 'Lock Status'".
            mcaster.send(
              mcaster.device_info_destination, 0xffff, [0x07, 0x2a, 0x10, 0x09, 0, 0, 0, 0],
              &[0; 16]
            ).await;
          }
          _ => {
            warn!("unknown request to multicast port: opcode: {}", hex::encode(opcode));
            warn!("raw udp payload: {}", hex::encode(request_buf));
          }
        };
      },
      m = rx.recv() => {
        // TODO we could also make seqnum atomic and simply share socket with anyone that wants it
        if let Some(msg) = m {
          mcaster.send(mcaster.device_info_destination, msg.start_code, msg.opcode, &msg.content).await;
        } else {
          break;
        }
      },
      _ = heartbeat_interval.tick() => {
        mcaster.send_heartbeat().await;
      },
      _ = channels_sub_rx.changed() => {
        mcaster.channels_subscriber = channels_sub_rx.borrow_and_update().clone();
      }
      // TODO receive shutdown properly, currently Ctrl-C doesn't work if there is error binding to socket
    };
  }
}
