//! Test-only memory-traffic accounting: the bytes each stage of one `process`
//! call reads and writes, by class, as the mixer's AArch64 int8 kernels lay
//! them out (on any host). Compiled only into tests; production builds have
//! no trace of it. Counting is off unless the calling thread turns it on.
//!
//! `total` is every byte a kernel loads or stores, re-reads included (most hit
//! L1). `unique` counts each buffer once per call, charged to the first stage
//! that touches it: with no reuse across calls (the int8 weights are larger
//! than the shared L2), that is the most a call can need from DRAM.
use std::cell::RefCell;
use std::collections::HashMap;

pub(crate) const STAGES: usize = 15;
pub(crate) const NAMES: [&str; STAGES] = [
    "features",
    "erb_conv0",
    "erb_conv1-3",
    "df_conv0+pw",
    "df_conv1",
    "df_fc_emb",
    "enc_gru+lsnr",
    "erb_gru",
    "erb_pathways+convt",
    "erb_out",
    "df_gru+skip",
    "df_out",
    "df_convp",
    "df_filter",
    "apply+synthesis",
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Class {
    /// int8 weights.
    Q8,
    /// The f32 scales of the int8 weights.
    Scale,
    /// Small f32 parameters: conv taps, norms, biases.
    Param,
    /// Activations and scratch.
    Act,
    /// State kept across calls: GRU hidden states, feature and spectrum history.
    State,
}
pub(crate) const CLASSES: [Class; 5] = [
    Class::Q8,
    Class::Scale,
    Class::Param,
    Class::Act,
    Class::State,
];

/// One access in program order, for the cache model: buffer, bytes from
/// its start, store or load.
#[derive(Clone, Copy)]
pub(crate) struct Access {
    pub(crate) key: (Class, usize),
    pub(crate) bytes: u32,
    pub(crate) times: u32,
    pub(crate) write: bool,
    pub(crate) stage: u8,
    pub(crate) call: u32,
}

#[derive(Default)]
struct Counts {
    on: bool,
    stage: usize,
    calls: u64,
    total: [[u64; 5]; STAGES],
    unique: [[u64; 5]; STAGES],
    /// This call's buffers: (class, address) -> largest span counted.
    seen: HashMap<(Class, usize), u64>,
    trace: Vec<Access>,
}

thread_local! { static COUNTS: RefCell<Counts> = RefCell::new(Counts::default()); }

fn index(class: Class) -> usize {
    CLASSES.iter().position(|&c| c == class).unwrap()
}

/// Turns counting on for this thread and clears it.
pub(crate) fn enable() {
    COUNTS.with(|c| {
        *c.borrow_mut() = Counts {
            on: true,
            ..Counts::default()
        }
    });
}

/// A new `process` call: uniqueness restarts.
pub(crate) fn start() {
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.on {
            c.stage = 0;
            c.calls += 1;
            c.seen.clear();
        }
    });
}

/// `profile::mark(stage)` closed `stage`; what follows belongs to the next.
pub(crate) fn mark(stage: usize) {
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.on {
            c.stage = (stage + 1).min(STAGES - 1);
        }
    });
}

/// Loads of `bytes` at `at`, `times` times over.
pub(crate) fn touch_n<T: ?Sized>(class: Class, at: *const T, bytes: usize, times: usize) {
    access(class, at, bytes, times, false);
}

/// Stores of `bytes` at `at`, `times` times over.
pub(crate) fn store_n<T: ?Sized>(class: Class, at: *const T, bytes: usize, times: usize) {
    access(class, at, bytes, times, true);
}

pub(crate) fn store<T: ?Sized>(class: Class, at: *const T, bytes: usize) {
    store_n(class, at, bytes, 1);
}

fn access<T: ?Sized>(class: Class, at: *const T, bytes: usize, times: usize, write: bool) {
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if !c.on || bytes == 0 || times == 0 {
            return;
        }
        let (s, k) = (c.stage, index(class));
        let access = Access {
            key: (class, at as *const u8 as usize),
            bytes: bytes as u32,
            times: times as u32,
            write,
            stage: s as u8,
            call: c.calls as u32,
        };
        c.trace.push(access);
        c.total[s][k] += (bytes * times) as u64;
        let key = (class, at as *const u8 as usize);
        let before = c.seen.get(&key).copied().unwrap_or(0);
        if (bytes as u64) > before {
            c.unique[s][k] += bytes as u64 - before;
            c.seen.insert(key, bytes as u64);
        }
    });
}

