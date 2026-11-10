//! `Suppression::process_block` against the per-sample provider methods,
//! through `Dsp` and `Chat`, bit for bit.
//!
//! Three identical states take the same input. One has a provider that
//! overrides `process_block` with a form written separately from its
//! `process_sample`; one has the same provider behind the trait's default
//! method; the third is the reference, `Dsp::frame`, which calls
//! `process_sample` and `voice` for each sample. Output samples, all eight
//! meter fields, the whole processing state with the gate, and what each
//! provider was handed and left behind must be the same in all three.
use super::block_tests::{Rng, adversarial, bands, edit, meter_bits, reference_block, state};
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering::Relaxed},
};

/// The model's frame.
const HOP: u64 = 240;

/// What a provider publishes after every call, for the test to compare:
/// a digest of each input and wet value in order, and where it ended.
#[derive(Default)]
struct Probe {
    digest: AtomicU64,
    samples: AtomicU64,
    position: AtomicU64,
    voice: AtomicU64,
    /// Calls to the overridden `process_block`.
    blocks: AtomicU64,
}
impl Probe {
    fn bits(&self) -> [u64; 4] {
        [
            self.digest.load(Relaxed),
            self.samples.load(Relaxed),
            self.position.load(Relaxed),
            self.voice.load(Relaxed),
        ]
    }
}

/// How a frame's result reaches the output.
enum Plan {
    /// In time: plays from the frame's first sample with this voice score.
    Ready(f32),
    /// Arrives this many samples into its frame and plays from there.
    Late(u64, f32),
    /// Never arrives; the frame plays the delayed original.
    Missing,
}

