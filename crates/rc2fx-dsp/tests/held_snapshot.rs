//! A `Dsp` that is handed the same `Prepared` block after block skips its
//! per-block preparation. These tests run one state that way and a second
//! state that gets a newly built `Prepared` of the same settings for every
//! block, which is never skipped, and compare output and meters bit for bit.
//! `RC2FX_DSP_HELD_BLOCKS` sets the number of blocks per run.
use rc2fx_core::{Band, BandType, SuppressionGeometry, Target, TargetKind, VoiceGate};
use rc2fx_dsp::{Chat, Dsp, Meter, Prepared, RetirementError, Suppression};

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
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn blocks() -> u64 {
    std::env::var("RC2FX_DSP_HELD_BLOCKS")
        .ok()
        .map(|value| value.parse().expect("RC2FX_DSP_HELD_BLOCKS is a number"))
        .unwrap_or(if cfg!(debug_assertions) {
            4_000
        } else {
            40_000
        })
}

/// One edit that keeps the state: shape, coefficient, high-pass, bypass,
/// gain and dynamics changes.
fn edit(t: &mut Target, r: &mut Rng) {
    let band = |t: &Target, r: &mut Rng| r.below(t.eq.bands.len().max(1) as u64) as usize;
    match r.below(16) {
        0 => t.eq.enabled = !t.eq.enabled,
        1 => t.highpass.enabled = !t.highpass.enabled,
        2 => t.highpass.frequency = 30.0 + 200.0 * r.unit(),
        3 => t.bypass = !t.bypass,
        4 => {
            t.gain.enabled = !t.gain.enabled;
            t.gain.db = -12.0 + 18.0 * r.unit();
        }
        5 => t.compressor.enabled = !t.compressor.enabled,
        6 => t.limiter.enabled = !t.limiter.enabled,
        7 => t.eq.input_gain_db = -6.0 + 12.0 * r.unit(),
        8 => t.eq.output_gain_db = [0.0, 6.0, -6.0][r.below(3) as usize],
        9 if !t.eq.bands.is_empty() => {
            let i = band(t, r);
            t.eq.bands[i].enabled = !t.eq.bands[i].enabled;
        }
        10 if !t.eq.bands.is_empty() => {
            let i = band(t, r);
            t.eq.bands[i].band_type = [
                BandType::Bell,
                BandType::LowShelf,
                BandType::HighShelf,
                BandType::Notch,
            ][r.below(4) as usize];
        }
        11 => {
            if t.eq.bands.len() > 3 && r.below(2) == 0 {
                t.eq.bands.pop();
            } else if t.eq.bands.len() < 32 {
                let id = format!("n{}", r.next() % 100_000);
                if t.eq.bands.iter().all(|b| b.id != id) {
                    t.eq.bands.push(Band::new(id));
                }
            }
        }
        12 if t.suppression.enabled => t.suppression.wet = r.unit(),
        13 if t.suppression.enabled => t.suppression.gate = VoiceGate::with_strength(r.unit()),
        _ if !t.eq.bands.is_empty() => {
            let i = band(t, r);
            t.eq.bands[i].gain = -9.0 + 18.0 * r.unit();
            t.eq.bands[i].frequency = 40.0 + 12_000.0 * r.unit();
            t.eq.bands[i].q = 0.3 + 4.0 * r.unit();
        }
        _ => {}
    }
}

fn bands() -> Vec<Band> {
    (0..15)
        .map(|i| {
            let mut b = Band::new(format!("b{i}"));
            b.frequency = 50.0 * 1.45_f64.powi(i);
            b.gain = if i % 2 == 0 { 3.0 } else { -2.5 };
            b.q = 1.2;
            b
        })
        .collect()
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

/// The schedule both tests share: bursts of edits one to three blocks apart,
/// which land inside fades and coefficient smoothing, and long holds. Calls
/// `block(target, changed, frames, loud)` for each block.
fn schedule(
    r: &mut Rng,
    target: &mut Target,
    mut block: impl FnMut(&Target, bool, usize, bool, &mut Rng),
) {
    let mut until = 0;
    let mut loud = false;
    for at in 0..blocks() {
        let mut changed = false;
        if at == until {
            until = at
                + match r.below(4) {
                    0 => 1,
                    1 => 1 + r.below(3),
                    2 => 5 + r.below(40),
                    _ => 200 + r.below(1500),
                };
            if at != 0 {
                let mut next = target.clone();
                edit(&mut next, r);
                if Prepared::new(&next, 48_000).is_ok() {
                    *target = next;
                    changed = true;
                }
                loud = r.below(8) == 0;
            }
        }
        let frames = match r.below(16) {
            0 => [0, 1, 7, 63, 128, 129, 240, 300][r.below(8) as usize],
            _ => 112,
        };
        block(target, changed, frames, loud, r);
    }
}

fn signal(r: &mut Rng, total: usize, frames: usize, ch: usize, loud: bool) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let x = ((total + i) as f64 * (0.013 + ch as f64 * 0.002)).sin() * 0.3
                + (r.unit() - 0.5) * 0.1;
            if loud && r.below(500) == 0 {
                f32::NAN
            } else if loud {
                (x * 40.0) as f32
            } else {
                x as f32
            }
        })
        .collect()
}

