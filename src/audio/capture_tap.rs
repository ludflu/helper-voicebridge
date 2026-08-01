//! Ring buffer for pre-roll plus a session accumulator for the active button press.

use super::ring::RingBuf;
use std::sync::Mutex;

const SAMPLE_RATE: u32 = 16_000;
const BYTES_PER_MS: usize = (SAMPLE_RATE as usize * 2) / 1000;

/// Always-on ring (pre-roll) and optional per-session PCM (button press → release).
pub(crate) struct CaptureTap {
    ring: Mutex<RingBuf>,
    session: Mutex<Option<Vec<u8>>>,
}

impl CaptureTap {
    pub(crate) fn new(ring_cap_bytes: usize) -> Self {
        Self {
            ring: Mutex::new(RingBuf::new(ring_cap_bytes)),
            session: Mutex::new(None),
        }
    }

    pub(crate) fn push(&self, chunk: &[u8]) {
        let mut ring = self.ring.lock().expect("capture ring lock");
        ring.push(chunk);
        drop(ring);
        if let Some(session) = self.session.lock().expect("capture session lock").as_mut() {
            session.extend_from_slice(chunk);
        }
    }

    pub(crate) fn begin_session(&self, preroll_ms: u64) -> (usize, usize) {
        let preroll_bytes = BYTES_PER_MS * preroll_ms as usize;
        let ring = self.ring.lock().expect("capture ring lock");
        let preroll = ring.slice_from(ring.len().saturating_sub(preroll_bytes));
        let ring_len = ring.len();
        drop(ring);
        let session_len = preroll.len();
        *self.session.lock().expect("capture session lock") = Some(preroll);
        (ring_len, session_len)
    }

    pub(crate) fn end_session(&self) -> Vec<u8> {
        self.session
            .lock()
            .expect("capture session lock")
            .take()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_keeps_full_utterance_after_ring_wraps() {
        let tap = CaptureTap::new(10);
        // Fill ring.
        tap.push(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let (ring_len, preroll_len) = tap.begin_session(1000);
        assert_eq!(ring_len, 10);
        assert_eq!(preroll_len, 10);

        // New audio wraps the ring; a stale index into the ring would lose the start.
        tap.push(&[10, 11, 12, 13, 14]);
        tap.push(&[15, 16, 17, 18, 19]);

        let pcm = tap.end_session();
        assert_eq!(pcm, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19]);
    }
}