/// A stand-in for the worker endpoint with the same shape of state: a ring
/// of delayed input, a result per 240-sample frame that starts playing one
/// frame and the worker lead after its input, and a voice score per frame.
/// Results arrive in time, late or not at all by a fixed schedule; `wild`
/// also makes it return values no worker should.
struct Hop {
    geometry: SuppressionGeometry,
    /// Samples before the first result plays.
    start: u64,
    line: Vec<f32>,
    at: usize,
    count: u64,
    voice: Option<f32>,
    digest: u64,
    /// Frames each step of the schedule lasts, and where this channel is in
    /// it, so two channels score differently.
    stretch: u64,
    offset: u64,
    wild: bool,
    probe: Arc<Probe>,
}
impl Hop {
    fn new(stretch: u64, offset: u64, wild: bool, probe: Arc<Probe>) -> Self {
        let geometry = rc2fx_core::Suppression::admitted_geometry(0).unwrap();
        Self {
            geometry,
            start: u64::from(geometry.frame_samples + geometry.worker_lead_samples),
            line: vec![0.0; geometry.latency_samples() as usize],
            at: 0,
            count: 0,
            voice: None,
            digest: 0,
            stretch,
            offset,
            wild,
            probe,
        }
    }
    fn plan(&self, frame: u64) -> Plan {
        match (frame / self.stretch + self.offset) % 14 {
            0 => Plan::Ready(0.9),
            1 => Plan::Ready(0.05),
            2 => Plan::Missing,
            3 => Plan::Ready(0.4),
            4 => Plan::Ready(f32::NAN),
            5 => Plan::Late(1 + frame * 37 % (HOP - 1), 0.62),
            6 => Plan::Ready(if frame.is_multiple_of(2) { 0.8 } else { 0.1 }),
            7 => Plan::Ready(-0.0),
            8 => Plan::Ready(0.0),
            9 => Plan::Late(HOP - 1, 0.95),
            10 => Plan::Ready(1.0),
            11 => Plan::Ready((frame % 17) as f32 / 16.0),
            12 => Plan::Late(frame % HOP, 0.02),
            _ => Plan::Ready(f32::INFINITY),
        }
    }
    /// The result sample for the input `old`, `since` samples after results
    /// began to play.
    fn result(&self, old: f32, since: u64) -> f32 {
        if self.wild {
            match since % 1013 {
                0 => return f32::NAN,
                1 => return f32::INFINITY,
                2 => return 40.0,
                3 => return -1e30,
                4 => return f32::from_bits(1),
                _ => {}
            }
        }
        old * 0.5
    }
    fn seen(digest: u64, input: f32, wet: f32) -> u64 {
        (digest.rotate_left(7) ^ u64::from(input.to_bits()) ^ (u64::from(wet.to_bits()) << 32))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }
    fn publish(&self) {
        self.probe.digest.store(self.digest, Relaxed);
        self.probe.samples.store(self.count, Relaxed);
        self.probe.position.store(self.at as u64, Relaxed);
        self.probe.voice.store(
            self.voice
                .map_or(u64::MAX, |score| u64::from(score.to_bits())),
            Relaxed,
        );
    }
    /// One sample.
    fn sample(&mut self, input: f32, wet: f32) -> f32 {
        self.digest = Self::seen(self.digest, input, wet);
        let x = if input.is_finite() { input } else { 0.0 };
        let latency = self.line.len();
        let dry = self.line[self.at];
        let old = self.line[(self.at + latency - self.start as usize) % latency];
        self.line[self.at] = x;
        self.at = (self.at + 1) % latency;
        let now = self.count;
        self.count += 1;
        let mut output = dry;
        self.voice = None;
        if now >= self.start {
            let since = now - self.start;
            let score = match self.plan(since / HOP) {
                Plan::Ready(score) => Some(score),
                Plan::Late(after, score) if since % HOP >= after => Some(score),
                _ => None,
            };
            if score.is_some() {
                output = self.result(old, since);
                self.voice = score;
            }
        }
        self.publish();
        dry + (output - dry) * wet
    }
    /// A block, written apart from `sample`: the block is cut into runs over
    /// which the score cannot change and neither ring index wraps, and each
    /// run is done a pass at a time.
    fn block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        assert!(wet.len() == samples.len() && voice.len() == samples.len());
        self.probe.blocks.fetch_add(1, Relaxed);
        let latency = self.line.len();
        let mut done = 0;
        while done < samples.len() {
            let behind = (self.at + latency - self.start as usize) % latency;
            let mut run = (samples.len() - done)
                .min(latency - self.at)
                .min(latency - behind);
            let mut score = None;
            if self.count < self.start {
                run = run.min((self.start - self.count) as usize);
            } else {
                let since = self.count - self.start;
                let phase = since % HOP;
                run = run.min((HOP - phase) as usize);
                match self.plan(since / HOP) {
                    Plan::Ready(ready) => score = Some(ready),
                    Plan::Late(after, late) if phase >= after => score = Some(late),
                    Plan::Late(after, _) => run = run.min((after - phase) as usize),
                    Plan::Missing => {}
                }
            }
            let (samples, wet, voice) = (
                &mut samples[done..done + run],
                &wet[done..done + run],
                &mut voice[done..done + run],
            );
            for (input, wet) in samples.iter().zip(wet) {
                self.digest = Self::seen(self.digest, *input, *wet);
            }
            let mut dry = [0.0_f32; BLOCK];
            dry[..run].copy_from_slice(&self.line[self.at..self.at + run]);
            let mut played = dry;
            if score.is_some() {
                let since = self.count - self.start;
                for (i, old) in self.line[behind..behind + run].iter().enumerate() {
                    played[i] = self.result(*old, since + i as u64);
                }
            }
            for (held, input) in self.line[self.at..self.at + run].iter_mut().zip(&*samples) {
                *held = if input.is_finite() { *input } else { 0.0 };
            }
            for i in 0..run {
                samples[i] = dry[i] + (played[i] - dry[i]) * wet[i];
            }
            voice.fill(score);
            self.voice = score;
            self.at = (self.at + run) % latency;
            self.count += run as u64;
            done += run;
        }
        self.publish();
    }
}

