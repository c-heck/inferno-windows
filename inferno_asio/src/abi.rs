use std::ffi::{c_char, c_void};

pub type Error = i32;
pub const OK: Error = 0;
pub const SUCCESS: Error = 0x3f4847a0;
pub const NOT_PRESENT: Error = -1000;
pub const HW_ERROR: Error = -999;
pub const INVALID_PARAMETER: Error = -998;
pub const INVALID_MODE: Error = -997;
pub const NOT_RUNNING: Error = -996;
pub const NO_CLOCK: Error = -995;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid {
    pub a: u32,
    pub b: u16,
    pub c: u16,
    pub d: [u8; 8],
}

pub const CLSID: Guid = Guid {
    a: 0xc4cd4cf3,
    b: 0x37e1,
    c: 0x4b6e,
    d: [0xab, 0x2f, 0x90, 0x01, 0x6a, 0xb0, 0xa6, 0x40],
};
pub const IUNKNOWN: Guid = Guid {
    a: 0,
    b: 0,
    c: 0,
    d: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
pub const ICLASS_FACTORY: Guid = Guid { a: 1, ..IUNKNOWN };
pub const E_NOINTERFACE: i32 = 0x80004002u32 as i32;
pub const E_POINTER: i32 = 0x80004003u32 as i32;
pub const CLASS_NOT_AVAILABLE: i32 = 0x80040111u32 as i32;
pub const NO_AGGREGATION: i32 = 0x80040110u32 as i32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Split64 {
    pub hi: u32,
    pub lo: u32,
}
impl From<u64> for Split64 {
    fn from(value: u64) -> Self {
        Self {
            hi: (value >> 32) as u32,
            lo: value as u32,
        }
    }
}

#[repr(C, packed(4))]
pub struct BufferInfo {
    pub is_input: i32,
    pub channel: i32,
    pub buffers: [*mut c_void; 2],
}

#[repr(C, packed(4))]
pub struct ChannelInfo {
    pub channel: i32,
    pub is_input: i32,
    pub active: i32,
    pub group: i32,
    pub sample_type: i32,
    pub name: [c_char; 32],
}

#[repr(C, packed(4))]
pub struct ClockSource {
    pub index: i32,
    pub associated_channel: i32,
    pub associated_group: i32,
    pub current: i32,
    pub name: [c_char; 32],
}

#[repr(C, packed(4))]
#[derive(Default)]
pub struct TimeInfo {
    pub speed: f64,
    pub system_time: Split64,
    pub sample_position: Split64,
    pub sample_rate: f64,
    pub flags: u32,
    pub reserved: [u8; 12],
}

#[repr(C, packed(4))]
pub struct TimeCode {
    pub speed: f64,
    pub samples: Split64,
    pub flags: u32,
    pub future: [u8; 64],
}

#[repr(C, packed(4))]
pub struct Time {
    pub reserved: [i32; 4],
    pub info: TimeInfo,
    pub code: TimeCode,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct Callbacks {
    pub switch: Option<unsafe extern "C" fn(i32, i32)>,
    pub rate_changed: Option<unsafe extern "C" fn(f64)>,
    pub message: Option<unsafe extern "C" fn(i32, i32, *mut c_void, *mut f64) -> i32>,
    pub switch_time: Option<unsafe extern "C" fn(*mut Time, i32, i32) -> *mut Time>,
}

pub fn write_text<const N: usize>(dest: &mut [c_char; N], text: &str) {
    dest.fill(0);
    for (out, byte) in dest.iter_mut().take(N - 1).zip(text.bytes()) {
        *out = byte as c_char;
    }
}

#[repr(C)]
pub struct AsioVtable {
    pub query: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub init: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    pub name: unsafe extern "system" fn(*mut c_void, *mut c_char),
    pub version: unsafe extern "system" fn(*mut c_void) -> i32,
    pub error: unsafe extern "system" fn(*mut c_void, *mut c_char),
    pub start: unsafe extern "system" fn(*mut c_void) -> Error,
    pub stop: unsafe extern "system" fn(*mut c_void) -> Error,
    pub channels: unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32) -> Error,
    pub latencies: unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32) -> Error,
    pub buffer_size:
        unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32, *mut i32, *mut i32) -> Error,
    pub can_rate: unsafe extern "system" fn(*mut c_void, f64) -> Error,
    pub get_rate: unsafe extern "system" fn(*mut c_void, *mut f64) -> Error,
    pub set_rate: unsafe extern "system" fn(*mut c_void, f64) -> Error,
    pub clocks: unsafe extern "system" fn(*mut c_void, *mut ClockSource, *mut i32) -> Error,
    pub set_clock: unsafe extern "system" fn(*mut c_void, i32) -> Error,
    pub position: unsafe extern "system" fn(*mut c_void, *mut Split64, *mut Split64) -> Error,
    pub channel_info: unsafe extern "system" fn(*mut c_void, *mut ChannelInfo) -> Error,
    pub create: unsafe extern "system" fn(
        *mut c_void,
        *mut BufferInfo,
        i32,
        i32,
        *const Callbacks,
    ) -> Error,
    pub dispose: unsafe extern "system" fn(*mut c_void) -> Error,
    pub panel: unsafe extern "system" fn(*mut c_void) -> Error,
    pub future: unsafe extern "system" fn(*mut c_void, i32, *mut c_void) -> Error,
    pub output_ready: unsafe extern "system" fn(*mut c_void) -> Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_64_bit_abi_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<Guid>(), 16);
        assert_eq!(size_of::<Split64>(), 8);
        assert_eq!(size_of::<BufferInfo>(), 24);
        assert_eq!(size_of::<ChannelInfo>(), 52);
        assert_eq!(size_of::<ClockSource>(), 48);
        assert_eq!(size_of::<Callbacks>(), 32);
        assert_eq!(size_of::<TimeInfo>(), 48);
        assert_eq!(size_of::<TimeCode>(), 84);
        assert_eq!(size_of::<Time>(), 148);
        assert_eq!(align_of::<Time>(), 4);
        assert_eq!(align_of::<BufferInfo>(), 4);
        assert_eq!(align_of::<Callbacks>(), 4);
        assert_eq!(offset_of!(Time, info), 16);
        assert_eq!(offset_of!(Time, code), 64);
        assert_eq!(size_of::<AsioVtable>(), 24 * 8);
    }
}
