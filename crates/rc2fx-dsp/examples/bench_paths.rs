//! DSP-only saved-path benchmark. No JACK, worker, routing or settings writes.
//! Suppression is explicitly removed to isolate our callback-local DSP cost.
#[cfg(target_os = "linux")]
fn main() {
    use rc2fx_core::Config;
    use rc2fx_dsp::{Dsp, Prepared};
    use std::{hint::black_box, time::Instant};
    fn cpu_ns() -> u64 {
        let mut stamp: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) },
            0
        );
        (stamp.tv_sec as u64) * 1_000_000_000 + stamp.tv_nsec as u64
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2, "bench_paths STATE_JSON BLOCKS_PER_ROUND");
    let blocks: usize = args[1].parse().unwrap();
    assert!((100..=10_000).contains(&blocks));
    let disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    let config: Config = serde_json::from_value(disk["config"].clone()).unwrap();
    config.validate().unwrap();
    const FRAMES: usize = 112;
    let input: [[f32; FRAMES]; 2] = std::array::from_fn(|ch| {
        std::array::from_fn(|i| (0.05 * ((i * 17 + ch * 31) as f64 * 0.017).sin()) as f32)
    });
    for id in &config.selected_targets {
        let mut target = config.targets.iter().find(|t| &t.id == id).unwrap().clone();
        let suppression_removed = target.suppression.enabled;
        target.suppression.enabled = false;
        let p = Prepared::new(&target, 48_000).unwrap();
        let stereo = target.channels == 2;
        let mut rounds = Vec::with_capacity(5);
        let mut fingerprint = 0_u64;
        for _ in 0..5 {
            let mut dsp = Dsp::without_suppression(&p).unwrap();
            let mut output = [[0.0; FRAMES]; 2];
            for _ in 0..256 {
                let [left, right] = &mut output;
                black_box(
                    dsp.process(
                        &p,
                        [&input[0], if stereo { &input[1] } else { &[] }],
                        [left, if stereo { right } else { &mut [] }],
                    )
                    .unwrap(),
                );
            }
            let wall = Instant::now();
            let cpu = cpu_ns();
            for _ in 0..blocks {
                let [left, right] = &mut output;
                black_box(
                    dsp.process(
                        black_box(&p),
                        black_box([&input[0], if stereo { &input[1] } else { &[] }]),
                        [left, if stereo { right } else { &mut [] }],
                    )
                    .unwrap(),
                );
                black_box(&output);
            }
            let cpu = cpu_ns() - cpu;
            let wall = wall.elapsed().as_nanos() as u64;
            for sample in output.iter().take(target.channels as usize).flatten() {
                fingerprint = fingerprint.wrapping_mul(1099511628211) ^ u64::from(sample.to_bits());
            }
            rounds.push(serde_json::json!({"cpuNsPerBlock":cpu / blocks as u64,"wallNsPerBlock":wall / blocks as u64}));
        }
        println!(
            "{}",
            serde_json::json!({"target":id,"channels":target.channels,"blocksPerRound":blocks,"framesPerBlock":FRAMES,"suppressionRemoved":suppression_removed,"fingerprint":format!("{fingerprint:016x}"),"rounds":rounds})
        );
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("Linux-only DSP timing benchmark");
}