/// `Hop` behind the per-sample methods only.
struct BySample(Hop);
impl Suppression for BySample {
    fn geometry(&self) -> SuppressionGeometry {
        self.0.geometry
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        self.0.sample(input, wet)
    }
    fn voice(&self) -> Option<f32> {
        self.0.voice
    }
    fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
        Ok(())
    }
}

/// `Hop` with its own `process_block`.
struct ByBlock(Hop);
impl Suppression for ByBlock {
    fn geometry(&self) -> SuppressionGeometry {
        self.0.geometry
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        self.0.sample(input, wet)
    }
    fn process_block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        self.0.block(samples, wet, voice)
    }
    fn voice(&self) -> Option<f32> {
        self.0.voice
    }
    fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Providers {
    stretch: u64,
    /// Where in the schedule the first channel starts.
    offset: u64,
    wild: bool,
}
impl Providers {
    fn boxed(self, channel: usize, block: bool, probe: &Arc<Probe>) -> Box<dyn Suppression> {
        let hop = Hop::new(
            self.stretch + 3 * channel as u64,
            self.offset + 5 * channel as u64,
            self.wild,
            probe.clone(),
        );
        if block {
            Box::new(ByBlock(hop))
        } else {
            Box::new(BySample(hop))
        }
    }
}

/// One state and its providers' probes.
struct Side {
    dsp: Dsp,
    probes: [Arc<Probe>; 2],
}
impl Side {
    fn new(p: &Prepared, providers: Providers, block: bool) -> Self {
        let probes: [Arc<Probe>; 2] = Default::default();
        let dsp = Dsp::new(
            p,
            std::array::from_fn(|ch| {
                (ch < p.channels()).then(|| providers.boxed(ch, block, &probes[ch]))
            }),
        )
        .unwrap();
        Self { dsp, probes }
    }
    fn probe_bits(&self) -> [[u64; 4]; 2] {
        [self.probes[0].bits(), self.probes[1].bits()]
    }
}

/// The block form, the default method and the per-sample reference.
struct Trio {
    block: Side,
    default: Side,
    reference: Side,
    /// Blocks whose gate level moved and blocks that left it where it was,
    /// and the same for the wet gain.
    gate: [u64; 2],
    wet: [u64; 2],
}
impl Trio {
    fn new(p: &Prepared, providers: Providers) -> Self {
        Self {
            block: Side::new(p, providers, true),
            default: Side::new(p, providers, false),
            reference: Side::new(p, providers, false),
            gate: [0; 2],
            wet: [0; 2],
        }
    }
    /// Runs `input` through all three and asserts that nothing differs.
    fn check(
        &mut self,
        p: &Prepared,
        input: &[Vec<f32>; 2],
        in_place: bool,
        context: &dyn Fn() -> String,
    ) -> Meter {
        let before = (
            self.block.dsp.gate_db.to_bits(),
            self.block.dsp.wet.to_bits(),
        );
        let (expected, expected_meter) = reference_block(&mut self.reference.dsp, p, input);
        for (name, side) in [("block", &mut self.block), ("default", &mut self.default)] {
            let mut output = if in_place {
                input.clone()
            } else {
                [vec![0.0; input[0].len()], vec![0.0; input[1].len()]]
            };
            let [left, right] = &mut output;
            let meter = if in_place {
                side.dsp.process_in_place(p, [left, right]).unwrap()
            } else {
                side.dsp
                    .process(p, [&input[0], &input[1]], [left, right])
                    .unwrap()
            };
            for ch in 0..p.channels() {
                for (n, (got, want)) in output[ch].iter().zip(&expected[ch]).enumerate() {
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "{name} sample ch{ch} n{n}: {got} != {want}; {}",
                        context()
                    );
                }
            }
            assert_eq!(
                meter_bits(&meter),
                meter_bits(&expected_meter),
                "{name} meter {meter:?} != {expected_meter:?}; {}",
                context()
            );
            assert_eq!(
                state(&side.dsp),
                state(&self.reference.dsp),
                "{name} state; {}",
                context()
            );
            assert_eq!(
                side.probe_bits(),
                self.reference.probe_bits(),
                "{name} provider; {}",
                context()
            );
        }
        // The reference does not keep the meter's gate position.
        assert_eq!(
            (
                self.block.dsp.gate_open.to_bits(),
                self.block.dsp.gate_open_for
            ),
            (
                self.default.dsp.gate_open.to_bits(),
                self.default.dsp.gate_open_for
            ),
            "kept gate position; {}",
            context()
        );
        let after = (
            self.block.dsp.gate_db.to_bits(),
            self.block.dsp.wet.to_bits(),
        );
        self.gate[usize::from(before.0 == after.0)] += 1;
        self.wet[usize::from(before.1 == after.1)] += 1;
        expected_meter
    }
    /// The overridden method ran for every block, and only there.
    fn assert_paths(&self, channels: usize) {
        for ch in 0..2 {
            let blocks = |side: &Side| side.probes[ch].blocks.load(Relaxed);
            assert_eq!(blocks(&self.block) > 0, ch < channels);
            assert_eq!(blocks(&self.default) + blocks(&self.reference), 0);
        }
    }
}

