//! Bounded late-callback history. Writers never wait; readers reject changing
//! slots. Sequentially consistent atomics keep every accepted row coherent.
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
pub type Events = History<32>;
pub type MainEvents = History<256>;
struct Row {
    version: AtomicU64,
    values: [AtomicU64; 7],
}
pub struct History<const CAPACITY: usize> {
    next: AtomicU64,
    dropped: AtomicU64,
    rows: [Row; CAPACITY],
}
impl<const CAPACITY: usize> History<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            next: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            rows: [const {
                Row {
                    version: AtomicU64::new(0),
                    values: [const { AtomicU64::new(0) }; 7],
                }
            }; CAPACITY],
        }
    }
    pub fn record(&self, start: u64, end: u64, cycle: u64, budget: u64) {
        self.record_tagged(start, end, cycle, budget, 0);
    }
    pub fn record_tagged(&self, start: u64, end: u64, cycle: u64, budget: u64, tag: u64) {
        self.record_partitioned([start, end, cycle, budget, tag], None);
    }
    pub fn record_partitioned(&self, timing: [u64; 5], native: Option<[u64; 2]>) {
        self.record_inner(timing, native, true);
    }
    pub fn record_all(&self, timing: [u64; 5]) {
        self.record_all_partitioned(timing, None);
    }
    pub fn record_all_partitioned(&self, timing: [u64; 5], native: Option<[u64; 2]>) {
        self.record_inner(timing, native, false);
    }
    fn record_inner(&self, timing: [u64; 5], native: Option<[u64; 2]>, only_late: bool) {
        let [start, end, cycle, budget, tag] = timing;
        let [before, after] = native
            .filter(|[before, after]| start <= *before && before <= after && *after <= end)
            .unwrap_or([0, 0]);
        if start == 0 || end < start || end == 0 {
            return;
        }
        if only_late && end - start <= budget && (cycle == 0 || end.saturating_sub(cycle) <= budget)
        {
            return;
        }
        let index = self.next.fetch_add(1, SeqCst);
        let row = &self.rows[index as usize % CAPACITY];
        let version = row.version.load(SeqCst);
        // Refuse busy or newer writers. No spin, and no stale writer can erase
        // a later publication after it has acquired this row.
        if !version.is_multiple_of(2)
            || version > index * 2
            || row
                .version
                .compare_exchange(version, index * 2 + 1, SeqCst, SeqCst)
                .is_err()
        {
            self.dropped.fetch_add(1, SeqCst);
            return;
        }
        for (dst, value) in row
            .values
            .iter()
            .zip([start, end, cycle, budget, tag, before, after])
        {
            dst.store(value, SeqCst);
        }
        row.version.store(index * 2 + 2, SeqCst);
    }
    pub fn snapshot(&self) -> serde_json::Value {
        let total = self.next.load(SeqCst);
        let first = total.saturating_sub(CAPACITY as u64);
        let mut rows = Vec::new();
        for index in first..total {
            let row = &self.rows[index as usize % CAPACITY];
            let expected = index * 2 + 2;
            if row.version.load(SeqCst) != expected {
                continue;
            }
            let [start, end, cycle, budget, tag, before, after] =
                std::array::from_fn(|i| row.values[i].load(SeqCst));
            if row.version.load(SeqCst) != expected {
                continue;
            }
            rows.push(
                serde_json::json!({"sequence":index,"startNs":start,"endNs":end,
                "wallNs":end-start,"estimatedCycleStartNs":(cycle != 0).then_some(cycle),
                "thresholdNs":budget,"registrationTag":tag,
                "nativeStartNs":(before != 0).then_some(before),
                "nativeEndNs":(after != 0).then_some(after)}),
            );
        }
        serde_json::json!({"total":total,"capacity":CAPACITY,"outsideWindow":first,
            "dropped":self.dropped.load(SeqCst),"unavailableInWindow":total-first-rows.len() as u64,
            "meaning":"exceeds local cycle-position or wall threshold; not a JACK failure assertion", "events":rows})
    }
    pub fn compact(&self, token: u64) -> serde_json::Value {
        let snapshot = self.snapshot();
        let rows: Vec<_> = snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["registrationTag"] == token)
            .map(|row| {
                serde_json::json!([
                    row["startNs"],
                    row["endNs"],
                    row["estimatedCycleStartNs"],
                    row["thresholdNs"]
                ])
            })
            .collect();
        serde_json::json!({"rows": rows, "dropped": snapshot["dropped"],
            "unavailableInWindow": snapshot["unavailableInWindow"], "columns": ["startNs", "endNs", "estimatedCycleStartNs", "budgetNs"]})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flight_history_keeps_fast_callbacks_and_filters_old_arm_tokens() {
        let events = Events::new();
        events.record_all([100, 101, 90, 100, 1]);
        events.record_all([200, 201, 190, 100, 2]);
        let value = events.compact(2);
        assert_eq!(value["rows"], serde_json::json!([[200, 201, 190, 100]]));
        assert_eq!(value["unavailableInWindow"], 0);
    }
    #[test]
    fn partitions_are_optional_and_must_belong_to_the_callback() {
        let events = Events::new();
        events.record_partitioned([100, 120, 90, 10, 7], Some([105, 115]));
        let value = events.snapshot();
        assert_eq!(value["events"][0]["nativeStartNs"], 105);
        assert_eq!(value["events"][0]["nativeEndNs"], 115);
        assert_eq!(value["events"][0]["registrationTag"], 7);
        for pair in [[99, 115], [110, 105], [105, 121]] {
            events.record_partitioned([100, 120, 90, 10, 7], Some(pair));
        }
        events.record(100, 120, 90, 10);
        for row in &events.snapshot()["events"].as_array().unwrap()[1..] {
            assert!(row["nativeStartNs"].is_null());
            assert!(row["nativeEndNs"].is_null());
        }
    }
    #[test]
    fn concurrent_readers_never_accept_mixed_rows() {
        let events = std::sync::Arc::new(Events::new());
        let writer = events.clone();
        let thread = std::thread::spawn(move || {
            for start in 1..20_000 {
                writer.record(start, start + 20, start, 10);
            }
        });
        while !thread.is_finished() {
            for row in events.snapshot()["events"].as_array().unwrap() {
                assert_eq!(
                    row["endNs"].as_u64().unwrap() - row["startNs"].as_u64().unwrap(),
                    20
                );
                assert_eq!(row["estimatedCycleStartNs"], row["startNs"]);
            }
        }
        thread.join().unwrap();
    }
    #[test]
    fn distinguishes_long_execution_from_late_entry_and_reports_overwrite() {
        let events = Events::new();
        events.record(0, 20, 1, 10);
        events.record(1, 2, 0, 10);
        assert_eq!(events.snapshot()["total"], 0);
        events.record(100, 102, 90, 10);
        let value = events.snapshot();
        assert_eq!(value["events"][0]["wallNs"], 2);
        assert_eq!(value["events"][0]["estimatedCycleStartNs"], 90);
        for _ in 0..40 {
            events.record(100, 120, 0, 10);
        }
        let value = events.snapshot();
        assert_eq!(value["total"], 41);
        assert_eq!(value["outsideWindow"], 9);
        assert_eq!(value["events"].as_array().unwrap().len(), 32);
        assert!(value["events"][0]["estimatedCycleStartNs"].is_null());
        assert_eq!(value["unavailableInWindow"], 0);
    }
}
