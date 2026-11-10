//! `Dsp` and `Chat` driving the worker endpoint through the provider trait:
//! the endpoint's `process_block`, which is what they call, against the
//! same endpoint behind the per-sample provider methods only.
//!
//! The endpoints here have no OS thread. A fake model on the test's thread
//! takes the frames both endpoints submit, checks that they are the same
//! frames, and publishes one result to both, in time, late, too late or
//! never, so the two see identical queues at every block.
use super::*;
use rc2fx_core::{Target, TargetKind, VoiceGate};
use rc2fx_dsp::{Chat, Dsp, Meter, Prepared};

/// The device period.
const PERIOD: usize = 112;

/// The endpoint behind the per-sample provider methods only, so `Dsp`
/// reaches it through the trait's default `process_block`.
struct PerSample(Worker);
impl Suppression for PerSample {
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        self.0.geometry().resolved()
    }
    fn latency_samples(&self) -> usize {
        self.0.latency_samples()
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        self.0.process_sample(input, wet)
    }
    fn voice(&self) -> Option<f32> {
        self.0.voice()
    }
    fn drain(&mut self, timeout: Duration) -> Result<(), RetirementError> {
        Suppression::drain(&mut self.0, timeout)
    }
}

fn microphone(channels: u8, wet: f64, strength: f64) -> Target {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, channels);
    target.suppression.enabled = true;
    target.suppression.wet = wet;
    target.suppression.gate = VoiceGate::with_strength(strength);
    target
}

/// A delay and nothing else, with a steady voice score: the least a
/// provider can cost, for counting what `Dsp` itself costs around it.
struct Delay {
    line: Vec<f32>,
    at: usize,
}
impl Suppression for Delay {
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        geometry().resolved()
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        let dry = self.line[self.at];
        self.line[self.at] = input;
        self.at += 1;
        if self.at == self.line.len() {
            self.at = 0;
        }
        dry + (dry * 0.5 - dry) * wet
    }
    fn voice(&self) -> Option<f32> {
        Some(0.75)
    }
    fn drain(&mut self, _: Duration) -> Result<(), RetirementError> {
        Ok(())
    }
}

