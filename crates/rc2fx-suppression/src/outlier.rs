//! The worker's slowest and latest slow frames and the endpoint's latest
//! missed and late deadlines, for placing an outlier in a measurement window,
//! plus the endpoint's first missed and late deadlines in order.
//! Each record has exactly one writer and is a sequence lock over atomic
//! words: no allocation, no lock, and a reader that gives up after a few
//! tries rather than wait for a writer.
use std::sync::atomic::{AtomicU64, Ordering, fence};

/// One frame through the worker. Times are CLOCK_MONOTONIC (wall) or the
/// worker's thread CPU clock (`cpu_ns`), in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameRecord {
    pub sequence: u64,
    pub submitted_ns: u64,
    /// Dequeued and started by the worker.
    pub started_ns: u64,
    /// Model finished.
    pub finished_ns: u64,
    /// Result published to the endpoint.
    pub published_ns: u64,
    pub cpu_ns: u64,
    pub wall_ns: u64,
    pub queue_wait_ns: u64,
    pub turnaround_ns: u64,
}
impl FrameRecord {
    fn words(self) -> [u64; 9] {
        [
            self.sequence,
            self.submitted_ns,
            self.started_ns,
            self.finished_ns,
            self.published_ns,
            self.cpu_ns,
            self.wall_ns,
            self.queue_wait_ns,
            self.turnaround_ns,
        ]
    }
    fn from_words(w: [u64; 9]) -> Self {
        Self {
            sequence: w[0],
            submitted_ns: w[1],
            started_ns: w[2],
            finished_ns: w[3],
            published_ns: w[4],
            cpu_ns: w[5],
            wall_ns: w[6],
            queue_wait_ns: w[7],
            turnaround_ns: w[8],
        }
    }
}
/// A deadline event at the endpoint: the frame it concerned and when
/// (CLOCK_MONOTONIC ns; 0 if the clock could not be read).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventRecord {
    pub sequence: u64,
    pub at_ns: u64,
}

/// What a reader got from a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded<T> {
    /// Never written.
    Empty,
    Stable(T),
    /// The writer was mid-update on every try; read again later.
    Busy,
}

/// Retries before a reader reports `Busy`.
const READ_TRIES: usize = 4;

