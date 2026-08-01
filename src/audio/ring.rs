//! Fixed-capacity PCM ring buffer (tail retained on overflow).

pub(crate) struct RingBuf {
    data: Vec<u8>,
    cap: usize,
}

impl RingBuf {
    pub(crate) fn new(cap_bytes: usize) -> Self {
        Self {
            data: Vec::new(),
            cap: cap_bytes,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.data.extend_from_slice(chunk);
        if self.data.len() > self.cap {
            self.data.drain(..self.data.len() - self.cap);
        }
    }

    pub(crate) fn slice_from(&self, start: usize) -> Vec<u8> {
        self.data.get(start..).unwrap_or_default().to_vec()
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_tail() {
        let mut ring = RingBuf::new(10);
        ring.push(&[1, 2, 3]);
        ring.push(&[4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(ring.data, vec![3, 4, 5, 6, 7, 8, 9, 10, 11]);
    }

    #[test]
    fn slice_from_offset() {
        let mut ring = RingBuf::new(100);
        ring.push(&[1, 2, 3, 4, 5]);
        assert_eq!(ring.slice_from(2), vec![3, 4, 5]);
    }
}
