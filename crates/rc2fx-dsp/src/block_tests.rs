//! The block path against the per-sample reference, bit for bit.
//!
//! `Dsp::frame` (with `EqBank::process`, `Filter::process`, `safe` and
//! `gate_step_reference`) is the processing as it was written before the
//! block checks, one sample at a time. Every test here runs the same input
//! through `Dsp::process` or `Dsp::process_in_place` and through that
//! reference, and compares the output samples, all eight meter fields and the
//! whole processing state as bits.
use super::*;

pub(crate) struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
}

/// A delay-only stand-in for the suppression worker. Its voice score walks
/// through voiced, unvoiced, missing and NaN stretches so the gate opens,
/// holds and closes; `wild` also makes it return values no worker should.
struct Voice {
    geometry: SuppressionGeometry,
    line: Vec<f32>,
    at: usize,
    count: u32,
    stretch: u32,
    wild: bool,
}
impl Voice {
    fn boxed(stretch: u32, wild: bool) -> Box<dyn Suppression> {
        let geometry = rc2fx_core::Suppression::admitted_geometry(0).unwrap();
        Box::new(Self {
            geometry,
            line: vec![0.0; geometry.latency_samples() as usize],
            at: 0,
            count: 0,
            stretch,
            wild,
        })
    }
}
impl Suppression for Voice {
    fn geometry(&self) -> SuppressionGeometry {
        self.geometry
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        let dry = self.line[self.at];
        self.line[self.at] = input;
        self.at = (self.at + 1) % self.line.len();
        self.count = self.count.wrapping_add(1);
        let out = dry + (dry * 0.5 - dry) * wet;
        if !self.wild {
            return out;
        }
        match self.count % 1013 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => 40.0,
            3 => -1e30,
            4 => f32::from_bits(1),
            _ => out,
        }
    }
    fn voice(&self) -> Option<f32> {
        match (self.count / self.stretch) % 7 {
            0 => Some(0.9),
            1 => Some(0.05),
            2 => None,
            3 => Some(0.4),
            4 => Some(f32::NAN),
            5 => Some(0.62),
            _ => Some(if self.count.is_multiple_of(2) {
                0.8
            } else {
                0.1
            }),
        }
    }
    fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
        Ok(())
    }
}

/// Everything `block` and `frame` leave behind, as bits.
pub(super) fn state(dsp: &Dsp) -> Vec<u64> {
    let mut bits = vec![
        dsp.gain.to_bits(),
        dsp.wet.to_bits(),
        dsp.processing_blend.to_bits(),
        dsp.envelope.to_bits(),
        dsp.limiter_gain.to_bits(),
        dsp.gate_db.to_bits(),
        dsp.gate_gain.to_bits(),
        u64::from(dsp.gate_hold),
        u64::from(dsp.gate_voiced),
        dsp.eq_current as u64,
        dsp.eq_remaining as u64,
    ];
    for channel in &dsp.channels {
        bits.push(channel.bypass_position as u64);
        bits.extend(channel.bypass_dry.iter().map(|v| u64::from(v.to_bits())));
        bits.extend(channel.hp.bits());
    }
    for bank in &dsp.eq {
        bits.extend(bank.bits());
    }
    bits
}

pub(super) fn meter_bits(m: &Meter) -> [u64; 8] {
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

/// One block through the reference: the full `begin_block`, then `frame`
/// for each sample.
pub(super) fn reference_block(
    dsp: &mut Dsp,
    p: &Prepared,
    input: &[Vec<f32>; 2],
) -> ([Vec<f32>; 2], Meter) {
    let frames = input[0].len();
    // The reference prepares every block in full.
    dsp.seen = 0;
    dsp.begin_block(p, frames);
    let mut output = [vec![0.0; frames], vec![0.0; input[1].len()]];
    let mut meter = Meter::default();
    for n in 0..frames {
        let y = dsp.frame(
            p,
            [
                input[0][n],
                if p.channels() == 2 { input[1][n] } else { 0.0 },
            ],
            &mut meter,
        );
        for ch in 0..p.channels() {
            output[ch][n] = y[ch];
        }
    }
    (output, meter)
}

/// Runs `input` through both paths and asserts that nothing differs. Returns
/// the block's meter.
fn check(
    actual: &mut Dsp,
    reference: &mut Dsp,
    p: &Prepared,
    input: &[Vec<f32>; 2],
    in_place: bool,
    context: &dyn Fn() -> String,
) -> Meter {
    let (expected, expected_meter) = reference_block(reference, p, input);
    let mut output = if in_place {
        input.clone()
    } else {
        [vec![0.0; input[0].len()], vec![0.0; input[1].len()]]
    };
    let [left, right] = &mut output;
    let meter = if in_place {
        actual.process_in_place(p, [left, right]).unwrap()
    } else {
        actual
            .process(p, [&input[0], &input[1]], [left, right])
            .unwrap()
    };
    for ch in 0..p.channels() {
        for (n, (got, want)) in output[ch].iter().zip(&expected[ch]).enumerate() {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "sample ch{ch} n{n}: {got} != {want}; {}",
                context()
            );
        }
    }
    assert_eq!(
        meter_bits(&meter),
        meter_bits(&expected_meter),
        "meter {meter:?} != {expected_meter:?}; {}",
        context()
    );
    assert_eq!(state(actual), state(reference), "state; {}", context());
    meter
}

fn pair(p: &Prepared, voice: Option<(u32, bool)>) -> (Dsp, Dsp) {
    let make = || match voice {
        Some((stretch, wild)) => Dsp::new(p, [Some(Voice::boxed(stretch, wild)), None]).unwrap(),
        None => Dsp::without_suppression(p).unwrap(),
    };
    (make(), make())
}

pub(super) fn bands(count: usize, right: bool) -> Vec<Band> {
    (0..count)
        .map(|i| Band {
            enabled: true,
            band_type: [BandType::Bell, BandType::LowShelf, BandType::HighShelf][i % 3],
            frequency: (40.0 * 1.45_f64.powi(i as i32)).min(18_000.0)
                + if right { 9.0 } else { 0.0 },
            gain: if i.is_multiple_of(2) { 3.0 } else { -2.5 },
            q: 0.7 + 0.1 * i as f64,
            ..Band::new(format!("band-{i}"))
        })
        .collect()
}

/// The headphone path's shape: a stereo output with an EQ and nothing else.
fn eq_output(channels: u8, count: usize) -> Target {
    let mut t = Target::new("out", "Out", TargetKind::Output, channels);
    t.eq.enabled = true;
    t.eq.input_gain_db = -6.0;
    t.eq.bands = bands(count, false);
    t
}

fn mic(wet: f64, strength: f64) -> Target {
    let mut t = Target::new("mic", "Mic", TargetKind::Mic, 1);
    t.suppression.enabled = true;
    t.suppression.wet = wet;
    t.suppression.gate = rc2fx_core::VoiceGate::with_strength(strength);
    t
}

fn tone(block: usize, frames: usize, channels: u8, amplitude: f32) -> [Vec<f32>; 2] {
    std::array::from_fn(|ch| {
        if ch < channels as usize {
            (0..frames)
                .map(|n| ((block * 131 + n) as f32 * (0.031 + ch as f32 * 0.007)).sin() * amplitude)
                .collect()
        } else {
            Vec::new()
        }
    })
}

const LENGTHS: [usize; 7] = [112, 1, 128, 129, 300, 112, 7];

/// Runs `blocks` blocks, applying `edit` before each one and `spoil` to each
/// block's input, alternating the two entry points. Returns the summed
/// counters so a test can show which path it reached.
fn run(
    mut target: Target,
    voice: Option<(u32, bool)>,
    blocks: usize,
    amplitude: f32,
    edit: impl Fn(usize, &mut Target),
    spoil: impl Fn(usize, &mut [Vec<f32>; 2]),
    prime: impl Fn(&mut Dsp),
) -> Meter {
    let mut p = Prepared::new(&target, 48_000).unwrap();
    let (mut actual, mut reference) = pair(&p, voice);
    prime(&mut actual);
    prime(&mut reference);
    let mut total = Meter::default();
    for block in 0..blocks {
        // The snapshot is held between edits, as the mixer holds it.
        let before = target.clone();
        edit(block, &mut target);
        if target != before {
            p = Prepared::new(&target, 48_000).unwrap();
        }
        let frames = LENGTHS[block % LENGTHS.len()];
        let mut input = tone(block, frames, target.channels, amplitude);
        spoil(block, &mut input);
        let meter = check(
            &mut actual,
            &mut reference,
            &p,
            &input,
            block % 2 == 0,
            &|| format!("block {block} frames {frames}"),
        );
        total.clipped += meter.clipped;
        total.samples += meter.samples;
        total.output_peak = total.output_peak.max(meter.output_peak);
        total.input_peak = total.input_peak.max(meter.input_peak);
        total.gain_reduction = total.gain_reduction.max(meter.gain_reduction);
        total.gate_open = total.gate_open.max(meter.gate_open);
        total.voice = total.voice.max(meter.voice);
    }
    total
}

