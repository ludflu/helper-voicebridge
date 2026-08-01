//! Continuous PipeWire capture into a ring buffer so button-press latency does not clip speech.

use anyhow::Result;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::task::JoinHandle;

use super::capture_tap::CaptureTap;
use super::pipewire_capture;

const SAMPLE_RATE: u32 = 16_000;
const BYTES_PER_MS: usize = (SAMPLE_RATE as usize * 2) / 1000;

/// Always-on mic ring; `begin_session` / `end_session` delimit a button press.
pub struct PreRollRecorder {
    tap: Arc<CaptureTap>,
    stop: Arc<AtomicBool>,
    source: String,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl PreRollRecorder {
    pub fn spawn(source: &str, ring_ms: u64) -> Result<Arc<Self>> {
        let cap = BYTES_PER_MS * ring_ms as usize;
        let recorder = Arc::new(Self {
            tap: Arc::new(CaptureTap::new(cap)),
            stop: Arc::new(AtomicBool::new(false)),
            source: source.to_string(),
            task: Mutex::new(None),
        });
        recorder.ensure_running()?;
        Ok(recorder)
    }

    pub fn begin_session(self: &Arc<Self>, preroll_ms: u64) {
        self.ensure_running().ok();
        let (ring_len, preroll_len) = self.tap.begin_session(preroll_ms);
        eprintln!(
            "capture armed (pre-roll {}ms, ring {} bytes, session starts with {} bytes)",
            preroll_ms, ring_len, preroll_len
        );
    }

    pub fn end_session(&self) -> Vec<u8> {
        let pcm = self.tap.end_session();
        let ms = pcm.len() as u64 / BYTES_PER_MS as u64;
        eprintln!("capture took {} bytes ({ms}ms, pre-roll included)", pcm.len());
        pcm
    }

    fn ensure_running(self: &Arc<Self>) -> Result<()> {
        let mut guard = self.task.lock().expect("preroll task lock");
        if guard.as_ref().is_some_and(|t| !t.is_finished()) {
            return Ok(());
        }
        self.stop.store(false, Ordering::Relaxed);
        crate::audio::linux::unsuspend_source(&self.source)?;
        let stop = self.stop.clone();
        let tap = self.tap.clone();
        let source = self.source.clone();
        *guard = Some(tokio::task::spawn_blocking(move || {
            if let Err(e) = pipewire_capture::capture_to_tap(&source, tap, stop) {
                eprintln!("pre-roll capture stopped: {e:#}");
            }
        }));
        Ok(())
    }
}