/// A single-writer sequence lock over `N` words. The version is even when
/// stable and odd while written; zero means never written.
pub(crate) struct Slot<const N: usize> {
    version: AtomicU64,
    words: [AtomicU64; N],
}
impl<const N: usize> Default for Slot<N> {
    fn default() -> Self {
        Self {
            version: AtomicU64::new(0),
            words: [const { AtomicU64::new(0) }; N],
        }
    }
}
impl<const N: usize> Slot<N> {
    /// Only the slot's one writer may call this.
    pub fn write(&self, words: [u64; N]) {
        let version = self.version.load(Ordering::Relaxed);
        self.version
            .store(version.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd version before every word store below.
        fence(Ordering::Release);
        for (slot, word) in self.words.iter().zip(words) {
            slot.store(word, Ordering::Relaxed);
        }
        // Publishes the words with the next even version. Wrapping skips 0,
        // which means empty: 2^63 writes away.
        let next = version.wrapping_add(2);
        self.version
            .store(if next == 0 { 2 } else { next }, Ordering::Release);
    }
    /// The words as of the writer's last completed write, if it got one.
    pub fn read(&self) -> Recorded<[u64; N]> {
        for _ in 0..READ_TRIES {
            let before = self.version.load(Ordering::Acquire);
            if before == 0 {
                return Recorded::Empty;
            }
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let words = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
            // Orders the word loads before the version re-check.
            fence(Ordering::Acquire);
            if self.version.load(Ordering::Relaxed) == before {
                return Recorded::Stable(words);
            }
        }
        Recorded::Busy
    }
}

/// Turnaround thresholds, in nanoseconds, counted cumulatively per worker:
/// how many frames took longer than this from submission to publication.
pub const TURNAROUND_THRESHOLDS_NS: [u64; 4] = [5_500_000, 6_000_000, 6_500_000, 7_000_000];

/// Written by the worker thread only.
#[derive(Default)]
pub(crate) struct WorkerOutliers {
    worst: Slot<9>,
    /// Turnaround of `worst`, so the writer need not read its own slot.
    worst_ns: AtomicU64,
    latest_slow: Slot<9>,
    slow: AtomicU64,
    /// Frames above each of `TURNAROUND_THRESHOLDS_NS`, since start.
    above: [AtomicU64; TURNAROUND_THRESHOLDS_NS.len()],
}
impl WorkerOutliers {
    /// `slow_ns`: a turnaround above this is slow (the frame budget).
    pub fn record(&self, frame: FrameRecord, slow_ns: u64) {
        if frame.turnaround_ns > self.worst_ns.load(Ordering::Relaxed) {
            self.worst_ns.store(frame.turnaround_ns, Ordering::Relaxed);
            self.worst.write(frame.words());
        }
        for (count, threshold) in self.above.iter().zip(TURNAROUND_THRESHOLDS_NS) {
            if frame.turnaround_ns > threshold {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        if frame.turnaround_ns > slow_ns {
            self.slow.fetch_add(1, Ordering::Relaxed);
            self.latest_slow.write(frame.words());
        }
    }
}
/// Events each history keeps.
pub const HISTORY: usize = 64;
/// The first HISTORY events of one kind, in order and never overwritten;
/// later ones are only counted. One writer; a record is published (its index
/// + 1) after its words, and the total after that.
pub(crate) struct History {
    total: AtomicU64,
    published: [AtomicU64; HISTORY],
    words: [[AtomicU64; 2]; HISTORY],
}
impl Default for History {
    fn default() -> Self {
        Self {
            total: AtomicU64::new(0),
            published: [const { AtomicU64::new(0) }; HISTORY],
            words: [const { [const { AtomicU64::new(0) }; 2] }; HISTORY],
        }
    }
}
impl History {
    /// Only the history's one writer may call this.
    pub fn record(&self, event: EventRecord) {
        let index = self.total.load(Ordering::Relaxed);
        if let Some(words) = self.words.get(index as usize) {
            words[0].store(event.sequence, Ordering::Relaxed);
            words[1].store(event.at_ns, Ordering::Relaxed);
            self.published[index as usize].store(index + 1, Ordering::Release);
        }
        self.total.store(index + 1, Ordering::Release);
    }
    fn snapshot(&self) -> EventHistory {
        let total = self.total.load(Ordering::Acquire);
        let mut history = EventHistory {
            total,
            capacity: HISTORY,
            events: Vec::new(),
            lost: total.saturating_sub(HISTORY as u64),
            unpublished: 0,
        };
        for (index, (published, words)) in self.published.iter().zip(&self.words).enumerate() {
            if index as u64 >= total {
                break;
            }
            if published.load(Ordering::Acquire) == index as u64 + 1 {
                history.events.push(EventRecord {
                    sequence: words[0].load(Ordering::Relaxed),
                    at_ns: words[1].load(Ordering::Relaxed),
                });
            } else {
                history.unpublished += 1;
            }
        }
        history
    }
}
/// What a history held when read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventHistory {
    /// Every event since the endpoint started.
    pub total: u64,
    pub capacity: usize,
    /// The first `capacity` events, in order.
    pub events: Vec<EventRecord>,
    /// Events beyond capacity: counted, not kept.
    pub lost: u64,
    /// Counted but not yet readable (expected 0: the total is published
    /// last).
    pub unpublished: u64,
}
impl EventHistory {
    /// Every event kept, readable and with a time (an `at_ns` of 0 is a
    /// failed clock read, not a time).
    pub fn complete(&self) -> bool {
        self.lost == 0 && self.unpublished == 0 && self.events.iter().all(|e| e.at_ns != 0)
    }
}
/// Written by the endpoint (the audio callback) only, and only on a miss or
/// late result.
#[derive(Default)]
pub(crate) struct EndpointOutliers {
    pub miss: Slot<2>,
    pub late: Slot<2>,
    pub miss_history: History,
    pub late_history: History,
}
impl EndpointOutliers {
    pub fn missed(&self, event: EventRecord) {
        self.miss.write([event.sequence, event.at_ns]);
        self.miss_history.record(event);
    }
    pub fn late(&self, event: EventRecord) {
        self.late.write([event.sequence, event.at_ns]);
        self.late_history.record(event);
    }
}

/// A snapshot for the control side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outliers {
    /// The frame with the longest turnaround since the worker started.
    pub worst: Recorded<FrameRecord>,
    /// The latest frame whose turnaround exceeded the frame budget.
    pub latest_slow: Recorded<FrameRecord>,
    pub slow_frames: u64,
    /// Frames whose turnaround exceeded each `TURNAROUND_THRESHOLDS_NS`
    /// entry, cumulative since the worker started.
    pub above: [u64; TURNAROUND_THRESHOLDS_NS.len()],
    pub last_miss: Recorded<EventRecord>,
    pub last_late: Recorded<EventRecord>,
    pub miss_history: EventHistory,
    pub late_history: EventHistory,
}
pub(crate) fn snapshot(worker: &WorkerOutliers, endpoint: &EndpointOutliers) -> Outliers {
    let frame = |slot: &Slot<9>| match slot.read() {
        Recorded::Stable(words) => Recorded::Stable(FrameRecord::from_words(words)),
        Recorded::Empty => Recorded::Empty,
        Recorded::Busy => Recorded::Busy,
    };
    let event = |slot: &Slot<2>| match slot.read() {
        Recorded::Stable([sequence, at_ns]) => Recorded::Stable(EventRecord { sequence, at_ns }),
        Recorded::Empty => Recorded::Empty,
        Recorded::Busy => Recorded::Busy,
    };
    Outliers {
        worst: frame(&worker.worst),
        latest_slow: frame(&worker.latest_slow),
        slow_frames: worker.slow.load(Ordering::Relaxed),
        above: std::array::from_fn(|i| worker.above[i].load(Ordering::Relaxed)),
        last_miss: event(&endpoint.miss),
        last_late: event(&endpoint.late),
        miss_history: endpoint.miss_history.snapshot(),
        late_history: endpoint.late_history.snapshot(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn empty_then_stable_and_worst_keeps_the_maximum() {
        let worker = WorkerOutliers::default();
        let endpoint = EndpointOutliers::default();
        let empty = snapshot(&worker, &endpoint);
        assert_eq!(empty.worst, Recorded::Empty);
        assert_eq!(empty.latest_slow, Recorded::Empty);
        assert_eq!(empty.last_miss, Recorded::Empty);
        assert_eq!(empty.slow_frames, 0);
        assert_eq!(empty.above, [0; 4]);
        let frame = |sequence, turnaround_ns| FrameRecord {
            sequence,
            turnaround_ns,
            submitted_ns: 1000 * sequence,
            ..FrameRecord::default()
        };
        worker.record(frame(1, 3_000_000), 5_000_000);
        worker.record(frame(2, 7_000_000), 5_000_000);
        worker.record(frame(3, 6_000_000), 5_000_000);
        worker.record(frame(4, 2_000_000), 5_000_000);
        let got = snapshot(&worker, &endpoint);
        assert_eq!(got.worst, Recorded::Stable(frame(2, 7_000_000)));
        assert_eq!(got.latest_slow, Recorded::Stable(frame(3, 6_000_000)));
        assert_eq!(got.slow_frames, 2);
        // Of 3.0, 7.0, 6.0 and 2.0 ms: 7.0 and 6.0 are above 5.5, only 7.0
        // is above 6.0 and 6.5, and none is above 7.0.
        assert_eq!(got.above, [2, 1, 1, 0]);
        // The thresholds are strict: exactly at one does not count, but each
        // frame at or above a later threshold is above every earlier one.
        let boundary = WorkerOutliers::default();
        for turnaround in TURNAROUND_THRESHOLDS_NS {
            boundary.record(frame(5, turnaround), 5_000_000);
            boundary.record(frame(6, turnaround + 1), 5_000_000);
        }
        assert_eq!(snapshot(&boundary, &endpoint).above, [7, 5, 3, 1]);
        endpoint.miss.write([9, 123]);
        assert_eq!(
            snapshot(&worker, &endpoint).last_miss,
            Recorded::Stable(EventRecord {
                sequence: 9,
                at_ns: 123
            })
        );
    }

    #[test]
    fn a_history_keeps_the_first_events_and_counts_the_rest() {
        let history = History::default();
        assert_eq!(
            history.snapshot(),
            EventHistory {
                capacity: HISTORY,
                ..Default::default()
            }
        );
        let event = |sequence| EventRecord {
            sequence,
            at_ns: 10 * sequence + 1,
        };
        for sequence in 0..HISTORY as u64 + 5 {
            history.record(event(sequence));
        }
        let got = history.snapshot();
        assert_eq!(
            (got.total, got.lost, got.unpublished),
            (HISTORY as u64 + 5, 5, 0)
        );
        assert_eq!(
            got.total,
            got.events.len() as u64 + got.lost + got.unpublished
        );
        assert_eq!(
            got.events,
            (0..HISTORY as u64).map(event).collect::<Vec<_>>()
        );
        assert!(!got.complete(), "events were lost");
    }

    #[test]
    fn an_event_without_a_time_is_kept_and_makes_the_history_incomplete() {
        let history = History::default();
        history.record(EventRecord {
            sequence: 3,
            at_ns: 5,
        });
        assert!(history.snapshot().complete());
        history.record(EventRecord {
            sequence: 4,
            at_ns: 0,
        });
        let got = history.snapshot();
        assert_eq!(
            got.events[1],
            EventRecord {
                sequence: 4,
                at_ns: 0
            }
        );
        assert!(!got.complete());
    }

    #[test]
    fn a_counted_record_not_yet_published_is_unpublished() {
        let history = History::default();
        history.record(EventRecord {
            sequence: 1,
            at_ns: 2,
        });
        // As if a second record were counted before its words were published.
        history.total.store(2, Ordering::Release);
        let got = history.snapshot();
        assert_eq!((got.total, got.events.len(), got.unpublished), (2, 1, 1));
    }

    /// A reader during writes sees a prefix of the writes, in order, with no
    /// torn or unpublished record counted as an event.
    #[test]
    fn concurrent_history_reads_are_ordered_prefixes() {
        let history = Arc::new(History::default());
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (history, done) = (Arc::clone(&history), Arc::clone(&done));
            std::thread::spawn(move || {
                for sequence in 0..10_000u64 {
                    history.record(EventRecord {
                        sequence,
                        at_ns: sequence + 1,
                    });
                }
                done.store(true, Ordering::Release);
            })
        };
        while !done.load(Ordering::Acquire) {
            let got = history.snapshot();
            assert_eq!(got.unpublished, 0);
            assert_eq!(got.events.len() as u64, got.total.min(HISTORY as u64));
            for (index, event) in got.events.iter().enumerate() {
                assert_eq!(
                    (event.sequence, event.at_ns),
                    (index as u64, index as u64 + 1)
                );
            }
        }
        writer.join().unwrap();
        let got = history.snapshot();
        assert_eq!((got.total, got.lost), (10_000, 10_000 - HISTORY as u64));
    }

    #[test]
    fn a_writer_mid_update_reads_busy_not_torn() {
        let slot = Slot::<2>::default();
        slot.write([1, 1]);
        // Odd version: as if the writer were between its two stores.
        slot.version.fetch_add(1, Ordering::Relaxed);
        assert_eq!(slot.read(), Recorded::Busy);
    }

    #[test]
    fn the_version_never_wraps_to_empty() {
        let slot = Slot::<1>::default();
        slot.version.store(u64::MAX - 1, Ordering::Relaxed);
        slot.write([5]);
        assert_eq!(slot.read(), Recorded::Stable([5]));
        slot.write([6]);
        assert_eq!(slot.read(), Recorded::Stable([6]));
    }

    /// Every word of a record is written with the same value; a reader must
    /// only ever see records whose words agree.
    #[test]
    fn concurrent_reads_are_never_torn() {
        let slot = Arc::new(Slot::<9>::default());
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (slot, done) = (Arc::clone(&slot), Arc::clone(&done));
            std::thread::spawn(move || {
                for value in 1..=200_000u64 {
                    slot.write([value; 9]);
                }
                done.store(true, Ordering::Release);
            })
        };
        let (mut stable, mut last) = (0u64, 0u64);
        while !done.load(Ordering::Acquire) {
            if let Recorded::Stable(words) = slot.read() {
                assert!(words.iter().all(|&w| w == words[0]), "torn {words:?}");
                assert!(words[0] >= last, "went backwards");
                last = words[0];
                stable += 1;
            }
        }
        writer.join().unwrap();
        assert_eq!(slot.read(), Recorded::Stable([200_000; 9]));
        assert!(stable > 0);
    }
}