fn nothing(_: usize, _: &mut Target) {}
fn clean(_: usize, _: &mut [Vec<f32>; 2]) {}
fn unprimed(_: &mut Dsp) {}

#[test]
fn max_abs_bits_orders_magnitudes_and_puts_every_non_finite_value_on_top() {
    let limit = 16.0_f64;
    let above = f64::from_bits(limit.to_bits() + 1);
    let below = f64::from_bits(limit.to_bits() - 1);
    let within = |values: &[f64]| max_abs_bits(values) <= limit.to_bits();
    assert_eq!(max_abs_bits(&[]), 0);
    assert!(within(&[]));
    assert!(within(&[limit]));
    assert!(within(&[-limit]));
    assert!(within(&[below, -below, 0.0, -0.0]));
    assert!(!within(&[above]));
    assert!(!within(&[-above]));
    assert!(!within(&[f64::INFINITY]));
    assert!(!within(&[f64::NEG_INFINITY]));
    assert!(!within(&[f64::NAN]));
    assert!(!within(&[-f64::NAN]));
    // Signalling NaNs, either sign.
    assert!(!within(&[f64::from_bits(0x7ff0_0000_0000_0001)]));
    assert!(!within(&[f64::from_bits(0xfff0_0000_0000_0001)]));
    assert!(!within(&[f64::MAX]));
    assert!(!within(&[f64::MIN]));
    // Negative zero and subnormals are small, not large.
    assert_eq!(max_abs_bits(&[-0.0]), 0);
    assert_eq!(max_abs_bits(&[f64::from_bits(1), -f64::from_bits(3)]), 3);
    assert!(within(&[f64::MIN_POSITIVE, -f64::MIN_POSITIVE / 2.0]));
    // The result is the largest magnitude itself, wherever it sits.
    for at in 0..9 {
        let mut values = [0.25, -0.5, 1e-300, -0.0, 0.75, -0.125, 0.0, 0.5, -0.25];
        values[at] = -3.5;
        assert_eq!(f64::from_bits(max_abs_bits(&values)), 3.5);
        values[at] = f64::NAN;
        assert!(!within(&values));
    }
    // The same holds at the other limits the block path uses.
    for limit in [1.0_f64, EQ_INTERNAL_LIMIT] {
        assert!(max_abs_bits(&[limit, -limit]) <= limit.to_bits());
        assert!(max_abs_bits(&[f64::from_bits(limit.to_bits() + 1)]) > limit.to_bits());
        assert!(max_abs_bits(&[f64::INFINITY]) > limit.to_bits());
        assert!(max_abs_bits(&[f64::NAN]) > limit.to_bits());
    }
}

pub(super) fn adversarial(r: &mut Rng, scale: f64) -> f64 {
    match r.below(40) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => f64::NEG_INFINITY,
        3 => -0.0,
        4 => f64::from_bits(r.below(1 << 20)),
        5 => scale,
        6 => -scale,
        7 => f64::from_bits(scale.to_bits() + 1),
        8 => -f64::from_bits(scale.to_bits() + 1),
        9 => f64::from_bits(scale.to_bits() - 1),
        10 => f64::from_bits(0x7ff0_0000_0000_0001 | (r.next() >> 13)),
        11 => f64::MAX,
        12 => 0.0,
        _ => r.between(-1.0, 1.0) * scale * [0.01, 0.5, 0.999, 1.5][r.below(4) as usize],
    }
}

#[test]
fn clamp_is_safe_on_every_sample_for_clean_and_spoiled_blocks() {
    let mut r = Rng(0x243F_6A88_85A3_08D3);
    let mut fell_back = 0;
    for round in 0..4000 {
        let limit = [1.0, 16.0, EQ_INTERNAL_LIMIT][round % 3];
        let n = r.below(BLOCK as u64 + 1) as usize;
        let spoiled = r.below(3) == 0;
        let input: Vec<f64> = (0..n)
            .map(|_| {
                if spoiled {
                    adversarial(&mut r, limit)
                } else {
                    r.between(-1.0, 1.0) * limit
                }
            })
            .collect();
        let mut expected_meter = Meter::default();
        let expected: Vec<u64> = input
            .iter()
            .map(|x| safe(*x, limit, &mut expected_meter).to_bits())
            .collect();
        let mut actual = input.clone();
        let mut meter = Meter::default();
        clamp(&mut actual, limit, &mut meter);
        let actual: Vec<u64> = actual.iter().map(|x| x.to_bits()).collect();
        assert_eq!(actual, expected, "round {round}");
        assert_eq!(meter, expected_meter, "round {round}");
        fell_back += usize::from(meter.clipped != 0);
    }
    assert!(fell_back > 500, "the per-sample path must run: {fell_back}");
}

/// The smoother as `frame` runs it: one step per sample.
fn recurrence(value: &mut f64, target: f64, n: usize) -> Vec<u64> {
    (0..n)
        .map(|_| {
            *value += GAIN_SMOOTH * (target - *value);
            value.to_bits()
        })
        .collect()
}

fn ramp_bits(ramp: &Ramp, n: usize) -> Vec<u64> {
    let mut bits = Vec::new();
    ramp.each(n, |t, value| {
        assert_eq!(t, bits.len());
        assert_eq!(value.to_bits(), ramp.at(t).to_bits());
        bits.push(value.to_bits());
    });
    bits
}

#[test]
fn a_ramp_is_the_per_sample_smoother_moving_stalled_or_settled() {
    // Where a smoother rising to 1 stops: short of 1, and it stays there.
    let mut stalled = 0.0;
    for _ in 0..40_000 {
        stalled += GAIN_SMOOTH * (1.0 - stalled);
    }
    assert!(stalled < 1.0, "this smoother stalls below its target");
    assert_eq!(
        (stalled + GAIN_SMOOTH * (1.0 - stalled)).to_bits(),
        stalled.to_bits()
    );
    let mut r = Rng(0x1319_8A2E_0370_7344);
    let mut cases: Vec<(f64, f64)> = vec![
        (stalled, 1.0),
        (1.0, 1.0),
        (0.0, 0.0),
        // One more step turns negative zero into positive zero.
        (-0.0, 0.0),
        (0.0, 1.0),
        (1.0, 0.0),
        (f64::from_bits(stalled.to_bits() - 1), 1.0),
        (f64::from_bits(1.0_f64.to_bits() + 1), 1.0),
        (f64::from_bits(1), 0.0),
        (f64::from_bits(700), 0.0),
        (f64::MIN_POSITIVE, 0.0),
        (-f64::MIN_POSITIVE, 0.0),
        (3.9810717055349722, 3.9810717055349722),
        (0.001, 0.001),
    ];
    for _ in 0..3000 {
        let target = [
            0.0,
            1.0,
            r.between(0.0, 4.0),
            db_gain(r.between(-60.0, 36.0)),
        ][r.below(4) as usize];
        let start = match r.below(5) {
            0 => target,
            1 => f64::from_bits(target.to_bits().wrapping_add(r.below(5)).wrapping_sub(2)),
            2 => r.between(0.0, 4.0),
            3 => target * (1.0 + r.between(-1e-12, 1e-12)),
            _ => r.between(0.0, 1.0) * 1e-300,
        };
        cases.push((start, target));
    }
    let (mut flat, mut moving) = (0, 0);
    for (start, target) in cases {
        for n in [0, 1, 7, 112, BLOCK] {
            let mut expected_value = start;
            let expected = recurrence(&mut expected_value, target, n);
            let mut value = start;
            let ramp = Ramp::advance(&mut value, target, n);
            assert_eq!(
                ramp_bits(&ramp, n),
                expected,
                "{start:e} -> {target:e}, n {n}"
            );
            assert_eq!(value.to_bits(), expected_value.to_bits());
            match ramp {
                Ramp::Flat(_) => flat += 1,
                Ramp::Moving(_) => moving += 1,
            }
            // Only a ramp that is exactly 1 at every sample may skip its
            // multiply.
            if ramp.is_one() {
                assert_eq!(start.to_bits(), 1.0_f64.to_bits());
                assert!(expected.iter().all(|bits| *bits == 1.0_f64.to_bits()));
            }
        }
    }
    assert!(flat > 1000 && moving > 1000, "flat {flat} moving {moving}");
    let mut value = -0.0_f64;
    assert!(matches!(Ramp::advance(&mut value, 0.0, 4), Ramp::Moving(_)));
    assert_eq!(value.to_bits(), 0.0_f64.to_bits());
}

