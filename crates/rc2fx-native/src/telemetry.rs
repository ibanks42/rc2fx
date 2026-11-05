//! Fixed SPSC windows. Both endpoints are built/destroyed on control; recording
//! copies a fixed value and uses two indices, with no allocation or waiting.
use rc2fx_dsp::Meter;
use rc2fx_native_control::AudioMeter;
use std::{
    cell::UnsafeCell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const CAPACITY: usize = 8;
const WINDOW_FRAMES: u64 = 2400;
#[derive(Clone, Copy)]
struct Window {
    value: AudioMeter,
    sum_squares: f64,
    samples: u64,
}
struct Ring {
    slots: [UnsafeCell<Option<Window>>; CAPACITY],
    head: AtomicUsize,
    tail: AtomicUsize,
}
// One non-cloneable Producer and Consumer own opposite ends. Release/acquire
// indices transfer each slot's exclusive ownership before any non-atomic copy.
unsafe impl Sync for Ring {}
pub struct Producer {
    ring: Arc<Ring>,
    revision: u64,
    window_frames: u64,
    meter: Meter,
    callbacks: u64,
    frames: u64,
    clipped: u64,
    dropped: u64,
}
pub struct Consumer {
    ring: Arc<Ring>,
    latest: Option<AudioMeter>,
}
pub fn channel() -> (Producer, Consumer) {
    let ring = Arc::new(Ring {
        slots: std::array::from_fn(|_| UnsafeCell::new(None)),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        Producer {
            ring: ring.clone(),
            revision: 0,
            window_frames: 0,
            meter: Meter::default(),
            callbacks: 0,
            frames: 0,
            clipped: 0,
            dropped: 0,
        },
        Consumer { ring, latest: None },
    )
}
impl Producer {
    pub fn record(&mut self, revision: u64, frames: usize, meter: Meter, latency: u32) {
        self.record_cycles(revision, 1, frames as u64, meter, latency);
    }
    /// `cycles` callbacks of unmetered frames, left alone and counted together.
    /// Stops at each window boundary, so the windows are exactly those of
    /// recording the cycles one at a time.
    pub fn record_skipped(&mut self, revision: u64, cycles: u64, frames: usize, latency: u32) {
        self.adopt(revision);
        let frames = frames.max(1) as u64;
        let mut left = cycles;
        while left > 0 {
            let to_boundary = WINDOW_FRAMES
                .saturating_sub(self.window_frames)
                .div_ceil(frames);
            let step = left.min(to_boundary.max(1));
            self.record_cycles(revision, step, step * frames, Meter::default(), latency);
            left -= step;
        }
    }
    fn adopt(&mut self, revision: u64) {
        if self.revision != revision {
            self.revision = revision;
            self.window_frames = 0;
            self.meter = Meter::default();
        }
    }
    fn record_cycles(
        &mut self,
        revision: u64,
        cycles: u64,
        frames: u64,
        meter: Meter,
        latency: u32,
    ) {
        self.adopt(revision);
        self.callbacks = self.callbacks.saturating_add(cycles);
        self.frames = self.frames.saturating_add(frames);
        self.clipped = self.clipped.saturating_add(meter.clipped);
        self.window_frames += frames;
        self.meter.input_peak = self.meter.input_peak.max(meter.input_peak);
        self.meter.output_peak = self.meter.output_peak.max(meter.output_peak);
        self.meter.gain_reduction = self.meter.gain_reduction.max(meter.gain_reduction);
        self.meter.voice = self.meter.voice.max(meter.voice);
        self.meter.gate_open = self.meter.gate_open.max(meter.gate_open);
        self.meter.sum_squares += meter.sum_squares;
        self.meter.samples = self.meter.samples.saturating_add(meter.samples);
        if self.window_frames < WINDOW_FRAMES {
            return;
        }
        let head = self.ring.head.load(Ordering::Relaxed);
        let tail = self.ring.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) < CAPACITY {
            let value = AudioMeter {
                revision,
                callbacks: self.callbacks,
                processed_frames: self.frames,
                input_peak: self.meter.input_peak,
                output_peak: self.meter.output_peak,
                rms: 0.0, // Control computes RMS after the fixed window transfer.
                gain_reduction: self.meter.gain_reduction,
                voice: self.meter.voice,
                gate_open: self.meter.gate_open,
                clipped: self.clipped,
                latency_samples: latency,
                dropped_windows: self.dropped,
            };
            unsafe {
                *self.ring.slots[head % CAPACITY].get() = Some(Window {
                    value,
                    sum_squares: self.meter.sum_squares,
                    samples: self.meter.samples,
                });
            }
            self.ring
                .head
                .store(head.wrapping_add(1), Ordering::Release);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
        self.window_frames = 0;
        self.meter = Meter::default();
    }
}
impl Consumer {
    pub fn latest(&mut self) -> Option<AudioMeter> {
        let mut tail = self.ring.tail.load(Ordering::Relaxed);
        let head = self.ring.head.load(Ordering::Acquire);
        // A single acquired head bounds this drain even while audio continues.
        while tail != head {
            if let Some(window) = unsafe { *self.ring.slots[tail % CAPACITY].get() } {
                let mut value = window.value;
                value.rms = if window.samples == 0 {
                    0.0
                } else {
                    (window.sum_squares / window.samples as f64).sqrt()
                };
                self.latest = Some(value);
            }
            tail = tail.wrapping_add(1);
        }
        self.ring.tail.store(tail, Ordering::Release);
        self.latest
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skipped_cycles_make_exactly_the_windows_of_single_records() {
        let loud = Meter {
            input_peak: 0.5,
            output_peak: 0.5,
            sum_squares: 28.0,
            samples: 112,
            ..Meter::default()
        };
        let (mut batched, mut batched_out) = channel();
        let (mut single, mut single_out) = channel();
        // Every eighth cycle metered, the rest left alone, across many windows.
        for _ in 0..40 {
            batched.record_skipped(1, 7, 112, 9);
            batched.record(1, 112, loud, 9);
            for _ in 0..7 {
                single.record(1, 112, Meter::default(), 9);
            }
            single.record(1, 112, loud, 9);
            let (a, b) = (batched_out.latest(), single_out.latest());
            assert_eq!(
                a.map(|v| (v.callbacks, v.processed_frames)),
                b.map(|v| (v.callbacks, v.processed_frames))
            );
            assert_eq!(a.map(|v| v.rms.to_bits()), b.map(|v| v.rms.to_bits()));
            assert_eq!(
                a.map(|v| v.input_peak.to_bits()),
                b.map(|v| v.input_peak.to_bits())
            );
        }
        assert!(batched_out.latest().is_some());
    }
    #[test]
    fn windows_are_coherent_bounded_and_never_mix_revisions() {
        let (mut producer, mut consumer) = channel();
        let meter = Meter {
            input_peak: 0.5,
            output_peak: 0.25,
            sum_squares: 150.0,
            samples: 2400,
            ..Meter::default()
        };
        producer.record(1, 112, meter, 1184);
        assert!(consumer.latest().is_none());
        producer.record(2, 2400, meter, 1184);
        let value = consumer.latest().unwrap();
        assert_eq!(value.revision, 2);
        assert_eq!(value.callbacks, 2);
        assert_eq!(value.rms, 0.25);
        for _ in 0..CAPACITY + 1 {
            producer.record(2, 2400, meter, 1184);
        }
        assert_eq!(consumer.latest().unwrap().dropped_windows, 0);
        producer.record(3, 2400, meter, 1184);
        let value = consumer.latest().unwrap();
        assert_eq!(value.revision, 3);
        assert_eq!(value.dropped_windows, 1);
    }
}