/// The mixer's microphone path as saved (mono, suppression only, wet 0.79,
/// gate 0.61) in 112-sample blocks, for counting instructions under an
/// emulator. `RC2FX_MIC_PATH` is `block` (the endpoint's block form, as the
/// mixer runs it), `sample` (the endpoint's per-sample provider methods) or
/// `delay` (a delay in place of the endpoint); `RC2FX_MIC_BLOCKS` is the
/// number of blocks. The fake model serves the endpoint's queues on this
/// thread after every block, and its cost is in the count: the difference
/// between two block counts is the steady cost of a block.
#[test]
#[ignore = "for counting instructions; set RC2FX_MIC_PATH and RC2FX_MIC_BLOCKS"]
fn mic_path_bench() {
    let path = std::env::var("RC2FX_MIC_PATH").expect("RC2FX_MIC_PATH=block|sample|delay");
    let blocks: usize = std::env::var("RC2FX_MIC_BLOCKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .expect("RC2FX_MIC_BLOCKS=count");
    let (worker, mut input, mut output) = endpoint();
    let provider: Box<dyn Suppression> = match path.as_str() {
        "block" => Box::new(worker),
        "sample" => Box::new(PerSample(worker)),
        "delay" => Box::new(Delay {
            line: vec![0.; geometry().latency_samples()],
            at: 0,
        }),
        other => panic!("RC2FX_MIC_PATH={other}"),
    };
    let p = Prepared::new(&microphone(1, 0.79, 0.61), 48_000).unwrap();
    let mut dsp = Dsp::new(&p, [Some(provider), None]).unwrap();
    let x: Vec<f32> = (0..PERIOD * 64)
        .map(|i| (i as f32 * 0.013).sin() * 0.25)
        .collect();
    let mut out = [0f32; PERIOD];
    let mut sum = 0.0;
    for b in 0..blocks {
        let chunk = std::hint::black_box(&x[(b % 64) * PERIOD..(b % 64 + 1) * PERIOD]);
        let meter = dsp.process(&p, [chunk, &[]], [&mut out, &mut []]).unwrap();
        sum += meter.sum_squares + f64::from(std::hint::black_box(&out)[0]);
        while let Some(mut frame) = input.pop() {
            for x in &mut frame.samples {
                *x *= 0.5;
            }
            frame.voice = 0.75;
            assert!(output.push(&frame));
        }
    }
    println!("{path} blocks {blocks} sum {sum}");
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn sample(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.
    }
}

/// When the fake model publishes a frame's result, counted in blocks after
/// the block that completed the frame.
#[derive(Clone, Copy, Debug)]
enum Timing {
    /// Before the next block: always in time.
    Prompt,
    /// Mostly prompt; some results a few blocks behind (late, or too late
    /// and discarded), some never, some not finite or marked invalid.
    Ragged,
    /// Every result several blocks behind.
    Slow,
    /// Nothing is ever published.
    Silent,
    /// The model stops taking frames for a while, so the input queue fills
    /// and frames are dropped; then it catches up.
    Stalling,
}

/// The test's side of one endpoint.
struct Wires {
    input: Consumer,
    output: Producer,
    shared: Arc<Shared>,
    telemetry: Telemetry,
}

/// The same endpoint twice: boxed as itself, so `Dsp` reaches its
/// `process_block`, and behind `PerSample`.
fn endpoints(as_enum: bool) -> ([Option<Box<dyn Suppression>>; 2], [Wires; 2]) {
    let mut wires = Vec::new();
    let mut providers = Vec::new();
    for block in [true, false] {
        let (worker, input, output) = endpoint();
        wires.push(Wires {
            input,
            output,
            shared: worker.shared.clone(),
            telemetry: worker.telemetry(),
        });
        let provider: Box<dyn Suppression> = match (block, as_enum) {
            (true, false) => Box::new(worker),
            (true, true) => Box::new(Endpoint::Worker(worker)),
            (false, _) => Box::new(PerSample(worker)),
        };
        providers.push(Some(provider));
    }
    (
        providers.try_into().ok().unwrap(),
        wires.try_into().ok().unwrap(),
    )
}

/// The fake model for both endpoints.
struct Model {
    timing: Timing,
    rng: Rng,
    /// Results not yet published and the block each is due after.
    waiting: Vec<(usize, Frame)>,
    submitted: u64,
}
impl Model {
    fn new(timing: Timing, seed: u64) -> Self {
        Self {
            timing,
            rng: Rng(seed | 1),
            waiting: Vec::new(),
            submitted: 0,
        }
    }
    /// After block `block`: takes what both endpoints submitted and
    /// publishes what is due.
    fn serve(&mut self, block: usize, wires: &mut [Wires; 2]) {
        let stalled = matches!(self.timing, Timing::Stalling) && block % 90 >= 50;
        loop {
            if stalled {
                break;
            }
            let [a, b] = wires.each_mut().map(|w| w.input.pop());
            let (Some(a), Some(b)) = (a, b) else {
                assert!(a.is_none() && b.is_none(), "one endpoint submitted more");
                break;
            };
            assert_eq!((a.sequence, a.valid), (b.sequence, b.valid));
            assert_eq!(
                a.samples.map(f32::to_bits),
                b.samples.map(f32::to_bits),
                "frame {} as submitted",
                a.sequence
            );
            self.submitted += 1;
            let mut result = a;
            for x in &mut result.samples {
                *x *= 0.5;
            }
            // Slow enough at first for the gate's hold to run out and the gate
            // to close, then fast.
            let step = if a.sequence < 240 {
                a.sequence / 30
            } else {
                a.sequence / 3
            };
            result.voice = [0.9, 0.05, 0.1, 0.3, 0.2, 0.75, 0.6, 0.0][(step % 8) as usize];
            let delay = match self.timing {
                Timing::Prompt | Timing::Stalling => Some(0),
                Timing::Slow => Some(3 + self.rng.below(4) as usize),
                Timing::Silent => None,
                Timing::Ragged => match self.rng.below(16) {
                    0 => None,
                    1 => Some(1),
                    2 => Some(2),
                    3 => Some(3),
                    4 => Some(4),
                    5 => Some(7),
                    6 => {
                        result.samples[self.rng.below(240) as usize] = f32::NAN;
                        Some(0)
                    }
                    7 => {
                        result.samples[0] = f32::INFINITY;
                        Some(0)
                    }
                    8 => {
                        result.valid = false;
                        Some(0)
                    }
                    9 => {
                        result.voice = f32::NAN;
                        Some(0)
                    }
                    _ => Some(0),
                },
            };
            if let Some(delay) = delay {
                self.waiting.push((block + delay, result));
            }
        }
        // In order of arrival, so a delayed result can come after a newer one.
        let mut held = Vec::new();
        for (due, result) in self.waiting.drain(..) {
            if due > block {
                held.push((due, result));
                continue;
            }
            let [a, b] = wires.each_mut().map(|w| w.output.push(&result));
            assert_eq!(a, b, "result {} accepted by one endpoint", result.sequence);
        }
        self.waiting = held;
    }
}

fn meter_bits(m: &Meter) -> [u64; 8] {
    [
        m.input_peak.to_bits(),
        m.output_peak.to_bits(),
        m.sum_squares.to_bits(),
        m.gain_reduction.to_bits(),
        m.voice.to_bits(),
        m.gate_open.to_bits(),
        m.samples,
        m.clipped,
    ]
}

fn counters(w: &Wires) -> [u64; 7] {
    let s = w.telemetry.snapshot();
    [
        s.completed_frames,
        s.input_drops,
        s.output_drops,
        s.missed_deadlines,
        s.late_results,
        s.stale_results,
        u64::from(s.failed),
    ]
}

/// What the endpoint itself counted once it has been retired.
fn receipt(w: &Wires) -> (u64, u64, usize, u64, u32, [u64; 7]) {
    let t = w.telemetry.terminal().expect("retired");
    (
        t.input_samples,
        t.full_input_frames,
        t.partial_input_samples,
        t.submitted_frames,
        t.pending_input_frames,
        counters(w),
    )
}

fn signal(rng: &mut Rng, frames: usize, amplitude: f32, rare: u64) -> Vec<f32> {
    (0..frames)
        .map(|_| {
            let value = rng.sample() * amplitude;
            match rng.below(rare) {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                3 => 17.0,
                4 => -40.0,
                5 => f32::MAX,
                6 => -0.0,
                7 => 1e-42,
                _ => value,
            }
        })
        .collect()
}

/// Block lengths: every length from 1 to 128 in turn, then the device
/// period, then lengths that straddle the model's 240-sample frame in
/// different ways. A block longer than the worker lead makes results late
/// by itself, whatever the model does.
fn length(block: usize, rng: &mut Rng) -> usize {
    match block {
        0..=127 => block + 1,
        128..=427 => PERIOD,
        428..=700 => [112, 113, 7, 128, 1, 240, 129, 300, 111, 480][block % 10],
        _ => 1 + rng.below(160) as usize,
    }
}

fn edits(block: usize, target: &mut Target) {
    match block {
        140 => target.suppression.wet = 0.2,
        230 => target.suppression.gate = VoiceGate::with_strength(1.0),
        300 => target.bypass = true,
        340 => target.bypass = false,
        440 => target.suppression.gate = VoiceGate::with_strength(0.0),
        500 => target.suppression.wet = 1.0,
        560 => target.suppression.gate = VoiceGate::with_strength(0.4),
        640 => target.suppression.wet = 0.0,
        720 => target.suppression.wet = 0.79,
        _ => {}
    }
}

const BLOCKS: usize = 900;
const TIMINGS: [Timing; 5] = [
    Timing::Prompt,
    Timing::Ragged,
    Timing::Slow,
    Timing::Silent,
    Timing::Stalling,
];

/// A mono microphone through `Dsp` with the endpoint's block form and with
/// its per-sample form: every output bit, every meter field (the voice
/// score and gate position among them), every submitted frame and every
/// endpoint counter, while results come in time, late, too late or not at
/// all, the worker fails, and the wet gain, gate and bypass are edited.
#[test]
fn a_microphone_with_the_block_endpoint_matches_the_per_sample_endpoint() {
    let mut seen = [0_u64; 7];
    let (mut voiced, mut gated) = (0_u32, 0_u32);
    for (case, timing) in TIMINGS.into_iter().enumerate() {
        for fail_at in [None, Some(333)] {
            let (providers, mut wires) = endpoints(case % 2 == 1);
            let mut target = microphone(1, 0.79, 0.61);
            let mut p = Prepared::new(&target, 48_000).unwrap();
            let mut dsps = providers.map(|provider| Dsp::new(&p, [provider, None]).unwrap());
            let mut model = Model::new(timing, 0x9E37_79B9 + case as u64);
            let mut rng = Rng(0x2545_F491_4F6C_DD1D + case as u64);
            for block in 0..BLOCKS {
                let before = target.clone();
                edits(block, &mut target);
                if target != before {
                    p = Prepared::new(&target, 48_000).unwrap();
                }
                if fail_at == Some(block) {
                    for w in &wires {
                        w.shared.failed.store(true, Ordering::Release);
                    }
                }
                let frames = length(block, &mut rng);
                let amplitude = if block % 40 < 30 { 0.5 } else { 0.0 };
                let input = signal(&mut rng, frames, amplitude, 250);
                let mut outputs = [vec![0f32; frames], input.clone()];
                let block_meter = dsps[0]
                    .process(&p, [&input, &[]], [&mut outputs[0], &mut []])
                    .unwrap();
                let sample_meter = dsps[1]
                    .process_in_place(&p, [&mut outputs[1], &mut []])
                    .unwrap();
                let context = format!("{timing:?} fail {fail_at:?} block {block} frames {frames}");
                let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&outputs[0]), bits(&outputs[1]), "{context}");
                assert_eq!(
                    meter_bits(&block_meter),
                    meter_bits(&sample_meter),
                    "{context}"
                );
                assert_eq!(counters(&wires[0]), counters(&wires[1]), "{context}");
                voiced += u32::from(block_meter.voice > 0.5);
                gated += u32::from(block_meter.gate_open < 0.5);
                model.serve(block, &mut wires);
            }
            for (i, total) in counters(&wires[0]).into_iter().enumerate() {
                seen[i] += total;
            }
            // No worker thread completes frames here, so the drain reports
            // them unfinished; what matters is that both endpoints retire
            // with the same receipt.
            let [a, b] = dsps
                .each_mut()
                .map(|dsp| dsp.drain_suppression(Duration::ZERO));
            assert_eq!(a, b, "{timing:?}");
            assert_eq!(receipt(&wires[0]), receipt(&wires[1]), "{timing:?}");
            assert!(receipt(&wires[0]).0 > 60_000);
        }
    }
    // Input drops, missed deadlines, late results and discarded results all
    // happened, and the gate both heard voice and closed.
    eprintln!("counters {seen:?} voiced {voiced} gated {gated}");
    assert!(
        seen[1] > 0 && seen[3] > 100 && seen[4] > 20 && seen[5] > 20,
        "{seen:?}"
    );
    assert!(voiced > 500 && gated > 50, "{voiced} {gated}");
}