fn microphone(channels: u8) -> Target {
    let mut target = Target::new("mic", "Mic", TargetKind::Mic, channels);
    target.suppression.enabled = true;
    target.suppression.wet = 0.79;
    target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.61);
    target
}

/// A mono microphone goes through validation. Validation admits no stereo
/// one, so that state is built past it to keep the merged score checked.
fn prepare(target: &Target) -> Prepared {
    try_prepare(target).unwrap()
}

/// None for settings that validation refuses.
fn try_prepare(target: &Target) -> Option<Prepared> {
    if target.channels == 1 {
        return Prepared::new(target, 48_000).ok();
    }
    // Everything but the channel count is validated all the same.
    let mono = Target {
        channels: 1,
        ..target.clone()
    };
    mono.validate().ok()?;
    Prepared::validated(target, target.suppression.geometry()).ok()
}

/// Input with a few values no source should send: one in about `rare`.
fn input(r: &mut Rng, frames: usize, channels: u8, amplitude: f64, rare: u64) -> [Vec<f32>; 2] {
    std::array::from_fn(|ch| {
        if ch >= channels as usize {
            return Vec::new();
        }
        (0..frames)
            .map(|_| {
                let value = (r.between(-1.0, 1.0) * amplitude) as f32;
                match r.below(rare) {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => f32::NEG_INFINITY,
                    3 => -0.0,
                    4 => 1e-42,
                    5 => 17.0,
                    6 => -16.5,
                    7 => f32::MAX,
                    8 => f32::from_bits(0xff80_0001),
                    9 => 16.0,
                    _ => value,
                }
            })
            .collect()
    })
}