#[test]
fn input_above_the_limit_and_non_finite_input_match_the_reference() {
    for channels in [1, 2] {
        let total = run(
            eq_output(channels, 15),
            None,
            70,
            0.5,
            nothing,
            |block, input| {
                let n = input[0].len();
                let at = (block * 5) % n;
                let ch = block % input.iter().filter(|v| !v.is_empty()).count();
                match block % 10 {
                    0 => input[ch][at] = 40.0,
                    1 => input[ch][at] = f32::NAN,
                    2 => input[ch][at] = f32::INFINITY,
                    3 => input[ch][at] = f32::NEG_INFINITY,
                    4 => input[ch][at] = -16.000002,
                    5 => input[ch][at] = f32::from_bits(0x7f80_0001),
                    6 => input[ch][at] = f32::MAX,
                    // Exactly at the limit, negative zero and a subnormal
                    // stay on the checked path.
                    7 => input[ch][at] = -16.0,
                    8 => input[ch][at] = -0.0,
                    _ => input[ch][at] = f32::from_bits(1),
                }
            },
            unprimed,
        );
        assert!(total.clipped >= 40, "clipped {}", total.clipped);
        assert_eq!(total.input_peak, f64::from(f32::MAX));
    }
}

#[test]
fn output_above_one_matches_the_reference() {
    for channels in [1, 2] {
        let mut target = eq_output(channels, 6);
        target.gain.enabled = true;
        target.gain.db = 12.0;
        target.eq.input_gain_db = 0.0;
        // Loud blocks clip at the output; quiet ones in between do not.
        let total = run(
            target,
            None,
            120,
            0.6,
            nothing,
            |block, input| {
                if block % 4 >= 2 {
                    for sample in input.iter_mut().flatten() {
                        *sample *= 0.05;
                    }
                }
            },
            unprimed,
        );
        assert!(total.clipped > 1000, "clipped {}", total.clipped);
        assert_eq!(total.output_peak, 1.0);
    }
}

#[test]
fn eq_gain_pushing_samples_past_the_stage_limit_matches_the_reference() {
    for (input_db, output_db) in [(0.0, 36.0), (36.0, 0.0), (36.0, 36.0), (-36.0, 36.0)] {
        let mut target = eq_output(2, 5);
        target.eq.input_gain_db = input_db;
        target.eq.output_gain_db = output_db;
        let total = run(target, None, 40, 0.9, nothing, clean, unprimed);
        if input_db + output_db > 30.0 {
            assert!(total.clipped > 1000, "clipped {}", total.clipped);
        }
    }
}

#[test]
fn unit_gain_with_a_moving_blend_or_wet_matches_the_reference() {
    // Gain is off, so it is exactly 1 throughout while bypass moves the blend.
    let total = run(
        eq_output(2, 15),
        None,
        420,
        0.3,
        |block, target| match block {
            20 | 260 => target.bypass = true,
            60 | 300 => target.bypass = false,
            _ => {}
        },
        clean,
        unprimed,
    );
    assert_eq!(total.clipped, 0);
    // The same on a microphone, where the wet level moves too.
    run(
        mic(0.8, 0.0),
        Some((900, false)),
        300,
        0.3,
        |block, target| match block {
            15 => target.suppression.wet = 0.25,
            40 => target.bypass = true,
            90 => target.bypass = false,
            120 => target.suppression.wet = 1.0,
            _ => {}
        },
        clean,
        unprimed,
    );
}

#[test]
fn a_stalled_blend_and_a_negative_zero_wet_match_the_reference() {
    // A blend parked one ulp either side of where the smoother stalls.
    let mut stalled = 0.0;
    for _ in 0..40_000 {
        stalled += GAIN_SMOOTH * (1.0 - stalled);
    }
    for offset in [-2_i64, -1, 0, 1] {
        let blend = f64::from_bits((stalled.to_bits() as i64 + offset) as u64);
        run(eq_output(2, 4), None, 12, 0.4, nothing, clean, |dsp| {
            dsp.processing_blend = blend
        });
    }
    // A smoother that ran to completion stalls short of 1 by itself.
    let p = Prepared::new(&eq_output(2, 4), 48_000).unwrap();
    let (mut actual, mut reference) = pair(&p, None);
    for dsp in [&mut actual, &mut reference] {
        dsp.processing_blend = 0.0;
    }
    for block in 0..200 {
        let input = tone(block, 112, 2, 0.4);
        check(&mut actual, &mut reference, &p, &input, true, &|| {
            format!("block {block}")
        });
    }
    assert_eq!(actual.processing_blend.to_bits(), stalled.to_bits());
    assert!(actual.processing_blend < 1.0);
    // Negative zero is not a fixed point: its next step is positive zero.
    for (wet, gain) in [(-0.0, 1.0), (0.0, -0.0), (-0.0, -0.0)] {
        let mut target = mic(0.0, 0.5);
        target.gain.enabled = true;
        target.gain.db = -60.0;
        run(target, Some((300, false)), 8, 0.4, nothing, clean, |dsp| {
            dsp.wet = wet;
            dsp.gain = gain;
        });
    }
}

#[test]
fn bypass_toggles_and_eq_fades_match_the_reference() {
    for channels in [1, 2] {
        let total = run(
            eq_output(channels, 9),
            None,
            160,
            0.5,
            |block, target| match block {
                // New shapes: the banks crossfade over the next blocks.
                5 => target.eq.bands[2].enabled = false,
                6 => target.bypass = true,
                7 => target.eq.bands.truncate(6),
                9 => target.bypass = false,
                // Same shape: coefficients smooth to the new values.
                30 => target.eq.bands[1].gain = 9.0,
                31 => target.eq.output_gain_db = 6.0,
                50 => target.eq.enabled = false,
                52 => target.eq.enabled = true,
                70 => target.eq.bands = bands(32, false),
                90 if channels == 2 => target.eq.right_bands = Some(bands(7, true)),
                110 => target.order.reverse(),
                130 => target.eq.input_gain_db = 36.0,
                _ => {}
            },
            |block, input| {
                // A fade that also has to clamp.
                if block == 8 || block == 71 {
                    input[0][0] = f32::NAN;
                }
            },
            unprimed,
        );
        assert!(total.clipped > 0);
    }
}

#[test]
fn a_disabled_eq_mono_and_every_other_stage_match_the_reference() {
    for channels in [1, 2] {
        let mut target = Target::new("out", "Out", TargetKind::Output, channels);
        target.eq.bands = bands(3, false);
        target.eq.input_gain_db = 12.0;
        let total = run(
            target,
            None,
            260,
            0.7,
            |block, target| match block {
                3 => target.highpass.enabled = true,
                20 => target.compressor.enabled = true,
                40 => {
                    target.limiter.enabled = true;
                    target.limiter.ceiling = -9.0;
                }
                60 => {
                    target.gain.enabled = true;
                    target.gain.db = 6.0;
                }
                80 => target.compressor.enabled = false,
                100 => target.gain.db = 0.0,
                140 => target.highpass.enabled = false,
                180 => target.eq.enabled = true,
                200 => target.limiter.enabled = false,
                _ => {}
            },
            |block, input| {
                if block % 37 == 0 {
                    input[0][0] = 30.0;
                }
            },
            unprimed,
        );
        assert!(total.gain_reduction > 0.0);
        assert!(total.clipped > 0);
    }
}

#[test]
fn a_microphone_with_its_delay_ring_and_gate_matches_the_reference() {
    for (stretch, wild) in [(700, false), (333, true), (40, false)] {
        let mut target = mic(1.0, 0.61);
        target.eq.enabled = true;
        target.eq.bands = bands(4, false);
        let total = run(
            target,
            Some((stretch, wild)),
            260,
            0.4,
            |block, target| match block {
                30 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(1.0),
                60 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.0),
                80 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.3),
                100 => target.bypass = true,
                140 => target.bypass = false,
                170 => target.suppression.wet = 0.5,
                200 => target.highpass.enabled = true,
                _ => {}
            },
            |block, input| {
                if block % 29 == 3 {
                    input[0][0] = f32::INFINITY;
                }
            },
            unprimed,
        );
        assert!(total.clipped > 0);
        assert!(total.voice > 0.5);
        assert_eq!(total.gate_open, 1.0);
    }
}