#[test]
fn a_held_snapshot_matches_a_new_snapshot_for_every_block() {
    for (channels, seed) in [(2u8, 1u64), (1, 7), (2, 99), (1, 12345), (2, 424242)] {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut target = Target::new("x", "X", TargetKind::Output, channels);
        target.eq.enabled = true;
        target.eq.bands = bands();
        let mut held = Prepared::new(&target, 48_000).unwrap();
        let mut fresh_state = Dsp::without_suppression(&held).unwrap();
        let mut held_state = Dsp::without_suppression(&held).unwrap();
        let mut total = 0;
        let mut at = 0_u64;
        schedule(&mut r, &mut target, |target, changed, frames, loud, r| {
            if changed {
                held = Prepared::new(target, 48_000).unwrap();
            }
            let fresh = Prepared::new(target, 48_000).unwrap();
            let input: [Vec<f32>; 2] = std::array::from_fn(|ch| {
                if ch < channels as usize {
                    signal(r, total, frames, ch, loud)
                } else {
                    Vec::new()
                }
            });
            total += frames;
            let mut expected = input.clone();
            let mut actual = input.clone();
            let [left, right] = &mut expected;
            let expected_meter = fresh_state.process_in_place(&fresh, [left, right]).unwrap();
            let actual_meter = if at.is_multiple_of(2) {
                let [left, right] = &mut actual;
                held_state.process_in_place(&held, [left, right]).unwrap()
            } else {
                let [left, right] = &mut actual;
                held_state
                    .process(&held, [&input[0], &input[1]], [left, right])
                    .unwrap()
            };
            for ch in 0..channels as usize {
                for i in 0..frames {
                    assert_eq!(
                        actual[ch][i].to_bits(),
                        expected[ch][i].to_bits(),
                        "seed {seed} block {at} ch {ch} sample {i}"
                    );
                }
            }
            assert_eq!(
                meter_bits(&actual_meter),
                meter_bits(&expected_meter),
                "seed {seed} block {at}"
            );
            at += 1;
        });
    }
}

/// A delay-only voice endpoint whose score moves between voiced, unvoiced and
/// missing, so the Chat voice gate opens and closes.
struct Voice {
    geometry: SuppressionGeometry,
    line: Vec<f32>,
    at: usize,
    count: u32,
}
impl Voice {
    fn boxed() -> Box<dyn Suppression> {
        let geometry = rc2fx_core::Suppression::admitted_geometry(0).unwrap();
        Box::new(Self {
            geometry,
            line: vec![0.0; geometry.latency_samples() as usize],
            at: 0,
            count: 0,
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
        dry + (dry * 0.5 - dry) * wet
    }
    fn voice(&self) -> Option<f32> {
        match (self.count / 9000) % 4 {
            0 => Some(0.9),
            1 => Some(0.05),
            2 => None,
            _ => Some(0.4),
        }
    }
    fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
        Ok(())
    }
}

/// Chat keeps two states and two snapshots of its own inside the one it is
/// given: the voice processor and the output effects.
#[test]
fn a_held_chat_snapshot_matches_a_new_snapshot_for_every_block() {
    for seed in [3u64, 58] {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut target = Target::new("send-chat", "Chat", TargetKind::Output, 2);
        target.suppression.enabled = true;
        target.suppression.gate = VoiceGate::with_strength(0.6);
        target.eq.enabled = true;
        target.eq.bands = bands();
        let mut held = Prepared::new(&target, 48_000).unwrap();
        let mut fresh_state = Chat::new(&held, Voice::boxed()).unwrap();
        let mut held_state = Chat::new(&held, Voice::boxed()).unwrap();
        let mut total = 0;
        let mut at = 0_u64;
        schedule(&mut r, &mut target, |target, changed, frames, loud, r| {
            if changed {
                held = Prepared::new(target, 48_000).unwrap();
            }
            let fresh = Prepared::new(target, 48_000).unwrap();
            // The microphone stem must be finite; the mix may be anything.
            let stem: [Vec<f32>; 2] =
                std::array::from_fn(|ch| signal(r, total + 31, frames, ch, false));
            let input: [Vec<f32>; 2] = std::array::from_fn(|ch| signal(r, total, frames, ch, loud));
            total += frames;
            let mut expected = input.clone();
            let mut actual = input.clone();
            let [left, right] = &mut expected;
            let expected_meter = fresh_state
                .process(&fresh, [&stem[0], &stem[1]], [left, right])
                .unwrap();
            let [left, right] = &mut actual;
            let actual_meter = held_state
                .process(&held, [&stem[0], &stem[1]], [left, right])
                .unwrap();
            for ch in 0..2 {
                for i in 0..frames {
                    assert_eq!(
                        actual[ch][i].to_bits(),
                        expected[ch][i].to_bits(),
                        "seed {seed} block {at} ch {ch} sample {i}"
                    );
                }
            }
            assert_eq!(
                meter_bits(&actual_meter),
                meter_bits(&expected_meter),
                "seed {seed} block {at}"
            );
            at += 1;
        });
    }
}
