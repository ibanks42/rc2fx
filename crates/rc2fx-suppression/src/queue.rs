use std::{
    cell::UnsafeCell,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

pub(crate) const CAPACITY: usize = 8;
pub(crate) const MAX_FRAME: usize = 480;
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub sequence: u64,
    pub submitted_ns: Option<u64>,
    pub valid: bool,
    /// The model's voice score for this frame, 0 to 1.
    pub voice: f32,
    pub samples: [f32; MAX_FRAME],
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            sequence: 0,
            submitted_ns: None,
            valid: false,
            voice: 0.,
            samples: [0.; MAX_FRAME],
        }
    }
}
struct Queue {
    slots: [UnsafeCell<Frame>; CAPACITY],
    written: AtomicU32,
    read: AtomicU32,
}
// Each slot is written only by the unique producer after acquiring the consumer
// index, and read only by the unique consumer after acquiring the producer index.
// Endpoints cannot be cloned; their mutable methods prevent concurrent access.
unsafe impl Sync for Queue {}
pub(crate) struct Producer(Arc<Queue>);
pub(crate) struct Consumer(Arc<Queue>);
pub(crate) fn channel() -> (Producer, Consumer) {
    let q = Arc::new(Queue {
        slots: std::array::from_fn(|_| UnsafeCell::new(Frame::default())),
        written: AtomicU32::new(0),
        read: AtomicU32::new(0),
    });
    (Producer(q.clone()), Consumer(q))
}
impl Producer {
    pub fn push(&mut self, frame: &Frame) -> bool {
        let w = self.0.written.load(Ordering::Relaxed);
        let r = self.0.read.load(Ordering::Acquire);
        if w.wrapping_sub(r) >= CAPACITY as u32 {
            return false;
        }
        // SAFETY: this unique producer owns the free slot until release below.
        unsafe {
            *self.0.slots[w as usize % CAPACITY].get() = *frame;
        }
        self.0.written.store(w.wrapping_add(1), Ordering::Release);
        true
    }
    pub fn pending(&self) -> u32 {
        self.0
            .written
            .load(Ordering::Acquire)
            .wrapping_sub(self.0.read.load(Ordering::Acquire))
    }
}
impl Consumer {
    /// Consumer-side observation; producer may enqueue immediately afterward.
    pub fn pending(&self) -> u32 {
        let read = self.0.read.load(Ordering::Relaxed);
        self.0.written.load(Ordering::Acquire).wrapping_sub(read)
    }
    pub fn sequence(&mut self) -> Option<u64> {
        let r = self.0.read.load(Ordering::Relaxed);
        if r == self.0.written.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: producer cannot reuse the occupied slot until our read release.
        Some(unsafe { (*self.0.slots[r as usize % CAPACITY].get()).sequence })
    }
    pub fn pop(&mut self) -> Option<Frame> {
        let r = self.0.read.load(Ordering::Relaxed);
        if r == self.0.written.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: unique consumer owns this occupied slot until release below.
        let frame = unsafe { *self.0.slots[r as usize % CAPACITY].get() };
        self.0.read.store(r.wrapping_add(1), Ordering::Release);
        Some(frame)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_wrap_and_future_peek() {
        let (mut p, mut c) = channel();
        p.0.written.store(u32::MAX - 3, Ordering::Relaxed);
        p.0.read.store(u32::MAX - 3, Ordering::Relaxed);
        for sequence in 0..8 {
            assert!(p.push(&Frame {
                sequence,
                ..Frame::default()
            }));
        }
        assert!(!p.push(&Frame::default()));
        assert_eq!(c.sequence(), Some(0));
        assert_eq!(p.pending(), 8);
        for sequence in 0..8 {
            assert_eq!(c.pop().unwrap().sequence, sequence);
        }
        assert!(c.pop().is_none());
        assert_eq!(p.pending(), 0);
    }
    #[test]
    fn concurrent_owned_slots() {
        let (mut p, mut c) = channel();
        let t = std::thread::spawn(move || {
            for sequence in 0..20000 {
                let f = Frame {
                    sequence,
                    valid: true,
                    voice: 0.,
                    samples: [sequence as f32; MAX_FRAME],
                    submitted_ns: Some(sequence),
                };
                while !p.push(&f) {
                    std::thread::yield_now();
                }
            }
        });
        for sequence in 0..20000 {
            let f = loop {
                if let Some(f) = c.pop() {
                    break f;
                }
                std::thread::yield_now();
            };
            assert_eq!(f.sequence, sequence);
            assert_eq!(f.submitted_ns, Some(sequence));
            assert!(f.valid);
            assert!(f.samples.iter().all(|&x| x == sequence as f32));
        }
        t.join().unwrap();
    }
}