#[test]
fn a_delay_ring_shorter_than_a_block_matches_the_reference() {
    // No worker reports a delay this short. The ring then wraps several times
    // inside one block and hands back samples written earlier in that block.
    for latency in [1, 5, 111, 112, 113, 127] {
        let p = Prepared::new(&mic(0.7, 0.5), 48_000).unwrap();
        let (mut actual, mut reference) = pair(&p, Some((200, false)));
        for dsp in [&mut actual, &mut reference] {
            dsp.latency = latency;
            dsp.channels[0].bypass_dry = vec![0.0; latency].into_boxed_slice();
        }
        for block in 0..24 {
            let frames = LENGTHS[block % LENGTHS.len()];
            let mut input = tone(block, frames, 1, 0.5);
            if block % 5 == 0 {
                input[0][frames / 2] = 20.0;
            }
            check(
                &mut actual,
                &mut reference,
                &p,
                &input,
                block % 2 == 0,
                &|| format!("latency {latency} block {block}"),
            );
        }
    }
}

fn random_target(r: &mut Rng, channels: u8, microphone: bool) -> Target {
    let mut t = if microphone {
        mic(r.unit(), r.unit())
    } else {
        Target::new("fuzz", "Fuzz", TargetKind::Output, channels)
    };
    t.eq.bands = bands(r.below(MAX_BANDS as u64 + 1) as usize, false);
    for _ in 0..12 {
        edit(r, &mut t);
    }
    if !microphone {
        // With a provider the order is part of the signature; without one it
        // may be anything.
        for i in (1..6).rev() {
            t.order.swap(i, r.below(i as u64 + 1) as usize);
        }
    }
    t
}

/// One live edit that keeps the signature: everything a user can change
/// without the state being rebuilt.
pub(super) fn edit(r: &mut Rng, t: &mut Target) {
    let level = |r: &mut Rng| match r.below(5) {
        0 => 0.0,
        1 => 36.0,
        2 => -36.0,
        _ => r.between(-12.0, 12.0),
    };
    match r.below(20) {
        0 => t.bypass = !t.bypass,
        1 => t.highpass.enabled = !t.highpass.enabled,
        2 => t.highpass.frequency = r.between(20.0, 1000.0),
        3 => t.compressor.enabled = !t.compressor.enabled,
        4 => {
            t.compressor.threshold = r.between(-60.0, 0.0);
            t.compressor.ratio = [1.0, 3.0, 20.0, r.between(1.0, 20.0)][r.below(4) as usize];
            t.compressor.makeup = r.between(0.0, 24.0);
            t.compressor.attack = r.between(0.1, 200.0);
            t.compressor.release = r.between(10.0, 2000.0);
        }
        5 => t.gain.enabled = !t.gain.enabled,
        6 => t.gain.db = [0.0, 12.0, -60.0, r.between(-20.0, 12.0)][r.below(4) as usize],
        7 => t.limiter.enabled = !t.limiter.enabled,
        8 => {
            t.limiter.ceiling = r.between(-24.0, 0.0);
            t.limiter.release = [10.0, 100.0, 2000.0][r.below(3) as usize];
        }
        9 => t.eq.enabled = !t.eq.enabled,
        10 => t.eq.input_gain_db = level(r),
        11 => t.eq.output_gain_db = level(r),
        12 if t.channels == 2 => {
            t.eq.right_bands = match r.below(3) {
                0 => None,
                1 => Some(t.eq.bands.clone()),
                _ => Some(bands(r.below(MAX_BANDS as u64 + 1) as usize, true)),
            }
        }
        13 => t.eq.bands = bands(r.below(MAX_BANDS as u64 + 1) as usize, false),
        14 if t.suppression.enabled => {
            t.suppression.wet = [0.0, 1.0, r.unit()][r.below(3) as usize]
        }
        15 if t.suppression.enabled => {
            t.suppression.gate =
                rc2fx_core::VoiceGate::with_strength([0.0, 1.0, r.unit()][r.below(3) as usize])
        }
        _ => {
            if !t.eq.bands.is_empty() {
                let i = r.below(t.eq.bands.len() as u64) as usize;
                let band = &mut t.eq.bands[i];
                match r.below(6) {
                    0 => band.enabled = !band.enabled,
                    1 => band.solo = !band.solo,
                    2 => {
                        band.band_type = BandType::ALL[r.below(BandType::ALL.len() as u64) as usize]
                    }
                    3 => band.gain = r.between(-36.0, 36.0),
                    4 => band.frequency = r.between(20.0, 20_000.0),
                    _ => band.q = r.between(0.1, 8.0),
                }
            }
        }
    }
}

/// The number of random cases: `RC2FX_DSP_FUZZ_CASES`, or a default sized
/// for the build.
fn fuzz_cases() -> u64 {
    std::env::var("RC2FX_DSP_FUZZ_CASES")
        .ok()
        .map(|value| value.parse().expect("RC2FX_DSP_FUZZ_CASES is a number"))
        .unwrap_or(if cfg!(debug_assertions) { 60 } else { 400 })
}

/// Random settings, live edits and input, including values no well-behaved
/// source produces, through both paths. Returns the summed meter and the
/// number of blocks whose gain and blend had settled and had not.
fn fuzz(cases: u64, seed: u64) -> (Meter, u64, u64) {
    let mut r = Rng(seed);
    let mut total = Meter::default();
    let (mut settled, mut moving) = (0_u64, 0_u64);
    for case in 0..cases {
        let microphone = r.below(4) == 0;
        let channels = if microphone { 1 } else { 1 + r.below(2) as u8 };
        let mut target = random_target(&mut r, channels, microphone);
        let Ok(mut p) = Prepared::new(&target, 48_000) else {
            continue;
        };
        let (mut actual, mut reference) = pair(
            &p,
            microphone.then(|| (40 + r.below(2000) as u32, r.below(3) == 0)),
        );
        let amplitude = [
            0.0, 1e-30, 0.05, 0.5, 0.99, 1.0, 3.0, 15.9, 16.0, 40.0, 1e10, 3e38,
        ][r.below(12) as usize];
        let weird = r.below(3) == 0;
        // Some cases edit constantly; others hold still long enough for
        // every smoother to stop.
        let rate = [2, 25, 400][r.below(3) as usize];
        let blocks = 60 + r.below(240);
        for block in 0..blocks {
            if r.below(rate) == 0 {
                let mut next = target.clone();
                for _ in 0..1 + r.below(3) {
                    edit(&mut r, &mut next);
                }
                if let Ok(prepared) = Prepared::new(&next, 48_000) {
                    (target, p) = (next, prepared);
                }
            }
            let frames = [1, 7, 112, 112, 112, 112, 128, 129, 300][r.below(9) as usize];
            let quiet = block % 16 >= 12;
            let input: [Vec<f32>; 2] = std::array::from_fn(|ch| {
                if ch >= channels as usize {
                    return Vec::new();
                }
                (0..frames)
                    .map(|_| {
                        let value =
                            (r.between(-1.0, 1.0) * if quiet { 0.0 } else { amplitude }) as f32;
                        if !weird {
                            return value;
                        }
                        match r.below(400) {
                            0 => f32::NAN,
                            1 => f32::INFINITY,
                            2 => f32::NEG_INFINITY,
                            3 => -0.0,
                            4 => 1e-42,
                            5 => 17.0,
                            6 => -16.0,
                            7 => 1.0,
                            8 => -1.0,
                            9 => f32::MAX,
                            10 => f32::from_bits(0xff80_0001),
                            _ => value,
                        }
                    })
                    .collect()
            });
            let flat = |value: f64, goal: f64| {
                (value + GAIN_SMOOTH * (goal - value)).to_bits() == value.to_bits()
            };
            if flat(actual.gain, if p.gain_on { p.gain } else { 1.0 })
                && flat(actual.processing_blend, if p.bypass { 0.0 } else { 1.0 })
            {
                settled += 1;
            } else {
                moving += 1;
            }
            let meter = check(
                &mut actual,
                &mut reference,
                &p,
                &input,
                block % 2 == 0,
                &|| format!("case {case} block {block} frames {frames} target {target:?}"),
            );
            total.clipped += meter.clipped;
            total.samples += meter.samples;
        }
    }
    (total, settled, moving)
}

/// `RC2FX_DSP_FUZZ_CASES` sets the number of cases.
#[test]
fn random_settings_edits_and_input_match_the_reference() {
    let cases = fuzz_cases();
    let (total, settled, moving) = fuzz(cases, 0x9E37_79B9_7F4A_7C15);
    eprintln!(
        "cases={cases} samples={} clipped={} settledBlocks={settled} movingBlocks={moving}",
        total.samples, total.clipped
    );
    assert!(total.clipped > 0 && settled > 0 && moving > 0);
}

