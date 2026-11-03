//! Callback-to-control fault and JACK xrun counters. Storage is allocated before
//! callback registration; callbacks only access the existing atomic fields.
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub fault: u32,
    pub xruns: u64,
}
#[derive(Default)]
struct Counters {
    fault: AtomicU32,
    xruns: AtomicU64,
}
#[derive(Clone)]
pub struct Reader(Arc<Counters>);
pub struct Writer(Arc<Counters>);
pub fn channel() -> (Writer, Reader) {
    let counters = Arc::new(Counters::default());
    (Writer(Arc::clone(&counters)), Reader(counters))
}
impl Reader {
    /// Individually atomic live counters, not a transaction. Exact totals are
    /// available after the native callbacks have quiesced.
    pub fn snapshot(&self) -> Snapshot {
        self.0.snapshot()
    }
}
impl Counters {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            fault: self.fault.load(Ordering::Acquire),
            xruns: self.xruns.load(Ordering::Relaxed),
        }
    }
}
impl Writer {
    pub fn reader(&self) -> Reader {
        Reader(Arc::clone(&self.0))
    }
    pub fn snapshot(&self) -> Snapshot {
        self.0.snapshot()
    }
    /// No callback clears a fault. The generation owner replaces the entire
    /// channel only after ending this generation.
    pub fn fault(&self, code: u32) {
        if code != 0 {
            self.0.fault.store(code, Ordering::Release);
        }
    }
    pub fn xrun(&self) {
        self.0.xruns.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readers_outlive_native_writer_without_resetting_fault_or_counts() {
        let (writer, reader) = channel();
        assert_eq!(reader.snapshot(), Snapshot::default());
        writer.xrun();
        writer.xrun();
        writer.fault(3);
        writer.fault(0);
        let another = writer.reader();
        drop(writer);
        assert_eq!(reader.snapshot(), Snapshot { fault: 3, xruns: 2 });
        assert_eq!(another.snapshot(), reader.snapshot());
    }
}