pub(crate) fn touch<T: ?Sized>(class: Class, at: *const T, bytes: usize) {
    touch_n(class, at, bytes, 1);
}

/// A slice's bytes, once.
pub(crate) fn slice<T>(class: Class, s: &[T]) {
    touch(class, s.as_ptr(), std::mem::size_of_val(s));
}

/// The accesses since `enable`, in order.
pub(crate) fn trace() -> Vec<Access> {
    COUNTS.with(|c| c.borrow().trace.clone())
}

/// DRAM traffic of `trace` through one write-back, write-allocate LRU cache
/// of `size` bytes, `ways`-way, 64-byte lines. Each buffer gets its own
/// address range (the largest span it was touched with), in first-touch
/// order. Returns per-call means over calls `from..`: line fills (reads from
/// DRAM) and write-backs, in bytes, [stage][class], each charged to the
/// stage that caused it.
pub(crate) fn simulate(
    trace: &[Access],
    size: usize,
    ways: usize,
    from: u32,
) -> ([[f64; 5]; STAGES], [[f64; 5]; STAGES]) {
    simulate_with(trace, size, ways, from, false)
}

/// `simulate`, evicting a pseudo-random way instead of the least recent.
pub(crate) fn simulate_random(
    trace: &[Access],
    size: usize,
    ways: usize,
    from: u32,
) -> ([[f64; 5]; STAGES], [[f64; 5]; STAGES]) {
    simulate_with(trace, size, ways, from, true)
}

fn simulate_with(
    trace: &[Access],
    size: usize,
    ways: usize,
    from: u32,
    random: bool,
) -> ([[f64; 5]; STAGES], [[f64; 5]; STAGES]) {
    simulate_shared(trace, size, ways, from, random, 0)
}

/// `simulate_with`, with another thread's `foreign` bytes (its own fixed
/// lines, stored to) passing through the cache before each call: the
/// mixer's callback and the other threads sharing the L2. Their own fills
/// and write-backs are not counted; the model's refills they cause are.
pub(crate) fn simulate_shared(
    trace: &[Access],
    size: usize,
    ways: usize,
    from: u32,
    random: bool,
    foreign: usize,
) -> ([[f64; 5]; STAGES], [[f64; 5]; STAGES]) {
    simulate_layout(trace, size, ways, from, random, foreign, false)
}