#[test]
fn a_stereo_microphone_with_a_delay_ring_per_channel_matches_the_reference() {
    // Validation admits suppression on a mono microphone or the Chat voice
    // mix only, so no target reaches this today. The state is built past
    // validation to keep the two-channel ring and the merged score checked
    // all the same. `None` keeps the worker's own delay; the short rings
    // wrap inside a block.
    let prepared =
        |target: &Target| Prepared::validated(target, target.suppression.geometry()).unwrap();
    for (latency, wild) in [
        (None, false),
        (None, true),
        (Some(5), false),
        (Some(113), true),
    ] {
        let mut target = Target::new("mic", "Mic", TargetKind::Mic, 2);
        target.suppression.enabled = true;
        target.suppression.wet = 0.8;
        target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.61);
        target.eq.enabled = true;
        target.eq.bands = bands(5, false);
        let mut p = prepared(&target);
        // The channels' scores change at different times, so the gate sees
        // the lower of two different scores.
        let make = |p: &Prepared| {
            let mut dsp = Dsp::new(
                p,
                [Some(Voice::boxed(300, wild)), Some(Voice::boxed(170, wild))],
            )
            .unwrap();
            if let Some(latency) = latency {
                dsp.latency = latency;
                for channel in &mut dsp.channels {
                    channel.bypass_dry = vec![0.0; latency].into_boxed_slice();
                }
            }
            dsp
        };
        let (mut actual, mut reference) = (make(&p), make(&p));
        let mut total = Meter::default();
        for block in 0..180 {
            let before = target.clone();
            match block {
                40 => target.bypass = true,
                80 => target.bypass = false,
                100 => target.suppression.wet = 0.3,
                120 => target.suppression.gate = rc2fx_core::VoiceGate::with_strength(1.0),
                140 => target.eq.bands[1].gain = 12.0,
                _ => {}
            }
            if target != before {
                p = prepared(&target);
            }
            let frames = LENGTHS[block % LENGTHS.len()];
            let mut input = tone(block, frames, 2, 0.5);
            // One channel alone needs limiting, at either end of the block.
            match block % 23 {
                5 => input[1][frames - 1] = f32::NAN,
                7 => input[0][0] = 30.0,
                11 => input[1][0] = f32::NEG_INFINITY,
                _ => {}
            }
            let meter = check(
                &mut actual,
                &mut reference,
                &p,
                &input,
                block % 2 == 0,
                &|| format!("latency {latency:?} wild {wild} block {block} frames {frames}"),
            );
            total.clipped += meter.clipped;
            total.voice = total.voice.max(meter.voice);
            total.gate_open = total.gate_open.max(meter.gate_open);
        }
        assert!(total.clipped > 0);
        assert!(total.voice > 0.5);
        assert_eq!(total.gate_open, 1.0);
        assert_eq!(
            actual.channels[0].bypass_position,
            actual.channels[1].bypass_position
        );
    }
}

/// Runs `body` on a thread of its own with flush-to-zero set, as an audio
/// engine may set it on its callback thread: a result below the normal range
/// becomes zero, and an operand below it is read as zero.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn with_flush_to_zero(body: impl FnOnce() + Send + 'static) {
    use std::hint::black_box;
    let thread = std::thread::spawn(move || {
        // SAFETY: changes the floating-point control register of this thread
        // alone, and the thread ends with `body`.
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let mut fpcr: u64;
            core::arch::asm!("mrs {0}, fpcr", out(reg) fpcr, options(nomem, nostack));
            // FZ.
            fpcr |= 1 << 24;
            core::arch::asm!("msr fpcr, {0}", in(reg) fpcr, options(nomem, nostack));
        }
        // SAFETY: as above, for MXCSR.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let mut mxcsr = 0_u32;
            core::arch::asm!("stmxcsr [{0}]", in(reg) &mut mxcsr, options(nostack));
            // FTZ and DAZ.
            mxcsr |= 0x8040;
            core::arch::asm!("ldmxcsr [{0}]", in(reg) &mxcsr, options(nostack, readonly));
        }
        // The mode is on, and still on afterwards: a subnormal product is
        // zero, and a subnormal operand counts as zero even when multiplied
        // by 1.
        let flushing = || {
            let small = black_box(1e-160_f64);
            let subnormal = black_box(f64::from_bits(1 << 40));
            (small * small).to_bits() == 0 && (subnormal * black_box(1.0)).to_bits() == 0
        };
        assert!(flushing());
        body();
        assert!(flushing());
    });
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}

/// The block path leaves out multiplies by a gain of exactly 1, which the
/// reference performs. Under flush-to-zero such a multiply turns a subnormal
/// sample into zero, so the two paths would part if a subnormal `f64` sample
/// could reach one. None can: every sample is the result of a conversion or
/// an operation that the mode has already flushed.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[test]
fn flush_to_zero_keeps_the_block_path_equal_to_the_reference() {
    with_flush_to_zero(|| {
        let (total, settled, moving) = fuzz(fuzz_cases(), 0x4528_21E6_38D0_1377);
        assert!(total.clipped > 0 && settled > 0 && moving > 0);
        // Input at the bottom of the `f32` range, with every gain at exactly
        // 1 and the smoothers starting from subnormal values, as a state
        // would hold them if the mode came on part-way through.
        for (channels, bypass) in [(1, false), (2, false), (2, true)] {
            let mut target = eq_output(channels, 6);
            target.eq.input_gain_db = 0.0;
            target.highpass.enabled = true;
            target.limiter.enabled = true;
            target.bypass = bypass;
            let p = Prepared::new(&target, 48_000).unwrap();
            assert_eq!(p.eq_input_gain, 1.0);
            let (mut actual, mut reference) = pair(&p, None);
            for dsp in [&mut actual, &mut reference] {
                dsp.processing_blend = f64::from_bits(5000);
                dsp.wet = -f64::from_bits(3);
                dsp.gain = f64::from_bits(1.0_f64.to_bits() - 1);
            }
            let mut r = Rng(0xBE54_66CF_34E9_0C6C);
            for block in 0..400 {
                let frames = LENGTHS[block % LENGTHS.len()];
                let input: [Vec<f32>; 2] = std::array::from_fn(|ch| {
                    if ch >= channels as usize {
                        return Vec::new();
                    }
                    (0..frames)
                        .map(|_| match r.below(6) {
                            0 => f32::from_bits(r.below(1 << 23) as u32),
                            1 => -f32::from_bits(r.below(1 << 23) as u32),
                            2 => f32::MIN_POSITIVE * r.between(-4.0, 4.0) as f32,
                            3 => 0.0,
                            4 => (r.between(-1.0, 1.0) * 1e-30) as f32,
                            _ => r.between(-0.5, 0.5) as f32,
                        })
                        .collect()
                });
                check(
                    &mut actual,
                    &mut reference,
                    &p,
                    &input,
                    block % 2 == 0,
                    &|| format!("channels {channels} bypass {bypass} block {block}"),
                );
            }
        }
    });
}

#[test]
fn a_limiter_parked_below_one_reports_the_reference_reduction() {
    for with_compressor in [false, true] {
        for release in [10.0, 100.0, 2000.0] {
            for ceiling in [-24.0, -6.0, -0.5] {
                let mut target = Target::new("out", "Out", TargetKind::Output, 2);
                target.limiter.enabled = true;
                target.limiter.release = release;
                target.limiter.ceiling = ceiling;
                target.compressor.enabled = with_compressor;
                // Enough makeup that the limiter still has work to do after
                // the compressor.
                target.compressor.threshold = -30.0;
                target.compressor.makeup = 24.0;
                let p = Prepared::new(&target, 48_000).unwrap();
                let (mut actual, mut reference) = pair(&p, None);
                let context = |block: usize| {
                    format!(
                        "compressor {with_compressor} release {release} ceiling {ceiling} block {block}"
                    )
                };
                // Loud, so the limiter reduces.
                let mut reduced = 0.0_f64;
                for block in 0..20 {
                    let input = tone(block, 112, 2, 0.95);
                    let meter = check(&mut actual, &mut reference, &p, &input, true, &|| {
                        context(block)
                    });
                    reduced = reduced.max(meter.gain_reduction);
                }
                assert!(reduced > 0.0);
                assert!(actual.limiter_gain < 1.0);
                if release > 100.0 {
                    // The slowest release takes a minute of audio to stop
                    // moving. Start both paths just short of where it stops.
                    for dsp in [&mut actual, &mut reference] {
                        dsp.limiter_gain = 1.0 - 6.0e-17 / (1.0 - p.limiter_release);
                    }
                }
                // Silent until the gain stops moving. It stops below 1 and
                // stays there: the state the kept logarithm is for.
                let mut still = 0;
                let mut block = 20;
                while still < 30 {
                    let before = actual.limiter_gain;
                    let input = tone(block, 112, 2, 0.0);
                    let meter = check(&mut actual, &mut reference, &p, &input, true, &|| {
                        context(block)
                    });
                    still = if actual.limiter_gain.to_bits() == before.to_bits() {
                        still + 1
                    } else {
                        0
                    };
                    assert!(meter.gain_reduction > 0.0 || with_compressor);
                    block += 1;
                    assert!(block < 2500, "{}", context(block));
                }
                assert!(actual.limiter_gain < 1.0, "{}", context(block));
                assert_eq!(
                    actual.limiter_reduction_for.to_bits(),
                    actual.limiter_gain.to_bits()
                );
                // Loud again from the parked gain, then quiet.
                for block in block..block + 30 {
                    let amplitude = if block % 30 < 20 { 0.95 } else { 0.01 };
                    let input = tone(block, LENGTHS[block % LENGTHS.len()], 2, amplitude);
                    check(
                        &mut actual,
                        &mut reference,
                        &p,
                        &input,
                        block % 2 == 0,
                        &|| context(block),
                    );
                }
            }
        }
    }
}