/// A long run with fixed edits: the gate turned off, strict and back, the
/// wet gain moved and left to settle, bypass, and the other stages'
/// settings. Block lengths go through every length from 1 to `BLOCK`, then
/// lengths `Dsp` has to cut, then the device period alone so that every
/// smoother stops.
fn scripted(channels: u8, providers: Providers, order: [Effect; 6]) -> [[u64; 2]; 2] {
    let mut target = microphone(channels);
    target.order = order.to_vec();
    target.eq.enabled = true;
    target.eq.bands = bands(4, false);
    let mut p = prepare(&target);
    let mut trio = Trio::new(&p, providers);
    let mut r = Rng(0x0123_4567_89AB_CDEF ^ providers.stretch);
    let mut total = Meter::default();
    let mut lengths = [0_u64; BLOCK + 1];
    for block in 0..2600 {
        let before = target.clone();
        match block {
            150 => target.suppression.wet = 0.25,
            260 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(1.0),
            400 => target.bypass = true,
            470 => target.bypass = false,
            520 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.0),
            640 => target.suppression.wet = 0.0,
            700 => target.suppression.wet = 1.0,
            760 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.3),
            800 => target.compressor.enabled = true,
            840 => target.limiter.enabled = true,
            880 => target.highpass.enabled = true,
            920 => target.gain.db = 9.0,
            960 => target.eq.bands[1].gain = 12.0,
            1000 => {
                target.suppression.gate.set_advanced(true);
                target.suppression.gate.reduction = 48.0;
                target.suppression.gate.hold = 4.0;
                target.suppression.gate.release = 30.0;
            }
            1100 => target.suppression.wet = 0.6,
            1200 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.61),
            _ => {}
        }
        if target != before {
            assert!(!p.requires_reset(&prepare(&target)));
            p = prepare(&target);
        }
        let frames = match block {
            0..=127 => block + 1,
            128..=1299 => [112, 129, 300, 7, 240, 128, 1, 480, 113][block % 9],
            _ => 112,
        };
        if frames <= BLOCK {
            lengths[frames] += 1;
        }
        let amplitude = if block % 50 < 40 { 0.5 } else { 0.0 };
        let rare = if block < 1300 { 200 } else { 100_000 };
        let input = input(&mut r, frames, channels, amplitude, rare);
        let meter = trio.check(&p, &input, block % 2 == 0, &|| {
            format!("channels {channels} block {block} frames {frames}")
        });
        total.clipped += meter.clipped;
        total.voice = total.voice.max(meter.voice);
        total.gate_open = total.gate_open.max(meter.gate_open);
    }
    trio.assert_paths(channels as usize);
    assert!(lengths[1..].iter().all(|count| *count > 0));
    assert!(total.clipped > 0);
    assert!(total.voice > 0.0);
    assert_eq!(total.gate_open, 1.0);
    [trio.gate, trio.wet]
}

/// Every run must have had blocks that moved the gate and the wet gain and
/// blocks that left them alone.
fn assert_moving_and_steady(runs: &[[[u64; 2]; 2]]) {
    for (run, [gate, wet]) in runs.iter().enumerate() {
        eprintln!("run {run}: gate (moving, steady) {gate:?}, wet {wet:?}");
        assert!(
            gate.iter().chain(wet).all(|blocks| *blocks >= 100),
            "run {run}: gate {gate:?} wet {wet:?}"
        );
    }
}

const SUPPRESSION_FIRST: [Effect; 6] = [
    Effect::Suppression,
    Effect::Highpass,
    Effect::Eq,
    Effect::Compressor,
    Effect::Gain,
    Effect::Limiter,
];
const SUPPRESSION_LAST: [Effect; 6] = [
    Effect::Eq,
    Effect::Gain,
    Effect::Highpass,
    Effect::Compressor,
    Effect::Limiter,
    Effect::Suppression,
];

#[test]
fn a_mono_microphone_matches_with_a_provider_block_method() {
    let runs = [
        (1, false, SUPPRESSION_FIRST),
        (40, true, SUPPRESSION_FIRST),
        (400, false, SUPPRESSION_LAST),
    ]
    .map(|(stretch, wild, order)| {
        let providers = Providers {
            stretch,
            offset: 0,
            wild,
        };
        scripted(1, providers, order)
    });
    assert_moving_and_steady(&runs);
}

#[test]
fn a_stereo_microphone_matches_with_a_provider_block_method() {
    let runs = [
        (1, true, SUPPRESSION_FIRST),
        (40, false, SUPPRESSION_LAST),
        (400, true, SUPPRESSION_FIRST),
    ]
    .map(|(stretch, wild, order)| {
        let providers = Providers {
            stretch,
            offset: 0,
            wild,
        };
        scripted(2, providers, order)
    });
    assert_moving_and_steady(&runs);
}