/// `simulate_shared`; with `packed`, buffers sit back to back at 64-byte
/// alignment (as malloc packs small vectors) and each 4 KiB page maps to a
/// pseudo-random physical page, as the physically indexed L2 sees them.
/// Otherwise each buffer starts a page of its own and addresses are
/// physical as laid out.
pub(crate) fn simulate_layout(
    trace: &[Access],
    size: usize,
    ways: usize,
    from: u32,
    random: bool,
    foreign: usize,
    packed: bool,
) -> ([[f64; 5]; STAGES], [[f64; 5]; STAGES]) {
    let phys = |line: usize| {
        if !packed {
            return line;
        }
        let mut z = (line / 64) as u64 ^ 0x9e37_79b9_7f4a_7c15;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z % (1 << 18)) as usize * 64 + line % 64
    };
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    const LINE: usize = 64;
    let mut span: HashMap<(Class, usize), usize> = HashMap::new();
    let mut order = Vec::new();
    for a in trace {
        let e = span.entry(a.key).or_insert_with(|| {
            order.push(a.key);
            0
        });
        *e = (*e).max(a.bytes as usize);
    }
    let mut base = HashMap::new();
    let mut next = 0usize;
    for key in order {
        base.insert(key, next);
        next +=
            span[&key].div_ceil(if packed { 64 } else { 4096 }) * if packed { 64 } else { 4096 };
    }
    let sets = size / (LINE * ways);
    // Per set: (tag, dirty, class index), most recent last.
    let mut cache: Vec<Vec<(usize, bool, usize)>> = vec![Vec::with_capacity(ways); sets];
    let mut fills = [[0u64; 5]; STAGES];
    let mut backs = [[0u64; 5]; STAGES];
    let last = trace.last().map_or(0, |a| a.call);
    const FOREIGN: usize = usize::MAX;
    let foreign_base = next / LINE;
    let mut call = u32::MAX;
    for a in trace {
        if a.call != call && foreign > 0 {
            call = a.call;
            for line in foreign_base..foreign_base + foreign.div_ceil(LINE) {
                let line = phys(line);
                let set = &mut cache[line % sets];
                if let Some(i) = set.iter().position(|e| e.0 == line) {
                    let e = set.remove(i);
                    set.push((e.0, true, e.2));
                    continue;
                }
                if set.len() == ways {
                    let victim = if random {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        (seed % ways as u64) as usize
                    } else {
                        0
                    };
                    let (_, dirty, owner) = set.remove(victim);
                    if dirty && owner != FOREIGN && a.call >= from {
                        backs[a.stage as usize][owner] += LINE as u64;
                    }
                }
                set.push((line, true, FOREIGN));
            }
        }
        let counted = a.call >= from;
        let k = index(a.key.0);
        let start = base[&a.key] / LINE;
        let lines = (a.bytes as usize).div_ceil(LINE);
        for _ in 0..a.times {
            for line in start..start + lines {
                let line = phys(line);
                let set = &mut cache[line % sets];
                if let Some(i) = set.iter().position(|e| e.0 == line) {
                    let mut e = set.remove(i);
                    e.1 |= a.write;
                    set.push(e);
                    continue;
                }
                if set.len() == ways {
                    let victim = if random {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        (seed % ways as u64) as usize
                    } else {
                        0
                    };
                    let (_, dirty, owner) = set.remove(victim);
                    if dirty && counted && owner != FOREIGN {
                        backs[a.stage as usize][owner] += LINE as u64;
                    }
                }
                if counted {
                    fills[a.stage as usize][k] += LINE as u64;
                }
                set.push((line, a.write, k));
            }
        }
    }
    let n = (last + 1).saturating_sub(from).max(1) as f64;
    let mean = |a: [[u64; 5]; STAGES]| a.map(|row| row.map(|v| v as f64 / n));
    (mean(fills), mean(backs))
}