/// The same for the Chat voice model, which `Chat` hands a block at a time.
#[test]
fn chat_with_the_block_endpoint_matches_the_per_sample_endpoint() {
    let mut seen = [0_u64; 7];
    for (case, timing) in TIMINGS.into_iter().enumerate() {
        let (providers, mut wires) = endpoints(case % 2 == 0);
        let mut target = Target::new("send-chat", "Chat", TargetKind::Output, 2);
        target.suppression.enabled = true;
        target.suppression.wet = 0.79;
        target.suppression.gate = VoiceGate::with_strength(0.61);
        let mut p = Prepared::new(&target, 48_000).unwrap();
        let mut chats = providers.map(|provider| Chat::new(&p, provider.unwrap()).unwrap());
        let mut model = Model::new(timing, 0xD1B5_4A32 + case as u64);
        let mut rng = Rng(0xA076_1D64_78BD_642F + case as u64);
        for block in 0..BLOCKS {
            let before = target.clone();
            edits(block, &mut target);
            if target != before {
                p = Prepared::new(&target, 48_000).unwrap();
            }
            if block == 777 {
                for w in &wires {
                    w.shared.failed.store(true, Ordering::Release);
                }
            }
            let frames = length(block, &mut rng);
            let amplitude = if block % 40 < 30 { 0.4 } else { 0.0 };
            // The stem must be finite; the mix may hold anything.
            let stem = [0, 1].map(|_| signal(&mut rng, frames, amplitude, u64::MAX));
            let mix = [0, 1].map(|_| signal(&mut rng, frames, 0.5, 250));
            let mut meters = Vec::new();
            let mut outputs = Vec::new();
            for chat in &mut chats {
                let mut output = mix.clone();
                let [left, right] = &mut output;
                meters.push(
                    chat.process(&p, [&stem[0], &stem[1]], [left, right])
                        .unwrap(),
                );
                outputs.push(output);
            }
            let context = format!("{timing:?} block {block} frames {frames}");
            for (ch, (block, sample)) in outputs[0].iter().zip(&outputs[1]).enumerate() {
                let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(block), bits(sample), "ch{ch}; {context}");
            }
            assert_eq!(meter_bits(&meters[0]), meter_bits(&meters[1]), "{context}");
            assert_eq!(counters(&wires[0]), counters(&wires[1]), "{context}");
            model.serve(block, &mut wires);
        }
        for (i, total) in counters(&wires[0]).into_iter().enumerate() {
            seen[i] += total;
        }
        let [a, b] = chats
            .each_mut()
            .map(|chat| chat.drain_suppression(Duration::ZERO));
        assert_eq!(a, b, "{timing:?}");
        assert_eq!(receipt(&wires[0]), receipt(&wires[1]), "{timing:?}");
    }
    eprintln!("counters {seen:?}");
    assert!(
        seen[1] > 0 && seen[3] > 50 && seen[4] > 10 && seen[5] > 10,
        "{seen:?}"
    );
}