/// Enabling or disabling suppression is not a live edit: the state has to be
/// rebuilt, and each new state starts new providers.
#[test]
fn suppression_switched_off_and_on_restarts_and_matches() {
    let providers = Providers {
        stretch: 3,
        offset: 0,
        wild: true,
    };
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    let mut target = microphone(1);
    let mut p = prepare(&target);
    let mut trio = Trio::new(&p, providers);
    for round in 0..6 {
        for block in 0..120 {
            let frames = 1 + r.below(BLOCK as u64 + 40) as usize;
            let input = input(&mut r, frames, 1, 0.7, 300);
            if target.suppression.enabled {
                trio.check(&p, &input, block % 2 == 0, &|| {
                    format!("round {round} block {block} frames {frames}")
                });
            }
        }
        if target.suppression.enabled {
            trio.assert_paths(1);
        }
        target.suppression.enabled = !target.suppression.enabled;
        let next = Prepared::new(&target, 48_000).unwrap();
        assert!(p.requires_reset(&next));
        p = next;
        if target.suppression.enabled {
            trio = Trio::new(&p, providers);
        } else {
            let [left, right] = &mut [vec![0.0; 16], Vec::new()];
            assert_eq!(
                trio.block.dsp.process_in_place(&p, [left, right]),
                Err(DspError::IncompatibleSnapshot)
            );
        }
    }
}

/// `RC2FX_DSP_FUZZ_CASES` sets the number of cases.
#[test]
fn random_settings_edits_and_input_match_with_a_provider_block_method() {
    let cases = std::env::var("RC2FX_DSP_FUZZ_CASES")
        .ok()
        .map(|value| value.parse().expect("RC2FX_DSP_FUZZ_CASES is a number"))
        .unwrap_or(if cfg!(debug_assertions) { 40 } else { 300 });
    let mut r = Rng(0x94D0_49BB_1331_11EB);
    let (mut clipped, mut gate, mut wet) = (0, [0; 2], [0; 2]);
    for case in 0..cases {
        let channels = 1 + r.below(2) as u8;
        let mut target = microphone(channels);
        target.suppression.wet = r.unit();
        target.suppression.gate = rc2fx_core::VoiceGate::with_strength(r.unit());
        target.eq.bands = bands(r.below(6) as usize, false);
        for i in (1..6).rev() {
            target.order.swap(i, r.below(i as u64 + 1) as usize);
        }
        for _ in 0..8 {
            edit(&mut r, &mut target);
        }
        let Some(mut p) = try_prepare(&target) else {
            continue;
        };
        let providers = Providers {
            stretch: [1, 2, 9, 60, 500][r.below(5) as usize],
            offset: r.below(14),
            wild: r.below(3) == 0,
        };
        let mut trio = Trio::new(&p, providers);
        let amplitude = [0.0, 1e-30, 0.05, 0.5, 1.0, 15.9, 16.0, 40.0, 3e38][r.below(9) as usize];
        let rare = [30, 400, 1_000_000][r.below(3) as usize];
        let rate = [2, 25, 400][r.below(3) as usize];
        for block in 0..80 + r.below(200) {
            if r.below(rate) == 0 {
                let mut next = target.clone();
                for _ in 0..1 + r.below(3) {
                    if r.below(4) == 0 {
                        // A gate quick enough to move within a few blocks.
                        let gate = &mut next.suppression.gate;
                        gate.set_advanced(true);
                        gate.reduction = r.between(3.0, 60.0);
                        gate.hold = r.between(0.0, 40.0);
                        gate.release = r.between(30.0, 200.0);
                    } else {
                        edit(&mut r, &mut next);
                    }
                }
                if let Some(prepared) = try_prepare(&next) {
                    (target, p) = (next, prepared);
                }
            }
            let frames = match r.below(8) {
                0 => 112,
                1 => 1 + r.below(300) as usize,
                _ => 1 + r.below(BLOCK as u64) as usize,
            };
            let input = input(&mut r, frames, channels, amplitude, rare);
            let meter = trio.check(&p, &input, block % 2 == 0, &|| {
                format!("case {case} block {block} frames {frames} target {target:?}")
            });
            clipped += meter.clipped;
        }
        trio.assert_paths(channels as usize);
        for i in 0..2 {
            gate[i] += trio.gate[i];
            wet[i] += trio.wet[i];
        }
    }
    eprintln!("cases={cases} clipped={clipped} gate(moving,steady)={gate:?} wet={wet:?}");
    assert!(clipped > 0);
    assert!(gate.iter().chain(&wet).all(|blocks| *blocks > 0));
}

