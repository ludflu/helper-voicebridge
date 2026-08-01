//! In-process PulseAudio capture and playback (Linux / PipeWire-Pulse).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod preroll;
#[cfg(not(target_os = "linux"))]
mod stub;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "linux")]
pub use preroll::PreRollRecorder;
#[cfg(not(target_os = "linux"))]
pub use stub::*;
