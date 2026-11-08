//! Process-wide audio traffic counters reported in the 0x8000 interface heartbeat record.

use std::sync::atomic::{AtomicU64, Ordering};

/// Ethernet + IPv4 + UDP header bytes added to each audio payload on the wire.
pub const FRAME_OVERHEAD: u64 = 14 + 20 + 8;

pub static TX_BYTES: AtomicU64 = AtomicU64::new(0);
pub static RX_BYTES: AtomicU64 = AtomicU64::new(0);
/// Audio packets missing from received flows (gaps in RTP timestamps).
pub static RX_ERRORS: AtomicU64 = AtomicU64::new(0);

pub fn add_tx(payload: usize) {
  TX_BYTES.fetch_add(payload as u64 + FRAME_OVERHEAD, Ordering::Relaxed);
}

pub fn add_rx(payload: usize) {
  RX_BYTES.fetch_add(payload as u64 + FRAME_OVERHEAD, Ordering::Relaxed);
}

/// Snapshot of all counters: (tx bytes, rx bytes, rx errors).
pub fn snapshot() -> (u64, u64, u64) {
  (
    TX_BYTES.load(Ordering::Relaxed),
    RX_BYTES.load(Ordering::Relaxed),
    RX_ERRORS.load(Ordering::Relaxed),
  )
}