/// The stage on its own, with samples no earlier stage would leave: not
/// finite, above 16, beyond an f32. Each sample must get `safe`, both
/// conversions, its own wet value and the merge exactly as `frame` writes
/// them, whatever the provider's form.
#[test]
fn the_stage_limits_converts_and_merges_like_the_per_sample_calls() {
    let mut r = Rng(0xA076_1D64_78BD_642F);
    let mut clipped = 0;
    for round in 0..3000 {
        let n = 1 + r.below(BLOCK as u64) as usize;
        let spoiled = r.below(3) != 0;
        let first = r.below(2) == 0;
        let providers = Providers {
            stretch: 1 + r.below(3),
            offset: r.below(14),
            wild: r.below(2) == 0,
        };
        let probes: [Arc<Probe>; 3] = Default::default();
        let mut by_block = providers.boxed(1, true, &probes[0]);
        let mut by_default = providers.boxed(1, false, &probes[1]);
        let mut by_sample = providers.boxed(1, false, &probes[2]);
        // Several blocks, so the providers are past their start.
        for step in 0..1 + r.below(12) {
            let samples: Vec<f64> = (0..n)
                .map(|_| {
                    if spoiled {
                        let scale = [16.0, 3.5e38, 1.0][r.below(3) as usize];
                        adversarial(&mut r, scale)
                    } else {
                        r.between(-16.0, 16.0)
                    }
                })
                .collect();
            let wet: Vec<f32> = (0..n)
                .map(|_| match r.below(30) {
                    0 => f32::NAN,
                    1 => -0.0,
                    2 => 1.0,
                    3 => 0.0,
                    _ => r.unit() as f32,
                })
                .collect();
            let so_far: Vec<Option<f32>> = (0..n)
                .map(|_| match r.below(6) {
                    0 => None,
                    1 => Some(f32::NAN),
                    2 => Some(-0.0),
                    3 => Some(0.0),
                    _ => Some(r.unit() as f32),
                })
                .collect();
            // The per-sample calls, as `frame` makes them.
            let mut expected = samples.clone();
            let mut expected_voice = so_far.clone();
            let mut expected_meter = Meter::default();
            for t in 0..n {
                expected[t] = by_sample
                    .process_sample(safe(expected[t], 16.0, &mut expected_meter) as f32, wet[t])
                    as f64;
                expected_voice[t] = merge_voice(first, expected_voice[t], by_sample.voice());
            }
            let bits = |x: &[f64]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
            let voice_bits =
                |x: &[Option<f32>]| x.iter().map(|v| v.map(f32::to_bits)).collect::<Vec<_>>();
            for (name, provider) in [("block", &mut by_block), ("default", &mut by_default)] {
                let mut actual = samples.clone();
                let mut voice = so_far.clone();
                let mut meter = Meter::default();
                suppress(
                    provider.as_mut(),
                    &mut actual,
                    &wet,
                    first,
                    &mut voice,
                    &mut meter,
                );
                let context = format!("{name} round {round} step {step} n {n} first {first}");
                assert_eq!(bits(&actual), bits(&expected), "{context}");
                assert_eq!(
                    voice_bits(&voice),
                    voice_bits(&expected_voice),
                    "voice; {context}"
                );
                assert_eq!(meter_bits(&meter), meter_bits(&expected_meter), "{context}");
            }
            assert_eq!(probes[0].bits(), probes[2].bits(), "round {round}");
            assert_eq!(probes[1].bits(), probes[2].bits(), "round {round}");
            clipped += expected_meter.clipped;
        }
    }
    assert!(clipped > 10_000, "samples that needed limiting: {clipped}");
}

