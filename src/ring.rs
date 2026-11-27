//! The buffer between the decoder (writes) and the audio output (reads), without a lock on
//! the output's side.
//!
//! The output callback runs on the system's real-time audio thread. With a `Mutex` it had to
//! wait whenever the decoder was writing — and a growing `VecDeque` copies megabytes while
//! it holds the lock (192 kHz, seconds ahead). Late callbacks are what is heard as crackles,
//! most of all in exclusive mode at high sample rates, where each callback has the least
//! time. Here the output only reads two atomic positions.
//!
//! One writer (the decoder thread), one reader (the output). Samples are interleaved stereo
//! and always move in whole frames. What does not fit waits in `pending`, which only the
//! writer touches.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

/// 2 M samples = 1 M stereo frames: 5.4 s at 192 kHz (more waits in `pending`).
const CAPACITY: usize = 1 << 21;

pub struct Ring {
    buf: Box<[AtomicU32]>,
    mask: usize,
    /// samples written so far (writer)
    write: AtomicUsize,
    /// samples read so far (reader)
    read: AtomicUsize,
    /// writer: everything before this position was dropped (seek / new song)
    clear_to: AtomicUsize,
    pending: Mutex<Vec<f32>>,
    pending_len: AtomicUsize,
}

impl Default for Ring {
    fn default() -> Self {
        Ring::with_capacity(CAPACITY)
    }
}

impl Ring {
    fn with_capacity(cap: usize) -> Ring {
        let cap = cap.next_power_of_two().max(2);
        Ring {
            buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            mask: cap - 1,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            clear_to: AtomicUsize::new(0),
            pending: Mutex::new(Vec::new()),
            pending_len: AtomicUsize::new(0),
        }
    }

    fn start(&self) -> usize {
        self.read.load(Ordering::Acquire).max(self.clear_to.load(Ordering::Acquire))
    }

    /// Samples waiting to be played (including those that did not fit yet).
    pub fn len(&self) -> usize {
        let w = self.write.load(Ordering::Acquire);
        w.saturating_sub(self.start()) + self.pending_len.load(Ordering::Relaxed)
    }

    /// Writer: drops everything not played yet.
    pub fn clear(&self) {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.clear();
        self.pending_len.store(0, Ordering::Relaxed);
        self.clear_to
            .store(self.write.load(Ordering::Relaxed), Ordering::Release);
    }

    /// Writer: appends samples (whole stereo frames).
    pub fn push(&self, data: &[f32]) {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.extend_from_slice(data);
        self.flush_locked(&mut p);
    }

    /// Writer: moves what waited in `pending` into the buffer, as far as there is room.
    pub fn flush(&self) {
        if self.pending_len.load(Ordering::Relaxed) > 0 {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            self.flush_locked(&mut p);
        }
    }

    fn flush_locked(&self, p: &mut Vec<f32>) {
        let w = self.write.load(Ordering::Relaxed);
        let free = self.buf.len() - (w - self.start());
        // whole frames only, so the reader never ends up between left and right
        let n = free.min(p.len()) & !1;
        for (i, s) in p[..n].iter().enumerate() {
            self.buf[(w + i) & self.mask].store(s.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w + n, Ordering::Release);
        p.drain(..n);
        self.pending_len.store(p.len(), Ordering::Relaxed);
    }

    /// Reader: takes up to `dst.len()` samples; returns how many. Never waits.
    pub fn pop(&self, dst: &mut [f32]) -> usize {
        let mut r = self.read.load(Ordering::Relaxed);
        let c = self.clear_to.load(Ordering::Acquire);
        if c > r {
            r = c;
        }
        let w = self.write.load(Ordering::Acquire);
        let n = (w - r).min(dst.len()) & !1;
        for (i, d) in dst[..n].iter_mut().enumerate() {
            *d = f32::from_bits(self.buf[(r + i) & self.mask].load(Ordering::Relaxed));
        }
        self.read.store(r + n, Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_overflow_and_clear() {
        let r = Ring::with_capacity(8);
        r.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        r.push(&[7.0, 8.0, 9.0, 10.0]); // 2 samples wait in pending
        assert_eq!(r.len(), 10);
        let mut out = [0f32; 6];
        assert_eq!(r.pop(&mut out), 6);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        r.flush();
        let mut out = [0f32; 8];
        assert_eq!(r.pop(&mut out), 4);
        assert_eq!(&out[..4], &[7.0, 8.0, 9.0, 10.0]);
        // clear drops what was not read, new samples come next
        r.push(&[1.0, 1.0, 2.0, 2.0]);
        r.clear();
        assert_eq!(r.len(), 0);
        r.push(&[3.0, 3.0]);
        assert_eq!(r.pop(&mut out), 2);
        assert_eq!(&out[..2], &[3.0, 3.0]);
        // an odd request still takes whole frames
        r.push(&[4.0, 4.0, 5.0, 5.0]);
        let mut three = [0f32; 3];
        assert_eq!(r.pop(&mut three), 2);
    }

    #[test]
    fn two_threads() {
        let r = std::sync::Arc::new(Ring::with_capacity(1024));
        let w = r.clone();
        // a multiple of the block size, so the reader takes exactly what was written
        const TOTAL: u32 = 256 * 800;
        let writer = std::thread::spawn(move || {
            let mut next = 0u32;
            while next < TOTAL {
                if w.len() < 4096 {
                    let block: Vec<f32> = (next..next + 256).map(|v| v as f32).collect();
                    w.push(&block);
                    next += 256;
                } else {
                    w.flush();
                    std::thread::yield_now();
                }
            }
            while w.len() > 0 {
                w.flush();
                std::thread::yield_now();
            }
        });
        let mut expect = 0u32;
        let mut buf = [0f32; 100];
        while expect < TOTAL {
            let n = r.pop(&mut buf);
            for v in &buf[..n] {
                assert_eq!(*v as u32, expect);
                expect += 1;
            }
        }
        writer.join().unwrap();
    }
}
