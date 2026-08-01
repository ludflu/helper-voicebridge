use anyhow::{bail, Result};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;
use tokio::sync::mpsc;

/// Stub for non-Linux builds (tests on macOS).
pub fn unsuspend_source(_source: &str) -> Result<()> {
    Ok(())
}

pub fn spawn_recorder(
    _source: &str,
    _chunk_tx: mpsc::Sender<Vec<u8>>,
    _stop: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>> {
    bail!("audio capture requires Linux with PulseAudio/PipeWire")
}

pub struct PreRollRecorder;

impl PreRollRecorder {
    pub fn spawn(_source: &str, _ring_ms: u64) -> Result<Arc<Self>> {
        bail!("pre-roll capture requires Linux")
    }

    pub fn begin_session(self: &Arc<Self>, _preroll_ms: u64) {}

    pub fn end_session(&self) -> Vec<u8> {
        Vec::new()
    }
}

pub fn spawn_playback(
    _pcm: Vec<u8>,
    _sample_rate: u32,
    _sink: Option<&str>,
    _stop: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>> {
    bail!("audio playback requires Linux with PulseAudio/PipeWire")
}

pub fn playback_warmup() -> Duration {
    Duration::from_millis(400)
}