#[test]
fn the_kept_gate_position_matches_the_reference_step() {
    let gates: Vec<Prepared> = [0.0, 0.3, 0.61, 1.0, 0.61, 0.05]
        .iter()
        .map(|strength| Prepared::new(&mic(1.0, *strength), 48_000).unwrap())
        .chain([0.5, 22.5, 60.0].iter().map(|reduction| {
            // Advanced settings, with chosen depths.
            let mut target = mic(1.0, 0.5);
            target.suppression.gate.set_advanced(true);
            target.suppression.gate.reduction = *reduction;
            target.suppression.gate.hold = 2.0;
            target.suppression.gate.release = 30.0;
            Prepared::new(&target, 48_000).unwrap()
        }))
        .collect();
    let mut r = Rng(0xA409_3822_299F_31D0);
    let (mut actual, mut reference) = pair(&gates[0], Some((100, false)));
    let gate = |dsp: &Dsp| {
        [
            dsp.gate_db.to_bits(),
            dsp.gate_gain.to_bits(),
            u64::from(dsp.gate_hold),
            u64::from(dsp.gate_voiced),
        ]
    };
    let mut p = &gates[0];
    let (mut moved, mut closed, mut opened) = (0, 0, 0);
    let mut score = Some(0.9_f32);
    let mut jitter = false;
    for step in 0..500_000 {
        // Settings change mid-stream, sometimes while the gate is closed or
        // moving; some changes land the level exactly on the new floor.
        if r.below(9000) == 0 {
            p = &gates[r.below(gates.len() as u64) as usize];
            if r.below(3) == 0 && p.gate_on {
                for dsp in [&mut actual, &mut reference] {
                    dsp.gate_db = -p.gate_reduction_db;
                    dsp.gate_gain = db_gain(dsp.gate_db);
                }
            }
        }
        // A score is held for a while, as a model frame holds it: long
        // enough, some of the time, for the gate to close all the way.
        if r.below(12_000) == 0 {
            jitter = false;
            score = match r.below(10) {
                0 => None,
                1 => Some(f32::NAN),
                2 => Some(1.0),
                3 => Some(p.gate_threshold),
                4 => Some(p.gate_close),
                5 => {
                    jitter = true;
                    None
                }
                6 => Some(r.unit() as f32),
                _ => Some(0.0),
            };
        }
        let voice = if jitter { Some(r.unit() as f32) } else { score };
        let mut meter = Meter::default();
        let mut expected_meter = Meter::default();
        if step % 3 == 0 {
            // A meter that already holds values from earlier in its block.
            meter.gate_open = r.unit();
            meter.voice = r.unit();
            expected_meter = meter;
        }
        let before = actual.gate_db;
        let gain = actual.gate_step(p, voice, &mut meter);
        let expected = reference.gate_step_reference(p, voice, &mut expected_meter);
        assert_eq!(gain.to_bits(), expected.to_bits(), "step {step}");
        assert_eq!(
            meter_bits(&meter),
            meter_bits(&expected_meter),
            "step {step}"
        );
        assert_eq!(gate(&actual), gate(&reference), "step {step}");
        moved += usize::from(actual.gate_db != before);
        closed += usize::from(p.gate_on && actual.gate_db == -p.gate_reduction_db);
        opened += usize::from(actual.gate_db == 0.0);
    }
    assert!(
        moved > 10_000 && closed > 10_000 && opened > 10_000,
        "moved {moved} closed {closed} opened {opened}"
    );
    // New settings whose floor is exactly where a half-closed gate stands:
    // the level does not move, but the position it stands for does.
    let p = &gates[2];
    let (mut actual, mut reference) = pair(p, Some((100, false)));
    let mut meter = Meter::default();
    for _ in 0..p.gate_hold + 4000 {
        actual.gate_step(p, Some(0.0), &mut meter);
        reference.gate_step_reference(p, Some(0.0), &mut meter);
    }
    let level = actual.gate_db;
    assert!(level < 0.0 && level > -p.gate_reduction_db, "half closed");
    let mut target = mic(1.0, 0.5);
    target.suppression.gate.set_advanced(true);
    target.suppression.gate.reduction = -level;
    let landed = Prepared::new(&target, 48_000).unwrap();
    for step in 0..3 {
        let mut meter = Meter::default();
        let mut expected_meter = Meter::default();
        let gain = actual.gate_step(&landed, Some(0.0), &mut meter);
        let expected = reference.gate_step_reference(&landed, Some(0.0), &mut expected_meter);
        assert_eq!(actual.gate_db.to_bits(), level.to_bits(), "the level stays");
        assert_eq!(gain.to_bits(), expected.to_bits(), "step {step}");
        assert_eq!(meter_bits(&meter), meter_bits(&expected_meter));
        assert_eq!(meter.gate_open, 0.0, "fully closed for the new floor");
    }
}