fn chat_target() -> Target {
    let mut target = Target::new("send-chat", "Chat", TargetKind::Output, 2);
    target.suppression.enabled = true;
    target.suppression.wet = 0.79;
    target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.61);
    target
}

/// `Chat` hands its voice model a block at a time too. The block form, the
/// default method and `Chat::process_reference`, which makes the per-sample
/// calls between each sample's other steps, must leave the same mix, meter
/// and state.
#[test]
fn chat_matches_its_per_sample_reference_with_either_provider_form() {
    let (mut voice, mut open, mut shut) = (0.0_f64, 0_u32, 0_u32);
    for (stretch, wild) in [(1, false), (7, true), (300, false)] {
        let providers = Providers {
            stretch,
            offset: 0,
            wild,
        };
        let mut target = chat_target();
        let mut p = Prepared::new(&target, 48_000).unwrap();
        let probes: [Arc<Probe>; 3] = Default::default();
        let mut chats: Vec<Chat> = (0..3)
            .map(|i| Chat::new(&p, providers.boxed(0, i == 0, &probes[i])).unwrap())
            .collect();
        let mut r = Rng(0x2545_F491_4F6C_DD1D ^ stretch);
        for block in 0..900 {
            let before = target.clone();
            match block {
                100 => target.suppression.wet = 0.2,
                200 => target.bypass = true,
                260 => target.bypass = false,
                330 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(1.0),
                450 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.0),
                520 => target.suppression.wet = 1.0,
                600 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.5),
                650 => target.eq.enabled = true,
                700 => target.limiter.enabled = true,
                _ => {}
            }
            if target != before {
                p = Prepared::new(&target, 48_000).unwrap();
            }
            let frames = match block {
                0..=127 => block + 1,
                128..=600 => [112, 129, 300, 7, 240, 128, 1, 480, 113][block % 9],
                _ => 112,
            };
            let amplitude = if block % 40 < 30 { 0.4 } else { 0.0 };
            // The stem is checked for finite values; the mix is not.
            let stem = input(&mut r, frames, 2, amplitude, u64::MAX);
            let mix = input(&mut r, frames, 2, 0.5, 150);
            let mut expected = mix.clone();
            let [left, right] = &mut expected;
            let expected_meter = chats[2]
                .process_reference(&p, [&stem[0], &stem[1]], [left, right])
                .unwrap();
            let expected_state = chats[2].state_bits();
            for (i, name) in ["block", "default"].into_iter().enumerate() {
                let mut output = mix.clone();
                let [left, right] = &mut output;
                let meter = chats[i]
                    .process(&p, [&stem[0], &stem[1]], [left, right])
                    .unwrap();
                let context = format!("{name} stretch {stretch} block {block} frames {frames}");
                for ch in 0..2 {
                    let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                    assert_eq!(bits(&output[ch]), bits(&expected[ch]), "ch{ch}; {context}");
                }
                assert_eq!(meter_bits(&meter), meter_bits(&expected_meter), "{context}");
                assert_eq!(chats[i].state_bits(), expected_state, "state; {context}");
                assert_eq!(probes[i].bits(), probes[2].bits(), "provider; {context}");
            }
            voice = voice.max(expected_meter.voice);
            open += u32::from(expected_meter.gate_open == 1.0);
            shut += u32::from(expected_meter.gate_open < 0.5);
        }
        assert!(probes[0].blocks.load(Relaxed) > 0);
        assert_eq!(
            probes[1].blocks.load(Relaxed) + probes[2].blocks.load(Relaxed),
            0
        );
    }
    assert!(
        voice >= 0.9 && open >= 50 && shut >= 50,
        "{voice} {open} {shut}"
    );
}
