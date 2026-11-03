//! Portable DSP-only timing; excludes JACK, scheduling and suppression workers.
//! Run with `cargo run --release -p rc2fx-dsp --example bench_eq`.
use rc2fx_core::{Band, Target, TargetKind};
use rc2fx_dsp::{Dsp, Prepared};
use std::{hint::black_box, time::Instant};

fn main() {
    const BLOCK: usize = 112;
    let blocks = std::env::args()
        .nth(1)
        .map(|value| {
            value
                .parse::<usize>()
                .expect("block count must be an integer")
        })
        .unwrap_or(10_000);
    assert!(
        (100..=100_000).contains(&blocks),
        "block count must be 100..=100000"
    );
    let input: [[f32; BLOCK]; 2] = std::array::from_fn(|ch| {
        std::array::from_fn(|i| (0.03 * ((i * 17 + ch * 31) as f64 * 0.017).sin()) as f32)
    });
    println!(
        "stereo, 48000 Hz, {BLOCK} samples/block, {blocks} blocks/round; elapsed ns/block; excludes JACK"
    );
    for bands in [0, 7, 20, 32] {
        let mut target = Target::new("usb-main", "USB", TargetKind::Playback, 2);
        target.limiter.enabled = false;
        for i in 0..bands {
            let mut band = Band::new(format!("b{i}"));
            band.frequency = 30.0 * 500.0_f64.powf(i as f64 / bands.max(1) as f64);
            band.gain = if i % 2 == 0 { -3.0 } else { 1.5 };
            target.eq.bands.push(band);
        }
        let prepared = Prepared::new(&target, 48_000).unwrap();
        let mut timings = Vec::new();
        let mut fingerprint = 0;
        for _ in 0..7 {
            let mut dsp = Dsp::without_suppression(&prepared).unwrap();
            let mut output = [[0.0; BLOCK]; 2];
            for _ in 0..1000 {
                let [left, right] = &mut output;
                dsp.process(&prepared, [&input[0], &input[1]], [left, right])
                    .unwrap();
            }
            let start = Instant::now();
            for _ in 0..blocks {
                let [left, right] = &mut output;
                dsp.process(
                    black_box(&prepared),
                    black_box([&input[0], &input[1]]),
                    [left, right],
                )
                .unwrap();
                black_box(&output);
            }
            timings.push(start.elapsed().as_nanos() / blocks as u128);
            fingerprint = output.iter().flatten().fold(0_u64, |hash, x| {
                hash.wrapping_mul(1099511628211) ^ u64::from(x.to_bits())
            });
        }
        timings.sort_unstable();
        println!(
            "bands={bands:2} min={} median={} max={} fingerprint={fingerprint:016x}",
            timings[0], timings[3], timings[6]
        );
    }
}