/// `gate_run` takes one sample in full and then every sample that would
/// change nothing. Against `gate_step_reference` for each sample: the gain
/// of every sample, the meter and the gate's state, for scores held as long
/// as a model frame holds them, scores on and next to both thresholds,
/// missing and NaN scores, a gate that is off, and settings that change
/// while the gate is open, moving and shut.
#[test]
fn a_gate_run_matches_the_reference_step_for_every_sample() {
    let gates: Vec<Prepared> = [0.0, 0.3, 0.61, 1.0]
        .iter()
        .map(|strength| Prepared::new(&mic(1.0, *strength), 48_000).unwrap())
        .chain(
            [(0.5, 0.0), (22.5, 2.0), (60.0, 0.5)]
                .iter()
                .map(|(reduction, hold)| {
                    let mut target = mic(1.0, 0.5);
                    target.suppression.gate.set_advanced(true);
                    target.suppression.gate.reduction = *reduction;
                    target.suppression.gate.hold = *hold;
                    target.suppression.gate.release = 30.0;
                    Prepared::new(&target, 48_000).unwrap()
                }),
        )
        .collect();
    let mut r = Rng(0x8538_ECB5_BD45_6EA3);
    let (mut actual, mut reference) = pair(&gates[0], Some((100, false)));
    let gate = |dsp: &Dsp| {
        [
            dsp.gate_db.to_bits(),
            dsp.gate_gain.to_bits(),
            u64::from(dsp.gate_hold),
            u64::from(dsp.gate_voiced),
        ]
    };
    let mut p = &gates[0];
    let mut score = Some(0.9_f32);
    let mut left = 0;
    let (mut skipped, mut stepped, mut shut, mut open) = (0_u64, 0_u64, 0_u64, 0_u64);
    for block in 0..40_000 {
        if r.below(60) == 0 {
            p = &gates[r.below(gates.len() as u64) as usize];
            if r.below(3) == 0 && p.gate_on {
                // Settings whose floor is exactly where the gate stands.
                for dsp in [&mut actual, &mut reference] {
                    dsp.gate_db = -p.gate_reduction_db;
                    dsp.gate_gain = db_gain(dsp.gate_db);
                }
            }
        }
        let n = 1 + r.below(BLOCK as u64) as usize;
        let scores: Vec<Option<f32>> = (0..n)
            .map(|_| {
                if left == 0 {
                    left = [1, 3, 240, 240, 2400, 24_000][r.below(6) as usize];
                    score = match r.below(14) {
                        0 => None,
                        1 => Some(f32::NAN),
                        2 => Some(1.0),
                        3 => Some(p.gate_threshold),
                        4 => Some(p.gate_close),
                        5 => Some(f32::from_bits(p.gate_threshold.to_bits() - 1)),
                        6 => Some(f32::from_bits(p.gate_close.to_bits() - 1)),
                        7 => Some(-0.0),
                        8 => Some(f32::INFINITY),
                        9 => Some(r.unit() as f32),
                        10 => Some(0.9),
                        _ => Some(0.0),
                    };
                }
                left -= 1;
                score
            })
            .collect();
        let mut meter = Meter::default();
        if block % 3 == 0 {
            // A meter that already holds values from earlier in its block.
            meter.gate_open = r.unit();
            meter.voice = r.unit();
        }
        let mut expected_meter = meter;
        let expected: Vec<u64> = scores
            .iter()
            .map(|score| {
                reference
                    .gate_step_reference(p, *score, &mut expected_meter)
                    .to_bits()
            })
            .collect();
        let mut gains = Vec::with_capacity(n);
        while gains.len() < n {
            let (gain, run) = actual.gate_run(p, &scores[gains.len()..], &mut meter);
            assert!(run >= 1 && gains.len() + run <= n, "block {block}");
            gains.extend(std::iter::repeat_n(gain.to_bits(), run));
            skipped += run as u64 - 1;
            stepped += 1;
        }
        assert_eq!(gains, expected, "block {block}");
        assert_eq!(
            meter_bits(&meter),
            meter_bits(&expected_meter),
            "block {block}"
        );
        assert_eq!(gate(&actual), gate(&reference), "block {block}");
        shut += u64::from(p.gate_on && actual.gate_db == -p.gate_reduction_db);
        open += u64::from(actual.gate_db == 0.0);
    }
    assert!(
        skipped > 100_000 && stepped > 100_000 && shut > 1000 && open > 1000,
        "skipped {skipped} stepped {stepped} shut {shut} open {open}"
    );
    // A level on its way up that lands exactly on the floor while the hold
    // is still running is not at rest: the next step moves it again.
    let mut p = gates[2].clone();
    let level = -30.0_f64;
    p.gate_reduction_db = -(0.0 + (level - 0.0) * p.gate_attack);
    for dsp in [&mut actual, &mut reference] {
        dsp.gate_db = level;
        dsp.gate_gain = db_gain(level);
        dsp.gate_voiced = false;
        dsp.gate_hold = 50;
    }
    let scores = [Some(0.0_f32); 20];
    let (mut meter, mut expected_meter) = (Meter::default(), Meter::default());
    let expected: Vec<u64> = scores
        .iter()
        .map(|score| {
            reference
                .gate_step_reference(&p, *score, &mut expected_meter)
                .to_bits()
        })
        .collect();
    let (gain, run) = actual.gate_run(&p, &scores, &mut meter);
    assert_eq!(actual.gate_db, -p.gate_reduction_db, "on the floor");
    assert_eq!((gain.to_bits(), run), (expected[0], 1), "and still moving");
    let mut gains = vec![gain.to_bits()];
    while gains.len() < scores.len() {
        let (gain, run) = actual.gate_run(&p, &scores[gains.len()..], &mut meter);
        gains.extend(std::iter::repeat_n(gain.to_bits(), run));
    }
    assert_eq!(gains, expected);
    assert_eq!(meter_bits(&meter), meter_bits(&expected_meter));
    assert_eq!(gate(&actual), gate(&reference));
}

#[test]
fn a_held_snapshot_is_prepared_once_and_again_after_every_change() {
    let mut target = eq_output(2, 15);
    target.highpass.enabled = true;
    let p = Prepared::new(&target, 48_000).unwrap();
    assert_ne!(p.serial, 0);
    assert_eq!(p.clone().serial, p.serial);
    assert_ne!(Prepared::new(&target, 48_000).unwrap().serial, p.serial);
    let (mut actual, mut reference) = pair(&p, None);
    assert_eq!(actual.seen, 0);
    // A block with no samples prepares nothing and records nothing.
    actual.process_in_place(&p, [&mut [], &mut []]).unwrap();
    assert_eq!(actual.seen, 0);
    let mut held = p;
    let mut skipped = 0;
    let mut block = 0;
    let mut run = |actual: &mut Dsp, held: &Prepared, blocks: usize, skipped: &mut usize| {
        for _ in 0..blocks {
            *skipped += usize::from(actual.seen == held.serial);
            let input = tone(block, LENGTHS[block % LENGTHS.len()], 2, 0.4);
            check(
                actual,
                &mut reference,
                held,
                &input,
                block % 2 == 0,
                &|| format!("block {block}"),
            );
            block += 1;
        }
    };
    run(&mut actual, &held, 3, &mut skipped);
    assert_eq!(actual.seen, held.serial);
    assert_eq!(skipped, 2, "every block after the first is skipped");
    // New coefficients, same shape: the filters smooth for a while, and
    // every one of those blocks is prepared in full.
    target.eq.bands[3].gain = -9.0;
    target.highpass.frequency = 200.0;
    held = Prepared::new(&target, 48_000).unwrap();
    run(&mut actual, &held, 1, &mut skipped);
    assert_eq!(actual.seen, 0, "coefficients are smoothing");
    assert!(actual.eq[actual.eq_current].is_smoothing(0));
    let before = skipped;
    run(&mut actual, &held, 400, &mut skipped);
    assert_eq!(actual.seen, held.serial, "smoothing has finished");
    assert!(skipped > before + 100 && skipped < before + 399);
    // A new shape: the banks fade, and the fade is prepared in full too.
    target.eq.bands[5].enabled = false;
    held = Prepared::new(&target, 48_000).unwrap();
    run(&mut actual, &held, 1, &mut skipped);
    assert_ne!(actual.eq_remaining, 0);
    assert_eq!(actual.seen, 0, "a fade is running");
    run(&mut actual, &held, 6, &mut skipped);
    assert_eq!(actual.eq_remaining, 0);
    assert_eq!(actual.seen, held.serial);
    // An edit that arrives during a fade waits for it, and is not lost.
    target.eq.bands[6].enabled = false;
    held = Prepared::new(&target, 48_000).unwrap();
    run(&mut actual, &held, 1, &mut skipped);
    target.eq.bands[7].enabled = false;
    held = Prepared::new(&target, 48_000).unwrap();
    run(&mut actual, &held, 1, &mut skipped);
    assert_eq!(actual.seen, 0);
    run(&mut actual, &held, 12, &mut skipped);
    assert_eq!(actual.seen, held.serial);
    assert!(actual.eq[actual.eq_current].shape.same_as(&held.eq_shape));
    // Going back to an earlier snapshot is a change like any other.
    let earlier = held.clone();
    target.eq.input_gain_db = 3.0;
    held = Prepared::new(&target, 48_000).unwrap();
    run(&mut actual, &held, 2, &mut skipped);
    assert_eq!(actual.seen, held.serial);
    run(&mut actual, &earlier, 2, &mut skipped);
    assert_eq!(actual.seen, earlier.serial);
}

/// What `Dsp::seen` stands for, tested directly: if `dsp` is about to skip
/// its preparation for `p`, the full preparation is run after all. It must
/// leave every bit of the state as it was and record the snapshot as seen
/// again. Returns whether the block was one that skips and, if so, whether a
/// filter it runs had stopped short of its target.
fn prepare_where_skipped(dsp: &mut Dsp, p: &Prepared, frames: usize) -> Option<bool> {
    if frames == 0 || dsp.seen != p.serial {
        return None;
    }
    let before = state(dsp);
    dsp.seen = 0;
    dsp.begin_block(p, frames);
    assert_eq!(
        state(dsp),
        before,
        "a skipped preparation changes the state"
    );
    assert_eq!(dsp.seen, p.serial);
    assert_eq!(dsp.eq_remaining, 0);
    let bank = &dsp.eq[dsp.eq_current];
    assert!(bank.shape.same_as(&p.eq_shape));
    Some(
        (0..p.channels())
            .any(|ch| bank.stalled(ch) != 0 || (p.hp_on && dsp.channels[ch].hp.stalled(p.hp))),
    )
}