/// Per-call means since `enable`: (calls, total, unique), [stage][class] bytes.
pub(crate) fn report() -> (u64, [[f64; 5]; STAGES], [[f64; 5]; STAGES]) {
    COUNTS.with(|c| {
        let c = c.borrow();
        let n = c.calls.max(1) as f64;
        let mean = |a: &[[u64; 5]; STAGES]| a.map(|row| row.map(|v| v as f64 / n));
        (c.calls, mean(&c.total), mean(&c.unique))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-call byte budget of the embedded model, by stage and class.
    /// `cargo test -p rc2fx-dfn --release -- --ignored --nocapture per_call_traffic`.
    #[test]
    #[ignore]
    fn per_call_traffic() {
        let mut filter = crate::DeepFilter::new().unwrap();
        let mut frame = vec![0f32; filter.frame()];
        let mut phase = 0u32;
        let mut next = || {
            phase = phase.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (phase >> 9) as f32 / (1u32 << 23) as f32 - 0.5
        };
        // Warm the state, then count.
        for _ in 0..50 {
            frame.iter_mut().for_each(|v| *v = next() * 0.1);
            filter.process(&mut frame);
        }
        enable();
        for _ in 0..200 {
            frame.iter_mut().for_each(|v| *v = next() * 0.1);
            filter.process(&mut frame);
        }
        let (calls, total, unique) = report();
        assert_eq!(calls, 200);
        let hops = (filter.frame() / 240).max(1) as f64;
        println!("per call ({hops} hop(s)): bytes, unique (total incl. re-reads)");
        println!(
            "{:<20} {:>16} {:>16} {:>16} {:>16} {:>16} {:>12}",
            "stage", "weights", "scales", "params", "activations", "state", "unique"
        );
        let mut sum_u = [0f64; 5];
        let mut sum_t = [0f64; 5];
        for (s, name) in NAMES.iter().enumerate() {
            let cell = |k: usize| format!("{:.0} ({:.0})", unique[s][k], total[s][k]);
            println!(
                "{:<20} {:>16} {:>16} {:>16} {:>16} {:>16} {:>12.0}",
                name,
                cell(0),
                cell(1),
                cell(2),
                cell(3),
                cell(4),
                unique[s].iter().sum::<f64>()
            );
            for k in 0..5 {
                sum_u[k] += unique[s][k];
                sum_t[k] += total[s][k];
            }
        }
        println!(
            "{:<20} {:>16} {:>16} {:>16} {:>16} {:>16} {:>12.0}",
            "TOTAL",
            format!("{:.0} ({:.0})", sum_u[0], sum_t[0]),
            format!("{:.0} ({:.0})", sum_u[1], sum_t[1]),
            format!("{:.0} ({:.0})", sum_u[2], sum_t[2]),
            format!("{:.0} ({:.0})", sum_u[3], sum_t[3]),
            format!("{:.0} ({:.0})", sum_u[4], sum_t[4]),
            sum_u.iter().sum::<f64>()
        );
        println!(
            "unique per hop: {:.0} bytes",
            sum_u.iter().sum::<f64>() / hops
        );
        assert!(sum_u[0] > 0.);
        // The cache model over the last 150 calls (the first 50 warm it).
        let trace = trace();
        // The largest non-weight buffers: span, first stage, accesses per call.
        let mut per: HashMap<(Class, usize), (usize, u8, u64, u64)> = HashMap::new();
        for a in trace.iter().filter(|a| a.call == 100) {
            let e = per.entry(a.key).or_insert((0, a.stage, 0, 0));
            e.0 = e.0.max(a.bytes as usize);
            e.2 += (a.bytes as u64) * a.times as u64;
            e.3 += a.write as u64;
        }
        let mut rows: Vec<_> = per
            .iter()
            .filter(|(k, _)| !matches!(k.0, Class::Q8 | Class::Scale))
            .collect();
        rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.0));
        println!(
            "largest non-weight buffers (one call): class, first stage, span bytes, bytes moved, store calls"
        );
        for ((class, _), (span, stage, moved, stores)) in rows.iter().take(14) {
            println!(
                "    {:<7} {:<20} {:>7} {:>8} {:>3}",
                format!("{class:?}"),
                NAMES[*stage as usize],
                span,
                moved,
                stores
            );
        }
        for size in [64 << 10, 128 << 10, 256 << 10, 512 << 10] {
            let (fills, backs) = simulate(&trace, size, 16, 50);
            let col = |a: &[[f64; 5]; STAGES], k: usize| a.iter().map(|r| r[k]).sum::<f64>();
            println!(
                "L2 {:>3} KiB 16-way LRU, per call: fills weights {:.0} scales {:.0} params {:.0} act {:.0} state {:.0} = {:.0}; write-backs act {:.0} state {:.0} = {:.0}",
                size >> 10,
                col(&fills, 0),
                col(&fills, 1),
                col(&fills, 2),
                col(&fills, 3),
                col(&fills, 4),
                (0..5).map(|k| col(&fills, k)).sum::<f64>(),
                col(&backs, 3),
                col(&backs, 4),
                (0..5).map(|k| col(&backs, k)).sum::<f64>(),
            );
            let (rf, rb) = simulate_random(&trace, size, 16, 50);
            let sum = |a: &[[f64; 5]; STAGES]| a.iter().flatten().sum::<f64>();
            println!(
                "      random replacement: fills {:.0} (weights {:.0}) write-backs {:.0}",
                sum(&rf),
                rf.iter().map(|r| r[0]).sum::<f64>(),
                sum(&rb)
            );
            if size == 128 << 10 {
                for (s, name) in NAMES.iter().enumerate() {
                    let f: f64 = fills[s].iter().sum();
                    let b: f64 = backs[s].iter().sum();
                    println!(
                        "    {name:<20} fills {f:>8.0} (weights {:>7.0} scales {:>6.0} act {:>7.0} state {:>7.0})  write-backs {b:>7.0}",
                        fills[s][0], fills[s][1], fills[s][3], fills[s][4]
                    );
                }
            }
        }
    }
    /// Per-hop traffic of every candidate weights file in `RC2FX_SWEEP_DIR`,
    /// as CSV lines:
    /// `cand,<name>,us_per_hop,unique bytes per class...` then one `sim,` line
    /// per cache size, policy and foreign working set, bytes per hop.
    /// `cargo test -p rc2fx-dfn --release -- --ignored --nocapture sweep_traffic`
    #[test]
    #[ignore]
    fn sweep_traffic() {
        let Ok(dir) = std::env::var("RC2FX_SWEEP_DIR") else {
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "bin"))
            .collect();
        files.sort();
        for path in files {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let bytes = std::fs::read(&path).unwrap();
            let mut filter = match crate::DeepFilter::from_weights(&bytes) {
                Ok(f) => f,
                Err(e) => {
                    println!("cand,{name},error,{e}");
                    continue;
                }
            };
            let mut frame = vec![0f32; filter.frame()];
            let mut phase = 0u32;
            let mut next = || {
                phase = phase.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (phase >> 9) as f32 / (1u32 << 23) as f32 - 0.5
            };
            let hops = (filter.frame() / 240).max(1);
            for _ in 0..50 {
                frame.iter_mut().for_each(|v| *v = next() * 0.1);
                filter.process(&mut frame);
            }
            let mut best = f64::MAX;
            for _ in 0..5 {
                let t = std::time::Instant::now();
                for _ in 0..200 {
                    frame.iter_mut().for_each(|v| *v = next() * 0.1);
                    filter.process(&mut frame);
                }
                best = best.min(t.elapsed().as_secs_f64() * 1e6 / (200 * hops) as f64);
            }
            enable();
            for _ in 0..200 {
                frame.iter_mut().for_each(|v| *v = next() * 0.1);
                filter.process(&mut frame);
            }
            let (_, total, unique) = report();
            let per = |a: &[[f64; 5]; STAGES], k: usize| {
                a.iter().map(|r| r[k]).sum::<f64>() / hops as f64
            };
            println!(
                "cand,{name},{best:.1},{},{}",
                (0..5)
                    .map(|k| format!("{:.0}", per(&unique, k)))
                    .collect::<Vec<_>>()
                    .join(","),
                (0..5)
                    .map(|k| format!("{:.0}", per(&total, k)))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let trace = trace();
            // The largest non-weight buffers of one call: class, first stage, span.
            let mut spans: HashMap<(Class, usize), (usize, u8)> = HashMap::new();
            for a in trace.iter().filter(|a| a.call == 100) {
                let e = spans.entry(a.key).or_insert((0, a.stage));
                e.0 = e.0.max(a.bytes as usize);
            }
            let mut rows: Vec<_> = spans
                .iter()
                .filter(|(k, _)| !matches!(k.0, Class::Q8 | Class::Scale))
                .map(|(k, v)| (v.0, k.0, v.1))
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.0));
            for (span, class, stage) in rows.iter().take(10) {
                println!("buf,{name},{class:?},{},{span}", NAMES[*stage as usize]);
            }
            // Without the weights: what activations, state and params need alone.
            let rest: Vec<Access> = trace
                .iter()
                .filter(|a| !matches!(a.key.0, Class::Q8 | Class::Scale))
                .copied()
                .collect();
            for kib in [32usize, 48, 64, 96, 128, 192, 256] {
                for foreign in [0usize, 64] {
                    if foreign >= kib {
                        continue;
                    }
                    let sum = |a: &[[f64; 5]; STAGES]| a.iter().flatten().sum::<f64>();
                    for packed in [false, true] {
                        let (f, b) =
                            simulate_layout(&rest, kib << 10, 16, 50, false, foreign << 10, packed);
                        println!(
                            "{},{name},{kib},{foreign},{:.0},{:.0}",
                            if packed { "nwp" } else { "nw" },
                            sum(&f) / hops as f64,
                            sum(&b) / hops as f64
                        );
                    }
                }
            }
            for kib in [32usize, 48, 64, 96, 128, 160, 192, 256, 384, 512] {
                for random in [false, true] {
                    for foreign in [0usize, 32, 64, 128] {
                        if foreign >= kib {
                            continue;
                        }
                        for packed in [false, true] {
                            let (f, b) = simulate_layout(
                                &trace,
                                kib << 10,
                                16,
                                50,
                                random,
                                foreign << 10,
                                packed,
                            );
                            println!(
                                "{},{name},{kib},{},{foreign},{},{}",
                                if packed { "simp" } else { "sim" },
                                if random { "random" } else { "lru" },
                                (0..5)
                                    .map(|k| format!("{:.0}", per(&f, k)))
                                    .collect::<Vec<_>>()
                                    .join(","),
                                (0..5)
                                    .map(|k| format!("{:.0}", per(&b, k)))
                                    .collect::<Vec<_>>()
                                    .join(",")
                            );
                        }
                    }
                }
            }
        }
    }
}
