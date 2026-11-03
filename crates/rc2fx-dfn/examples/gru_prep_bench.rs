//! Host stage benchmark for experimental staged GRU preparation
//! (`DeepFilter::set_gru_prep`). Not a mixer measurement: host caches, clock
//! and kernels differ from the Cortex-A53.
//!
//!     cargo run --release -p rc2fx-dfn --example gru_prep_bench -- [ROUNDS] [HOPS]
//!
//! Each mode has its own filter, fed the same input; the modes take turns in
//! blocks of HOPS calls for ROUNDS rounds, so slow drift in the host affects
//! them alike. Prints one JSON object. Times are wall nanoseconds per call
//! (`frame()` samples: one hop).
//!
//! Modes:
//! - `baseline`: staged preparation off.
//! - `none`: on, never prepared (the cost of checking).
//! - `partialK`: the first K layers prepared after each call.
//! - `full`: every layer prepared after each call; each step timed.
//! - `reset-baseline`, `reset-full`: a reset before every call, then (full)
//!   preparation from the zero states.
use rc2fx_dfn::DeepFilter;
use std::time::Instant;

struct Mode {
    name: String,
    filter: DeepFilter,
    /// Layers prepared after each call (`usize::MAX`: all).
    prep: usize,
    reset: bool,
    process: Vec<u64>,
    prep_total: Vec<u64>,
    /// Per layer index, in preparation order.
    steps: Vec<Vec<u64>>,
    out: Vec<f32>,
}

fn signal(seconds: usize) -> Vec<f32> {
    let rate = 48_000;
    let mut seed = 0x2545_f491u32;
    (0..seconds * rate)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1 << 23) as f32 - 1.;
            let t = i as f32 / rate as f32;
            let envelope = (t * 3. * std::f32::consts::TAU).sin().max(0.).powi(2);
            let pitch = 140. + 30. * (t * 2.).sin();
            let voice: f32 = (1..12)
                .map(|h| (h as f32 * pitch * t * std::f32::consts::TAU).sin() / h as f32)
                .sum::<f32>()
                * envelope
                * 0.15;
            voice + 0.01 * noise
        })
        .collect()
}

fn stats(v: &[u64]) -> String {
    if v.is_empty() {
        return "null".into();
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
    let mean = s.iter().sum::<u64>() as f64 / s.len() as f64;
    format!(
        "{{\"n\":{},\"mean\":{:.0},\"p50\":{},\"p90\":{},\"p99\":{},\"p999\":{},\"max\":{}}}",
        s.len(),
        mean,
        at(0.5),
        at(0.9),
        at(0.99),
        at(0.999),
        s[s.len() - 1]
    )
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("ROUNDS HOPS"))
        .collect();
    let rounds = args.first().copied().unwrap_or(40);
    let hops = args.get(1).copied().unwrap_or(250);
    let probe = DeepFilter::new().unwrap();
    let frame = probe.frame();
    let mut layers_probe = DeepFilter::new().unwrap();
    layers_probe.set_gru_prep(true);
    let layers = layers_probe.prep_pending();
    let input = signal(10);
    let calls = input.len() / frame;

    let mut modes: Vec<Mode> = Vec::new();
    let mut add = |name: String, on: bool, prep: usize, reset: bool| {
        let mut filter = DeepFilter::new().unwrap();
        filter.set_gru_prep(on);
        modes.push(Mode {
            name,
            filter,
            prep,
            reset,
            process: Vec::new(),
            prep_total: Vec::new(),
            steps: vec![Vec::new(); layers],
            out: Vec::new(),
        });
    };
    add("baseline".into(), false, 0, false);
    add("none".into(), true, 0, false);
    for k in 1..layers {
        add(format!("partial{k}"), true, k, false);
    }
    add("full".into(), true, usize::MAX, false);
    add("reset-baseline".into(), false, 0, true);
    add("reset-full".into(), true, usize::MAX, true);

    let mut buf = vec![0f32; frame];
    let warmup = 200;
    let mut position = 0usize;
    for round in 0..=rounds {
        let start = position;
        // Round 0 warms every mode up and is not recorded.
        let count = if round == 0 { warmup } else { hops };
        for m in &mut modes {
            for at in start..start + count {
                let chunk = &input[(at % calls) * frame..(at % calls + 1) * frame];
                buf.copy_from_slice(chunk);
                if m.reset {
                    m.filter.reset();
                    if m.prep > 0 {
                        m.filter.prepare_while(|| true);
                    }
                }
                let t0 = Instant::now();
                m.filter.process(&mut buf);
                let t1 = Instant::now();
                let mut prep_ns = 0;
                let mut i = 0;
                if !m.reset {
                    while i < m.prep.min(layers) {
                        let s0 = Instant::now();
                        let did = m.filter.prepare_step();
                        let s1 = Instant::now();
                        assert!(did);
                        let ns = (s1 - s0).as_nanos() as u64;
                        prep_ns += ns;
                        if round > 0 {
                            m.steps[i].push(ns);
                        }
                        i += 1;
                    }
                }
                if round > 0 {
                    m.process.push((t1 - t0).as_nanos() as u64);
                    if m.prep > 0 && !m.reset {
                        m.prep_total.push(prep_ns);
                    }
                    if round == 1 && m.out.len() < 50 * frame {
                        m.out.extend_from_slice(&buf);
                    }
                }
            }
        }
        position = start + count;
    }

    // The staged modes must reproduce the baseline output exactly.
    let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let base = bits(&modes[0].out);
    let reset_base = bits(
        &modes
            .iter()
            .find(|m| m.name == "reset-baseline")
            .unwrap()
            .out,
    );
    let mut identical = true;
    for m in &modes {
        let want = if m.reset { &reset_base } else { &base };
        identical &= bits(&m.out) == *want;
    }

    let mut json = format!(
        "{{\"frameSamples\":{frame},\"layers\":{layers},\"rounds\":{rounds},\"hopsPerRound\":{hops},\"outputsIdentical\":{identical},\"modes\":{{",
    );
    for (i, m) in modes.iter().enumerate() {
        let total: Vec<u64> = if m.prep_total.is_empty() {
            m.process.clone()
        } else {
            m.process
                .iter()
                .zip(&m.prep_total)
                .map(|(a, b)| a + b)
                .collect()
        };
        let steps: Vec<String> = m
            .steps
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| stats(s))
            .collect();
        json += &format!(
            "{}\"{}\":{{\"process\":{},\"prep\":{},\"processPlusPrep\":{},\"steps\":[{}]}}",
            if i > 0 { "," } else { "" },
            m.name,
            stats(&m.process),
            stats(&m.prep_total),
            stats(&total),
            steps.join(",")
        );
    }
    json += "}}";
    println!("{json}");
    assert!(identical, "staged output differs from baseline");
}
