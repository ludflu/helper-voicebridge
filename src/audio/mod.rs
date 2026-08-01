//! In-process PipeWire capture and PulseAudio playback (Linux).

#[cfg(target_os = "linux")]
mod capture_tap;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod pipewire_capture;
#[cfg(target_os = "linux")]
mod preroll;
#[cfg(target_os = "linux")]
mod ring;
#[cfg(not(target_os = "linux"))]
mod stub;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "linux")]
pub use preroll::PreRollRecorder;
#[cfg(not(target_os = "linux"))]
pub use stub::*;
