//! Experimental, unofficial AoIP ASIO device. No Audinate code or SDK is included.
#![cfg(windows)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(target_pointer_width = "64"))]
compile_error!("Only native ARM64 and x64 Windows hosts are supported.");

pub mod abi;
mod com;
pub mod config;
mod driver;
mod engine;
mod logfile;
mod ptp;
pub mod power;
mod timer;

pub use com::{DllCanUnloadNow, DllGetClassObject};
