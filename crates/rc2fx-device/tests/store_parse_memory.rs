//! Requested allocator-byte comparison, not RSS or target performance. Both
//! paths run the same strict guard; the reference intentionally retains Value.
use serde::Deserialize;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Default)]
struct Counts {
    enabled: bool,
    live: usize,
    peak: usize,
    allocated: usize,
}
thread_local! { static COUNTS: Cell<Counts> = const { Cell::new(Counts { enabled:false, live:0, peak:0, allocated:0 }) }; }
struct CountingAllocator;
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
fn allocation(size: usize) {
    let _ = COUNTS.try_with(|slot| {
        let mut counts = slot.get();
        if counts.enabled {
            counts.live += size;
            counts.allocated += size;
            counts.peak = counts.peak.max(counts.live);
            slot.set(counts);
        }
    });
}
fn release(size: usize) {
    let _ = COUNTS.try_with(|slot| {
        let mut counts = slot.get();
        if counts.enabled {
            counts.live -= size;
            slot.set(counts);
        }
    });
}
// SAFETY: every operation delegates unchanged pointer/layout ownership to System;
// bookkeeping is a nonallocating thread-local Cell and never changes the result.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocation(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocation(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        release(layout.size());
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            release(layout.size());
            allocation(size);
        }
        result
    }
}
fn measured(f: impl FnOnce()) -> Counts {
    COUNTS.with(|slot| {
        slot.set(Counts {
            enabled: true,
            ..Counts::default()
        })
    });
    f();
    COUNTS.with(|slot| {
        let mut counts = slot.get();
        counts.enabled = false;
        slot.set(counts);
        counts
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    #[serde(rename = "engineGeneration")]
    engine_generation: u64,
    config: rc2fx_core::Config,
    presets: Vec<Preset>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preset {
    id: String,
    name: String,
    targets: Vec<rc2fx_core::Target>,
}
fn consume(saved: Saved) {
    std::hint::black_box(saved.engine_generation);
    std::hint::black_box(&saved.config);
    for preset in &saved.presets {
        std::hint::black_box((&preset.id, &preset.name, &preset.targets));
    }
    drop(saved);
}

#[test]
fn typed_saved_state_does_not_retain_a_full_untyped_tree() {
    let config = rc2fx_core::Config::default();
    let presets: Vec<_> = (0..128).map(|index| serde_json::json!({"id":format!("p-{index}"),"name":"Preset","targets":config.targets})).collect();
    let text =
        serde_json::json!({"engineGeneration":0,"config":config,"presets":presets}).to_string();
    let maximum = rc2fx_device::store::MAX_STATE_BYTES;
    let tree = measured(|| {
        let value =
            rc2fx_core::from_json_limited::<serde_json::Value>(&text, maximum, 4_000_000).unwrap();
        consume(serde_json::from_value(value).unwrap());
    });
    let direct = measured(|| {
        consume(rc2fx_core::from_json_limited::<Saved>(&text, maximum, 4_000_000).unwrap())
    });
    assert_eq!(tree.live, 0);
    assert_eq!(direct.live, 0);
    assert!(direct.peak < tree.peak / 2);
    println!(
        "{{\"inputBytes\":{},\"treePeakRequestedBytes\":{},\"typedPeakRequestedBytes\":{},\"treeAllocatedBytes\":{},\"typedAllocatedBytes\":{}}}",
        text.len(),
        tree.peak,
        direct.peak,
        tree.allocated,
        direct.allocated
    );
}
