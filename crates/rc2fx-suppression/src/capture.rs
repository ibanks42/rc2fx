//! Explicitly armed, bounded worker timing capture. One worker writes frames;
//! one endpoint writes the first-miss latch. Control readers never wait.
//! This is separate from ordinary, always-on performance counters.
use crate::outlier::{EventRecord, Recorded, Slot};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const CAPTURE_FRAMES: usize = 128;
pub const CAPTURE_POST_SEQUENCES: u64 = 32;
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureState {
    Armed,
    Triggered,
    Frozen,
    Stopped,
}
impl CaptureState {
    fn word(self) -> u64 {
        match self {
            Self::Armed => 1,
            Self::Triggered => 2,
            Self::Frozen => 3,
            Self::Stopped => 4,
        }
    }
    fn from_word(word: u64) -> Self {
        match word {
            1 => Self::Armed,
            2 => Self::Triggered,
            3 => Self::Frozen,
            4 => Self::Stopped,
            _ => unreachable!("private capture state"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureFrame {
    pub sequence: u64,
    pub submitted_ns: Option<u64>,
    pub started_ns: Option<u64>,
    pub finished_ns: Option<u64>,
    /// None means the output was not published or its clock read failed.
    pub published_ns: Option<u64>,
    pub cpu_ns: Option<u64>,
    pub previous_sequence: Option<u64>,
    pub previous_published_ns: Option<u64>,
    /// Observed just after dequeue; the producer may concurrently enqueue.
    pub input_pending_after_pop: u32,
    pub output_published: bool,
}
impl CaptureFrame {
    fn words(self, epoch: u32, ordinal: u64) -> [u64; 12] {
        let optional = [
            self.submitted_ns,
            self.started_ns,
            self.finished_ns,
            self.published_ns,
            self.cpu_ns,
            self.previous_sequence,
            self.previous_published_ns,
        ];
        let flags = optional
            .iter()
            .enumerate()
            .fold(0, |f, (i, v)| f | (u64::from(v.is_some()) << i))
            | (u64::from(self.output_published) << 7);
        [
            epoch.into(),
            ordinal,
            self.sequence,
            optional[0].unwrap_or(0),
            optional[1].unwrap_or(0),
            optional[2].unwrap_or(0),
            optional[3].unwrap_or(0),
            optional[4].unwrap_or(0),
            optional[5].unwrap_or(0),
            optional[6].unwrap_or(0),
            self.input_pending_after_pop.into(),
            flags,
        ]
    }
    fn from_words(w: [u64; 12]) -> Self {
        let get = |index: usize| (w[11] & (1 << index) != 0).then_some(w[index + 3]);
        Self {
            sequence: w[2],
            submitted_ns: get(0),
            started_ns: get(1),
            finished_ns: get(2),
            published_ns: get(3),
            cpu_ns: get(4),
            previous_sequence: get(5),
            previous_published_ns: get(6),
            input_pending_after_pop: w[10] as u32,
            output_published: w[11] & 128 != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub instance: u64,
    pub requested_epoch: u32,
    pub applied_epoch: u32,
    pub state: CaptureState,
    pub armed_at_ns: Option<u64>,
    pub trigger: Option<EventRecord>,
    pub recorded: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureWindow {
    pub metadata: CaptureMetadata,
    pub capacity: usize,
    /// Earlier records overwritten before the ring froze (not queue drops).
    pub overwritten: u64,
    pub frames: Vec<CaptureFrame>,
}

pub(crate) struct Capture {
    instance: u64,
    requested: AtomicU32,
    active: AtomicU32,
    armed_at: AtomicU64,
    // Only the endpoint writes `latched_epoch` and `first_miss`.
    latched_epoch: AtomicU32,
    first_miss: Slot<3>,
    // Only the worker writes metadata and frame slots.
    metadata: Slot<7>,
    frames: [Slot<12>; CAPTURE_FRAMES],
}
impl Default for Capture {
    fn default() -> Self {
        Self {
            instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            requested: AtomicU32::new(0),
            active: AtomicU32::new(0),
            armed_at: AtomicU64::new(0),
            latched_epoch: AtomicU32::new(0),
            first_miss: Slot::default(),
            metadata: Slot::default(),
            frames: std::array::from_fn(|_| Slot::default()),
        }
    }
}
impl Capture {
    pub fn identity(&self) -> (u64, u32) {
        (self.instance, self.requested.load(Ordering::Acquire))
    }
    /// Control-side only. Monotonic epochs, no rearming by repeated polling.
    pub fn arm(&self, epoch: u32) -> bool {
        if epoch == 0 {
            return false;
        }
        self.requested
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (epoch > old).then_some(epoch)
            })
            .is_ok()
    }
    /// Endpoint-only one-shot. The active-epoch acquire is the attribution
    /// point. Publication from an old epoch racing rearm remains old-tagged.
    pub fn missed(&self, event: EventRecord) {
        let epoch = self.active.load(Ordering::Acquire);
        if epoch == 0 || epoch == self.latched_epoch.load(Ordering::Relaxed) {
            return;
        }
        let armed_at = self.armed_at.load(Ordering::Relaxed);
        if event.at_ns != 0 && armed_at != 0 && event.at_ns < armed_at {
            return;
        }
        self.latched_epoch.store(epoch, Ordering::Relaxed);
        self.first_miss
            .write([epoch.into(), event.sequence, event.at_ns]);
    }
    fn decode_metadata(&self, w: [u64; 7]) -> CaptureMetadata {
        CaptureMetadata {
            instance: self.instance,
            requested_epoch: self.requested.load(Ordering::Acquire),
            applied_epoch: w[0] as u32,
            state: CaptureState::from_word(w[1]),
            recorded: w[2],
            armed_at_ns: (w[6] & 1 != 0).then_some(w[3]),
            trigger: (w[6] & 2 != 0).then_some(EventRecord {
                sequence: w[4],
                at_ns: w[5],
            }),
        }
    }
    fn acknowledged(&self, words: [u64; 7]) -> bool {
        let state = CaptureState::from_word(words[1]);
        !matches!(state, CaptureState::Armed | CaptureState::Triggered)
            || self.active.load(Ordering::Acquire) == words[0] as u32
    }
    pub fn metadata(&self) -> Recorded<CaptureMetadata> {
        match self.metadata.read() {
            Recorded::Stable(w) if self.acknowledged(w) => {
                Recorded::Stable(self.decode_metadata(w))
            }
            Recorded::Stable(_) => Recorded::Busy,
            Recorded::Empty => Recorded::Empty,
            Recorded::Busy => Recorded::Busy,
        }
    }
    /// Cold diagnostic path only; ordinary telemetry must not copy this ring.
    pub fn snapshot(&self) -> Recorded<CaptureWindow> {
        let before = match self.metadata.read() {
            Recorded::Stable(w) => w,
            Recorded::Empty => return Recorded::Empty,
            Recorded::Busy => return Recorded::Busy,
        };
        if !self.acknowledged(before) {
            return Recorded::Busy;
        }
        let count = before[2];
        let first = count.saturating_sub(CAPTURE_FRAMES as u64);
        let mut frames = Vec::with_capacity((count - first) as usize);
        for ordinal in first..count {
            match self.frames[ordinal as usize % CAPTURE_FRAMES].read() {
                Recorded::Stable(w) if w[0] == before[0] && w[1] == ordinal => {
                    frames.push(CaptureFrame::from_words(w))
                }
                _ => return Recorded::Busy,
            }
        }
        // Check epoch AND publication count: otherwise a wrap/rearm can mix
        // histories even if every individual slot was internally consistent.
        if self.metadata.read() != Recorded::Stable(before) {
            return Recorded::Busy;
        }
        Recorded::Stable(CaptureWindow {
            metadata: self.decode_metadata(before),
            capacity: CAPTURE_FRAMES,
            overwritten: first,
            frames,
        })
    }
}

/// Owned by the worker loop. Dropping it publishes incomplete retirement,
/// including unwinding; it never changes DSP/performance state.
pub(crate) struct Recorder<'a> {
    shared: &'a Capture,
    epoch: u32,
    state: CaptureState,
    armed_at: Option<u64>,
    trigger: Option<EventRecord>,
    recorded: u64,
    previous_sequence: Option<u64>,
    previous_published: Option<u64>,
}
impl<'a> Recorder<'a> {
    pub fn new(shared: &'a Capture) -> Self {
        Self {
            shared,
            epoch: 0,
            state: CaptureState::Armed,
            armed_at: None,
            trigger: None,
            recorded: 0,
            previous_sequence: None,
            previous_published: None,
        }
    }
    pub fn begin(&mut self, at_ns: Option<u64>) {
        let requested = self.shared.requested.load(Ordering::Acquire);
        if requested > self.epoch {
            self.epoch = requested;
            self.state = CaptureState::Armed;
            self.armed_at = at_ns;
            self.trigger = None;
            self.recorded = 0;
            self.publish();
            // Publish the timestamp before acknowledging the epoch. A miss
            // whose recording was delayed across rearm must not steal the latch.
            self.shared
                .armed_at
                .store(at_ns.unwrap_or(0), Ordering::Relaxed);
            self.shared.active.store(requested, Ordering::Release);
        }
    }
    fn observe_trigger(&mut self) {
        if self.trigger.is_none()
            && let Recorded::Stable([epoch, sequence, at_ns]) = self.shared.first_miss.read()
            && epoch == u64::from(self.epoch)
        {
            self.trigger = Some(EventRecord { sequence, at_ns });
            self.state = CaptureState::Triggered;
        }
    }
    pub fn record(&mut self, mut frame: CaptureFrame) {
        frame.previous_sequence = self.previous_sequence;
        frame.previous_published_ns = self.previous_published;
        self.previous_sequence = Some(frame.sequence);
        self.previous_published = frame.published_ns;
        if self.epoch == 0 || self.state == CaptureState::Frozen {
            return;
        }
        self.observe_trigger();
        self.shared.frames[self.recorded as usize % CAPTURE_FRAMES]
            .write(frame.words(self.epoch, self.recorded));
        self.recorded += 1;
        if self.trigger.is_some_and(|event| {
            frame.sequence >= event.sequence.saturating_add(CAPTURE_POST_SEQUENCES)
        }) {
            self.state = CaptureState::Frozen;
        }
        self.publish();
    }
    fn publish(&self) {
        let event = self.trigger.unwrap_or_default();
        self.shared.metadata.write([
            self.epoch.into(),
            self.state.word(),
            self.recorded,
            self.armed_at.unwrap_or(0),
            event.sequence,
            event.at_ns,
            u64::from(self.armed_at.is_some()) | (u64::from(self.trigger.is_some()) << 1),
        ]);
    }
}
impl Drop for Recorder<'_> {
    fn drop(&mut self) {
        if self.epoch != 0 && self.state != CaptureState::Frozen {
            self.observe_trigger();
            self.state = CaptureState::Stopped;
            self.publish();
        }
        self.shared.active.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
