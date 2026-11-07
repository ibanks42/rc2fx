//! Saved selected chains, sequential paced producer; no JACK or settings writes.
//! This measures DSP work, not the real graph's parallel callback scheduling.
#[cfg(target_os = "linux")]
fn main() {
    use rc2fx_core::{Config, TargetKind};
    use rc2fx_device::{
        audio::{Chain, Group},
        metrics::Clock,
    };
    use rc2fx_dsp::Suppression;
    use rc2fx_suppression::{Endpoint, PRODUCTION_WORKER, WorkerOptions};
    use std::time::{Duration, Instant};
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(
        (3..=4).contains(&args.len()),
        "STATE_JSON on|off SECONDS [device]"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    let config: Config = serde_json::from_value(state["config"].clone()).unwrap();
    let seconds: u64 = args[2].parse().unwrap();
    assert!((1..=30).contains(&seconds));
    let options = if args.get(3).is_some_and(|s| s == "device") {
        PRODUCTION_WORKER
    } else {
        assert_eq!(args.len(), 3);
        WorkerOptions::default()
    };
    let mut targets = Vec::new();
    let mut chains = Vec::new();
    let mut workers = Vec::new();
    for id in &config.selected_targets {
        let mut target = config.targets.iter().find(|t| &t.id == id).unwrap().clone();
        if target.kind == TargetKind::Mic && target.suppression.enabled {
            match args[1].as_str() {
                "on" => {}
                "off" => target.suppression.enabled = false,
                _ => panic!("on|off"),
            }
        }
        let mut providers: [Option<Box<dyn Suppression>>; 2] = [None, None];
        if target.kind == TargetKind::Mic && target.suppression.enabled {
            let worker = Endpoint::new(target.suppression.lookahead, options).unwrap();
            workers.push((id.clone(), worker.telemetry()));
            providers[0] = Some(Box::new(worker));
        }
        chains.push(Chain::new(&target, providers).unwrap());
        targets.push(target);
    }
    let mut group = Group::new(chains, config.revision).unwrap();
    let clock = Clock::probe(true);
    let mut cpu = vec![0_u64; targets.len()];
    let mut wall = cpu.clone();
    let mut peak = cpu.clone();
    let mut clock_errors = cpu.clone();
    let input: [f32; 112] = std::array::from_fn(|i| 0.08 * (i as f32 * 0.037).sin());
    let mut left = [0.0_f32; 112];
    let mut right = left;
    let blocks = seconds * 48_000 / 112;
    let start = Instant::now();
    let mut late_periods = 0;
    let mut max_late = 0;
    {
        let (_control, mut callbacks) = group.split().unwrap();
        for block in 0..blocks {
            let deadline = start + Duration::from_nanos(block * 112 * 1_000_000_000 / 48_000);
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let late = Instant::now()
                .saturating_duration_since(deadline)
                .as_nanos();
            max_late = max_late.max(late);
            if late > 112 * 1_000_000_000 / 48_000 {
                late_periods += 1;
            }
            for (i, callback) in callbacks.iter_mut().enumerate() {
                let stamp = clock.start();
                let stereo = callback.channels() == 2;
                std::hint::black_box(
                    callback
                        .process(
                            [&input, if stereo { &input } else { &[] }],
                            [&mut left, if stereo { &mut right } else { &mut [] }],
                        )
                        .unwrap(),
                );
                let elapsed = clock.finish(stamp);
                if let (Some(c), Some(w)) = (elapsed.thread_cpu_ns(), elapsed.wall_ns()) {
                    cpu[i] += c;
                    wall[i] += w;
                    peak[i] = peak[i].max(w);
                } else {
                    clock_errors[i] += 1;
                }
            }
        }
    }
    group.drain_suppression(Duration::from_secs(2)).unwrap();
    println!(
        "{}",
        serde_json::json!({"model":args[1],"blocks":blocks,"latePeriods":late_periods,"maxLateNs":max_late,"productionWorkerPolicy":args.len()==4})
    );
    for (i, target) in targets.iter().enumerate() {
        println!(
            "{}",
            serde_json::json!({"target":target.id,"channels":target.channels,"bands":target.eq.bands.len(),"bypass":target.bypass,"cpuNs":cpu[i],"wallNs":wall[i],"maxWallNs":peak[i],"clockErrors":clock_errors[i]})
        );
    }
    for (id, worker) in workers {
        println!("worker={id} terminal={:?}", worker.terminal().unwrap());
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("Linux timing benchmark; run on Linux");
}