/// Coefficient smoothing does not arrive: it stops a few units in the last
/// place short of the target, with `smoothing` false and the coefficients
/// still not the target's. Preparation is skipped from then on, and must be.
#[test]
fn filters_stopped_short_of_their_target_are_skipped_and_match_the_reference() {
    for channels in [1, 2] {
        let mut target = eq_output(channels, 15);
        target.highpass.enabled = true;
        let mut p = Prepared::new(&target, 48_000).unwrap();
        let (mut actual, mut reference) = pair(&p, None);
        let (mut skipped, mut stalled, mut block) = (0, 0, 0);
        for edit in 0..3 {
            // Same shape, new coefficients: every band and the high-pass
            // smooth towards them and stop short.
            match edit {
                0 => {}
                1 => {
                    for band in &mut target.eq.bands {
                        band.gain = -band.gain;
                        band.frequency *= 1.07;
                    }
                    target.highpass.frequency = 173.0;
                }
                _ => target.eq.bands[4].q = 2.9,
            }
            p = Prepared::new(&target, 48_000).unwrap();
            for _ in 0..500 {
                let frames = LENGTHS[block % LENGTHS.len()];
                if let Some(short) = prepare_where_skipped(&mut actual, &p, frames) {
                    skipped += 1;
                    stalled += usize::from(short);
                    // The first snapshot's filters start on their targets.
                    assert_eq!(short, edit != 0, "edit {edit} block {block}");
                }
                let input = tone(block, frames, channels, 0.4);
                check(
                    &mut actual,
                    &mut reference,
                    &p,
                    &input,
                    block % 2 == 0,
                    &|| format!("channels {channels} edit {edit} block {block}"),
                );
                block += 1;
            }
            assert_eq!(actual.seen, p.serial, "smoothing has stopped");
        }
        assert!(
            skipped > 1000 && stalled > 600,
            "skipped {skipped} stalled {stalled}"
        );
        // Stopped short, not arrived: the high-pass and at least one band.
        assert!(actual.channels[0].hp.stalled(p.hp));
        assert_ne!(actual.eq[actual.eq_current].stalled(0), 0);
    }
}

/// The same for random settings and live edits: every block that skips its
/// preparation is checked by running it, and every block is compared with
/// the reference.
#[test]
fn a_preparation_that_would_be_skipped_changes_nothing_when_it_runs() {
    let mut r = Rng(0x0D95_748F_728E_B658);
    let (mut skipped, mut full, mut stalled) = (0_u64, 0_u64, 0_u64);
    // Skipped blocks with the high-pass on, and with the bank off.
    let (mut highpass, mut disabled) = (0_u64, 0_u64);
    for case in 0..fuzz_cases().div_ceil(2) {
        let microphone = r.below(5) == 0;
        let channels = if microphone { 1 } else { 1 + r.below(2) as u8 };
        let mut target = random_target(&mut r, channels, microphone);
        let Ok(mut p) = Prepared::new(&target, 48_000) else {
            continue;
        };
        let (mut actual, mut reference) = pair(&p, microphone.then_some((300, false)));
        // Held long enough for coefficient smoothing to run to its end, or
        // edited so often that edits land inside fades and smoothing.
        let rate = [3, 40, 400][r.below(3) as usize];
        for block in 0..100 + r.below(200) as usize {
            if r.below(rate) == 0 {
                let mut next = target.clone();
                for _ in 0..1 + r.below(3) {
                    edit(&mut r, &mut next);
                }
                if let Ok(prepared) = Prepared::new(&next, 48_000) {
                    (target, p) = (next, prepared);
                }
            }
            let frames = [0, 1, 112, 112, 112, 112, 129, 300][r.below(8) as usize];
            match prepare_where_skipped(&mut actual, &p, frames) {
                Some(short) => {
                    skipped += 1;
                    stalled += u64::from(short);
                    highpass += u64::from(p.hp_on);
                    disabled += u64::from(!actual.eq[actual.eq_current].is_on());
                }
                None => full += u64::from(frames != 0),
            }
            let input = tone(block, frames, channels, 0.4);
            check(
                &mut actual,
                &mut reference,
                &p,
                &input,
                block % 2 == 0,
                &|| format!("case {case} block {block} frames {frames} target {target:?}"),
            );
        }
    }
    eprintln!(
        "skipped={skipped} full={full} stalled={stalled} highpass={highpass} disabled={disabled}"
    );
    assert!(skipped > 500 && full > 500 && highpass > 0 && disabled > 0);
}

/// Snapshots as the mixer does not hand them out but a caller may: one
/// snapshot serving two states that are at different points of their fades,
/// clones, the same settings built again under a new serial, earlier
/// snapshots coming back, empty blocks, and refused calls in between.
#[test]
fn snapshots_shared_cloned_rebuilt_and_revisited_match_the_reference() {
    let mut r = Rng(0x7163_4E2D_0C52_F1A3);
    let (mut skipped, mut full, mut refused) = (0_u64, 0_u64, 0_u64);
    for case in 0..fuzz_cases().div_ceil(3) {
        let channels = 1 + r.below(2) as u8;
        let target = random_target(&mut r, channels, false);
        let Ok(first) = Prepared::new(&target, 48_000) else {
            continue;
        };
        // A snapshot neither state accepts: the other channel count.
        let foreign = Prepared::new(
            &Target::new("other", "Other", TargetKind::Output, 3 - channels),
            48_000,
        )
        .unwrap();
        let mut states = [pair(&first, None), pair(&first, None)];
        let mut pool = vec![(target, first)];
        let mut held = [0_usize; 2];
        let rate = [3, 12, 150][r.below(3) as usize];
        for block in 0..80 + r.below(160) as usize {
            for (index, (actual, reference)) in states.iter_mut().enumerate() {
                match r.below(rate) {
                    0 => {
                        let mut next = pool[held[index]].0.clone();
                        for _ in 0..1 + r.below(2) {
                            edit(&mut r, &mut next);
                        }
                        if let Ok(prepared) = Prepared::new(&next, 48_000) {
                            if pool.len() < 24 {
                                pool.push((next, prepared));
                                held[index] = pool.len() - 1;
                            } else {
                                // The other state may be holding this place:
                                // it then finds a new snapshot there.
                                held[index] = r.below(24) as usize;
                                pool[held[index]] = (next, prepared);
                            }
                        }
                    }
                    // An earlier snapshot, or the one the other state holds.
                    1 => held[index] = r.below(pool.len() as u64) as usize,
                    2 => {
                        // The same settings built again: every field equal
                        // but the serial.
                        let again = Prepared::new(&pool[held[index]].0, 48_000).unwrap();
                        assert_ne!(again.serial, pool[held[index]].1.serial);
                        pool[held[index]].1 = again;
                    }
                    _ => {}
                }
                let (_, p) = &pool[held[index]];
                if r.below(10) == 0 {
                    // A refused call comes before any preparation: it leaves
                    // the state, what it has seen and the buffers alone.
                    let before = (state(actual), actual.seen);
                    let mut buffers = [[0.5_f32; 8]; 2];
                    let [left, right] = &mut buffers;
                    assert_eq!(
                        actual.process_in_place(&foreign, [&mut left[..], &mut right[..]]),
                        Err(DspError::IncompatibleSnapshot)
                    );
                    assert_eq!(
                        actual.process_in_place(p, [&mut left[..], &mut right[..7]]),
                        Err(DspError::BufferShape)
                    );
                    assert_eq!((state(actual), actual.seen), before);
                    assert_eq!(buffers, [[0.5; 8]; 2]);
                    refused += 1;
                }
                let clone = p.clone();
                let p = if r.below(4) == 0 { &clone } else { p };
                let frames = [0, 1, 7, 112, 112, 112, 112, 128, 129, 300][r.below(10) as usize];
                if frames != 0 {
                    if actual.seen == p.serial {
                        skipped += 1;
                    } else {
                        full += 1;
                    }
                }
                let amplitude = [0.3, 0.3, 0.9, 20.0][r.below(4) as usize];
                let mut input = tone(block * 2 + index, frames, channels, amplitude);
                if frames != 0 && r.below(40) == 0 {
                    input[0][frames / 2] = f32::NAN;
                }
                check(actual, reference, p, &input, r.below(2) == 0, &|| {
                    format!("case {case} block {block} state {index} frames {frames}")
                });
            }
        }
        // A retired state refuses everything and is left as it was.
        for (actual, _) in &mut states {
            actual.drain_suppression(std::time::Duration::ZERO).unwrap();
            let before = (state(actual), actual.seen);
            let mut buffers = [[0.5_f32; 8]; 2];
            let [left, right] = &mut buffers;
            let right = if channels == 2 {
                &mut right[..]
            } else {
                &mut right[..0]
            };
            assert_eq!(
                actual.process_in_place(&pool[0].1, [&mut left[..], right]),
                Err(DspError::Retired)
            );
            assert_eq!((state(actual), actual.seen), before);
        }
    }
    eprintln!("skipped={skipped} full={full} refused={refused}");
    assert!(skipped > 300 && full > 300 && refused > 100);
}
