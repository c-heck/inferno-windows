use thread_priority::{thread_native_id, Error, ThreadPriority};

#[cfg(target_family = "unix")]
pub fn set_current_thread_realtime(priority_hint: u8) -> Result<(), Error> {
  use thread_priority::unix::set_thread_priority_and_policy;
  use thread_priority::{
    RealtimeThreadSchedulePolicy, ThreadPriority, ThreadPriorityValue, ThreadSchedulePolicy,
  };

  let priority_value = ThreadPriorityValue::try_from(priority_hint)
    .map_err(|_| Error::InvalidThreadPriority)?;
  set_thread_priority_and_policy(
    thread_native_id(),
    ThreadPriority::Crossplatform(priority_value),
    ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo),
  )
}

#[cfg(windows)]
pub fn set_current_thread_realtime(_priority_hint: u32) -> Result<(), Error> {
  use std::ffi::c_void;
  #[link(name = "avrt")]
  extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> *mut c_void;
    fn AvSetMmThreadPriority(handle: *mut c_void, priority: i32) -> i32;
  }
  #[link(name = "kernel32")]
  extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn SetThreadInformation(thread: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
  }
  // Opt out of EcoQoS (ThreadPowerThrottling, EXECUTION_SPEED controlled and off) so packet
  // threads are not parked on low-frequency operating points on Snapdragon.
  let throttling: [u32; 3] = [1, 1, 0];
  unsafe { SetThreadInformation(GetCurrentThread(), 3, throttling.as_ptr() as *const c_void, 12) };
  // DAW and ASIO threads run under MMCSS "Pro Audio" (realtime priority range) and would
  // otherwise preempt packet transmission for milliseconds. The registration ends with the thread.
  let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
  let mut index = 0u32;
  let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
  if !handle.is_null() {
    unsafe { AvSetMmThreadPriority(handle, 2 /* AVRT_PRIORITY_CRITICAL */) };
    return Ok(());
  }
  thread_priority::set_current_thread_priority(ThreadPriority::Max)
}

#[cfg(not(any(target_family = "unix", windows)))]
pub fn set_current_thread_realtime(_priority_hint: u32) -> Result<(), Error> {
  thread_priority::set_current_thread_priority(ThreadPriority::Max)
}
