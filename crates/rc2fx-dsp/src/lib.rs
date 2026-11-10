//! Owned Rust signal processing. Configuration preparation is separate from
//! callback-local state; suppression is supplied by the owned worker layer.

mod chat;
mod equalizer;
mod filter;
pub use chat::Chat;

use equalizer::{EqBank, EqShape};
use filter::{Filter, safe};
use rc2fx_core::{
    Band, BandType, Channel, CoreError, Effect, MAX_BANDS, SAMPLE_RATE, SuppressionGeometry,
    Target, TargetKind,
    eq::{Coefficients, coefficients},
};
use std::sync::atomic::{AtomicU64, Ordering};

pub const EQ_FADE_SAMPLES: usize = 240;
pub const MAX_SUPPRESSION_DELAY: usize = 2368;
/// Samples per channel each stage processes at a time; covers the mixer's
/// 112-sample period in one pass.
pub(crate) const BLOCK: usize = 128;
pub(crate) const GAIN_SMOOTH: f64 = 0.0020811647007;
pub(crate) const EQ_INTERNAL_LIMIT: f64 = 1.3407807929942597e154; // 2^512

/// Callback-facing owned worker endpoint. Its implementation must not allocate,
/// lock, wait, or call the control service in `process_sample` or
/// `process_block`. Inference there is allowed only for an inline endpoint
/// whose model's worst frame fits the calling thread's period budget (the
/// mixer's DSP helper).
/// Dry and wet streams must have the same immutable delay, including on failure.
/// Construction, worker startup, joining and destruction belong outside callbacks.
pub trait Suppression: Send {
    fn geometry(&self) -> SuppressionGeometry;
    fn latency_samples(&self) -> usize {
        self.geometry().latency_samples() as usize
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32;
    /// The model's voice score (0 to 1) for the frame the last
    /// `process_sample` output came from; None when that output was the
    /// delayed original (no result yet, late or failed).
    fn voice(&self) -> Option<f32>;
    /// `process_sample` for a block, in place: `samples[i]` with `wet[i]`
    /// becomes the output sample and `voice[i]` is what `voice()` returns
    /// after it. The three slices have the same length, which `Dsp` keeps
    /// between 1 and 128 and which need not match the model's frame.
    ///
    /// `Dsp` processes blocks through this method only. An implementation
    /// that overrides it must leave the same output, voice scores and state,
    /// bit for bit, as these per-sample calls.
    fn process_block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        assert!(wet.len() == samples.len() && voice.len() == samples.len());
        for ((sample, wet), voice) in samples.iter_mut().zip(wet).zip(voice) {
            *sample = self.process_sample(*sample, *wet);
            *voice = self.voice();
        }
    }
    /// Control only, after every callback loan has returned. Implementations
    /// retain their exact terminal receipt outside this vendor-neutral trait.
    fn drain(&mut self, timeout: std::time::Duration) -> Result<(), RetirementError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetirementError {
    Unsupported,
    TimedOut,
    WorkerFailed,
    Incomplete,
}
impl std::fmt::Display for RetirementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RetirementError {}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Meter {
    pub input_peak: f64,
    pub output_peak: f64,
    pub sum_squares: f64,
    pub gain_reduction: f64,
    /// Highest suppression voice score, 0 to 1 (0 without suppression).
    pub voice: f64,
    /// Most open voice gate position, 0 (closed) to 1 (open; also while the
    /// gate is off). 0 without suppression.
    pub gate_open: f64,
    pub samples: u64,
    pub clipped: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DspError {
    MissingSuppression,
    UnexpectedSuppression,
    SuppressionGeometry,
    IncompatibleSnapshot,
    BufferShape,
    MissingChatStem,
    Retired,
}
impl std::fmt::Display for DspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DspError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Signature {
    channels: usize,
    kind: TargetKind,
    geometry: Option<SuppressionGeometry>,
    // A provider owns queued samples and model history at this position in the
    // chain. Without one, order belongs entirely to the prepared parameters.
    suppression_order: Option<[Effect; 6]>,
}

/// Immutable control-thread preparation. Contains no borrowed editor data.
/// A callback does not clone this value or retain references to it afterwards.
///
/// Nothing may change a `Prepared` once `new` has returned it: a `Dsp` that
/// has prepared a block from one skips that work while it keeps being handed
/// the same one, and tells them apart by `serial` alone.
#[derive(Clone)]
pub struct Prepared {
    /// Different for every `new`, never 0. A clone keeps it, and is equal in
    /// every other field too.
    serial: u64,
    signature: Signature,
    chat: Option<Box<chat::PreparedChat>>,
    active_order: [Effect; 6],
    active_count: usize,
    bypass: bool,
    hp_on: bool,
    noise_on: bool,
    comp_on: bool,
    gain_on: bool,
    limit_on: bool,
    hp: Coefficients,
    eq_shape: EqShape,
    eq: [[Coefficients; MAX_BANDS]; 2],
    eq_active: [[bool; MAX_BANDS]; 2],
    eq_input_gain: f64,
    eq_output_gain: f64,
    wet: f64,
    gate_on: bool,
    gate_threshold: f32,
    gate_close: f32,
    gate_reduction_db: f64,
    gate_hold: u32,
    gate_attack: f64,
    gate_release: f64,
    gain: f64,
    makeup: f64,
    ceiling: f64,
    threshold_db: f64,
    ratio: f64,
    comp_threshold: f64,
    comp_attack: f64,
    comp_release: f64,
    limiter_release: f64,
}
fn db_gain(db: f64) -> f64 {
    10_f64.powf(db / 20.0)
}
impl Prepared {
    /// Validation, coefficient calculation and ID copying are control work.
    pub fn new(target: &Target, sample_rate: u32) -> Result<Self, CoreError> {
        if sample_rate != 48_000 {
            return Err(CoreError("DSP requires 48000 Hz".into()));
        }
        target.validate()?;
        let geometry = target.suppression.geometry();
        let mut prepared = Self::validated(target, geometry)?;
        if target.is_chat_output() && target.suppression.enabled {
            prepared.chat = Some(Box::new(chat::PreparedChat::new(target)?));
        }
        Ok(prepared)
    }
    fn validated(
        target: &Target,
        geometry: Option<SuppressionGeometry>,
    ) -> Result<Self, CoreError> {
        let geometry = if target.suppression.enabled {
            let geometry =
                geometry.ok_or_else(|| CoreError("unresolved suppression geometry".into()))?;
            if geometry.sample_rate != 48_000
                || geometry.latency_samples() as usize > MAX_SUPPRESSION_DELAY
            {
                return Err(CoreError("suppression geometry exceeds DSP storage".into()));
            }
            Some(geometry)
        } else {
            None
        };
        let mut hp = Band::new("highpass");
        hp.band_type = BandType::HighPass;
        hp.frequency = target.highpass.frequency;
        hp.q = std::f64::consts::FRAC_1_SQRT_2;
        let gate = target.suppression.gate.shape();
        // Control thread only. An address would not do as the identity: a
        // freed snapshot's address can be handed to the next one.
        static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);
        let mut p = Self {
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            chat: None,
            signature: Signature {
                channels: target.channels as usize,
                kind: target.kind,
                geometry,
                suppression_order: geometry.map(|_| {
                    if target.is_chat_output() {
                        // Chat's voice processor is permanently before the stereo
                        // sum. Moving output effects does not move model history.
                        [
                            Effect::Suppression,
                            Effect::Highpass,
                            Effect::Eq,
                            Effect::Compressor,
                            Effect::Gain,
                            Effect::Limiter,
                        ]
                    } else {
                        target
                            .order
                            .as_slice()
                            .try_into()
                            .expect("validated six effects")
                    }
                }),
            },
            active_order: [Effect::Eq; 6],
            active_count: 0,
            bypass: target.bypass,
            hp_on: target.highpass.enabled,
            noise_on: target.suppression.enabled,
            comp_on: target.compressor.enabled,
            gain_on: target.gain.enabled,
            limit_on: target.limiter.enabled,
            hp: coefficients(&hp)?,
            eq_shape: EqShape::new(target),
            eq: [[Coefficients::IDENTITY; MAX_BANDS]; 2],
            eq_active: [[false; MAX_BANDS]; 2],
            eq_input_gain: db_gain(target.eq.input_gain_db),
            eq_output_gain: db_gain(target.eq.output_gain_db),
            wet: target.suppression.wet,
            gate_on: target.suppression.enabled && gate.reduction_db > 0.0,
            gate_threshold: gate.threshold as f32,
            gate_close: gate.close_threshold as f32,
            gate_reduction_db: gate.reduction_db,
            gate_hold: (gate.hold_ms * 0.001 * SAMPLE_RATE).round() as u32,
            gate_attack: (-1.0 / (gate.attack_ms * 0.001 * SAMPLE_RATE)).exp(),
            gate_release: (-1.0 / (gate.release_ms * 0.001 * SAMPLE_RATE)).exp(),
            gain: db_gain(target.gain.db),
            makeup: db_gain(target.compressor.makeup),
            ceiling: db_gain(target.limiter.ceiling),
            threshold_db: target.compressor.threshold,
            ratio: target.compressor.ratio,
            comp_threshold: db_gain(target.compressor.threshold),
            comp_attack: (-1.0 / (target.compressor.attack * 0.001 * SAMPLE_RATE)).exp(),
            comp_release: (-1.0 / (target.compressor.release * 0.001 * SAMPLE_RATE)).exp(),
            limiter_release: (-1.0 / (target.limiter.release * 0.001 * SAMPLE_RATE)).exp(),
        };
        // Only stages whose disabled path is an identity can be omitted;
        // EQ/gain smoothing and an allocated microphone provider continue while
        // the chain is bypassed. Order changes without a provider retain the
        // existing filter/envelope histories and take effect on the next block.
        // Enabling/disabling suppression changes geometry and requires restart.
        for stage in target.order.iter().copied() {
            let include = match stage {
                Effect::Highpass => p.hp_on,
                Effect::Compressor => p.comp_on,
                Effect::Limiter => p.limit_on,
                Effect::Suppression => geometry.is_some(),
                Effect::Eq | Effect::Gain => true,
            };
            if include {
                p.active_order[p.active_count] = stage;
                p.active_count += 1;
            }
        }
        for ch in 0..p.channels() {
            let bands = target.eq.channel(if ch == 0 {
                Channel::Left
            } else {
                Channel::Right
            });
            let solo = bands.iter().any(|b| b.solo);
            for (i, band) in bands.iter().enumerate() {
                p.eq[ch][i] = coefficients(band)?;
                p.eq_active[ch][i] =
                    band.enabled && band.band_type != BandType::Off && (!solo || band.solo);
            }
        }
        Ok(p)
    }
    pub fn channels(&self) -> usize {
        self.signature.channels
    }
    pub fn kind(&self) -> TargetKind {
        self.signature.kind
    }
    pub fn is_chat_voice(&self) -> bool {
        self.chat.is_some()
    }
    pub fn suppression_geometry(&self) -> Option<SuppressionGeometry> {
        self.signature.geometry
    }
    pub fn requires_reset(&self, next: &Self) -> bool {
        self.signature != next.signature
    }
}

struct ChannelState {
    suppression: Option<Box<dyn Suppression>>,
    bypass_dry: Box<[f32]>,
    bypass_position: usize,
    hp: Filter,
}

/// One owner serializes processing and snapshot handoff. No shared reference
/// counts, mutexes or service callbacks are part of this processor.
pub struct Dsp {
    signature: Signature,
    retired: bool,
    channels: [ChannelState; 2],
    latency: usize,
    gain: f64,
    wet: f64,
    /// Voice gate level in dB (0 = open) and its linear gain, the samples
    /// left of its hold, and whether the voice score last counted as voice
    /// (for hysteresis).
    gate_db: f64,
    gate_gain: f64,
    gate_hold: u32,
    gate_voiced: bool,
    /// The gate position the meter shows, and the level and floor (as bits)
    /// it was worked out from.
    gate_open: f64,
    gate_open_for: [u64; 2],
    envelope: f64,
    limiter_gain: f64,
    /// The limiter's reduction in dB for the meter, and the gain it was
    /// worked out from.
    limiter_reduction: f64,
    limiter_reduction_for: f64,
    processing_blend: f64,
    eq: [EqBank; 2],
    eq_current: usize,
    eq_remaining: usize,
    /// The serial of the `Prepared` whose `begin_block` would change nothing
    /// if it ran again, or 0. Only `begin_block` and `block` change the EQ
    /// banks, the fade and the filters; anything else that does must set
    /// this back to 0.
    seen: u64,
}
impl Dsp {
    /// This constructor is deliberately outside the audio callback. Only enabled
    /// suppression needs a provider; bypass retains that provider and its delay.
    pub fn new(
        p: &Prepared,
        mut suppression: [Option<Box<dyn Suppression>>; 2],
    ) -> Result<Self, DspError> {
        if p.is_chat_voice() {
            // A completed Chat mix contains pads. Only Chat::process, with a
            // separately captured microphone stem, may use its suppressor.
            return Err(DspError::MissingChatStem);
        }
        let mut latency = 0;
        for (ch, provider) in suppression.iter().enumerate() {
            if ch < p.channels() && p.signature.geometry.is_some() {
                let provider = provider.as_ref().ok_or(DspError::MissingSuppression)?;
                let geometry = provider.geometry();
                if Some(geometry) != p.signature.geometry {
                    return Err(DspError::SuppressionGeometry);
                }
                let delay = provider.latency_samples();
                if delay != geometry.latency_samples() as usize
                    || delay == 0
                    || delay > MAX_SUPPRESSION_DELAY
                    || (ch > 0 && delay != latency)
                {
                    return Err(DspError::SuppressionGeometry);
                }
                latency = delay;
            } else if provider.is_some() {
                return Err(DspError::UnexpectedSuppression);
            }
        }
        Ok(Self {
            signature: p.signature,
            retired: false,
            channels: std::array::from_fn(|ch| ChannelState {
                suppression: suppression[ch].take(),
                bypass_dry: vec![0.0; if ch < p.channels() { latency } else { 0 }]
                    .into_boxed_slice(),
                bypass_position: 0,
                hp: Filter::with_coefficients(p.hp),
            }),
            latency,
            gain: if p.gain_on { p.gain } else { 1.0 },
            wet: if p.noise_on { p.wet } else { 0.0 },
            gate_db: 0.0,
            gate_gain: 1.0,
            gate_hold: 0,
            gate_voiced: true,
            // An open gate with no floor reads fully open.
            gate_open: 1.0,
            gate_open_for: [0.0_f64.to_bits(); 2],
            envelope: 0.0,
            limiter_gain: 1.0,
            // A gain of 1 has no reduction and is never looked up.
            limiter_reduction: 0.0,
            limiter_reduction_for: 1.0,
            processing_blend: if p.bypass { 0.0 } else { 1.0 },
            eq: [EqBank::new(p, None), EqBank::default()],
            eq_current: 0,
            eq_remaining: 0,
            seen: 0,
        })
    }
    pub fn without_suppression(p: &Prepared) -> Result<Self, DspError> {
        Self::new(p, [None, None])
    }
    pub fn latency_samples(&self) -> usize {
        self.latency
    }
    /// Irreversible control-side retirement after callback quiescence. The
    /// deadline is shared by all channel providers; retry can observe a late
    /// join, but processing remains disabled even if the first call times out.
    pub fn drain_suppression(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<(), RetirementError> {
        self.retired = true;
        let end = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or(RetirementError::TimedOut)?;
        let mut failure = None;
        for channel in &mut self.channels {
            if let Some(provider) = &mut channel.suppression
                && let Err(error) =
                    provider.drain(end.saturating_duration_since(std::time::Instant::now()))
            {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn check(&self, p: &Prepared, lengths: [usize; 2]) -> Result<usize, DspError> {
        if self.retired {
            return Err(DspError::Retired);
        }
        if self.signature != p.signature {
            return Err(DspError::IncompatibleSnapshot);
        }
        if (p.channels() == 2 && lengths[0] != lengths[1]) || (p.channels() == 1 && lengths[1] != 0)
        {
            return Err(DspError::BufferShape);
        }
        Ok(lengths[0])
    }
    /// Takes up `p`'s EQ shape and targets and decides which filters smooth
    /// their coefficients over this block.
    ///
    /// The same `Prepared` usually arrives for thousands of blocks. Once a
    /// full pass finds the shape unchanged, no fade running and no filter
    /// smoothing, another pass with the same `Prepared` would write back
    /// exactly what is there: the targets are the same, no coefficient moves
    /// while its filter is not smoothing, and `Filter::begin` then finds the
    /// same answer. `seen` records that, and the pass is skipped until a
    /// different `Prepared` arrives.
    fn begin_block(&mut self, p: &Prepared, frames: usize) {
        if frames == 0 || self.seen == p.serial {
            return;
        }
        let current = self.eq_current;
        let same_shape = self.eq[current].shape.same_as(&p.eq_shape);
        if !same_shape {
            if self.eq_remaining == 0 {
                self.eq[1 - current] = EqBank::new(p, Some(&self.eq[current]));
                self.eq_current = 1 - current;
                self.eq_remaining = EQ_FADE_SAMPLES;
            }
        } else {
            self.eq[current].update_targets(p);
        }
        let mut steady = same_shape && self.eq_remaining == 0;
        for ch in 0..p.channels() {
            if p.hp_on {
                self.channels[ch].hp.begin(p.hp);
                steady &= !self.channels[ch].hp.is_smoothing();
            }
            for bank in 0..2 {
                if bank == self.eq_current || self.eq_remaining != 0 {
                    self.eq[bank].begin(ch);
                }
            }
            steady &= !self.eq[self.eq_current].is_smoothing(ch);
        }
        self.seen = if steady { p.serial } else { 0 };
    }
    /// Planar output, no allocation or frame-count limit. All lengths and reset
    /// compatibility are checked before any state or output is changed.
    pub fn process(
        &mut self,
        p: &Prepared,
        input: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) -> Result<Meter, DspError> {
        let frames = self.check(p, [input[0].len(), input[1].len()])?;
        self.check(p, [output[0].len(), output[1].len()])?;
        if frames != output[0].len() {
            return Err(DspError::BufferShape);
        }
        self.begin_block(p, frames);
        let mut meter = Meter::default();
        if self.steady_identity(p) {
            for n in 0..frames {
                let samples = [
                    input[0][n],
                    if p.channels() == 2 { input[1][n] } else { 0.0 },
                ];
                let y = Self::identity_frame(samples, p.channels(), &mut meter);
                for ch in 0..p.channels() {
                    output[ch][n] = y[ch];
                }
            }
            return Ok(meter);
        }
        let mut x = [[0.0; BLOCK]; 2];
        for start in (0..frames).step_by(BLOCK) {
            let n = BLOCK.min(frames - start);
            for ch in 0..p.channels() {
                for (sample, value) in x[ch].iter_mut().zip(&input[ch][start..start + n]) {
                    *sample = f64::from(*value);
                }
            }
            self.block(p, &mut x, n, &mut meter);
            for ch in 0..p.channels() {
                for (value, sample) in output[ch][start..start + n].iter_mut().zip(&x[ch]) {
                    *value = *sample as f32;
                }
            }
        }
        Ok(meter)
    }
    pub fn process_in_place(
        &mut self,
        p: &Prepared,
        planes: [&mut [f32]; 2],
    ) -> Result<Meter, DspError> {
        let frames = self.check(p, [planes[0].len(), planes[1].len()])?;
        self.begin_block(p, frames);
        let mut meter = Meter::default();
        let [left, right] = planes;
        if self.steady_identity(p) {
            if p.channels() == 1 {
                for sample in left {
                    *sample = Self::identity_frame([*sample, 0.0], 1, &mut meter)[0];
                }
            } else {
                for (left, right) in left.iter_mut().zip(right) {
                    let y = Self::identity_frame([*left, *right], 2, &mut meter);
                    *left = y[0];
                    *right = y[1];
                }
            }
            return Ok(meter);
        }
        let planes = [left, right];
        let mut x = [[0.0; BLOCK]; 2];
        for start in (0..frames).step_by(BLOCK) {
            let n = BLOCK.min(frames - start);
            for ch in 0..p.channels() {
                for (sample, value) in x[ch].iter_mut().zip(&planes[ch][start..start + n]) {
                    *sample = f64::from(*value);
                }
            }
            self.block(p, &mut x, n, &mut meter);
            for ch in 0..p.channels() {
                for (value, sample) in planes[ch][start..start + n].iter_mut().zip(&x[ch]) {
                    *value = *sample as f32;
                }
            }
        }
        Ok(meter)
    }
    fn steady_identity(&self, p: &Prepared) -> bool {
        self.latency == 0
            && self.signature.geometry.is_none()
            && !p.hp_on
            && !p.noise_on
            && !p.comp_on
            && !p.limit_on
            && (if p.gain_on { p.gain } else { 1.0 }) == 1.0
            && self.gain == 1.0
            && self.eq_remaining == 0
            && self.eq[self.eq_current].steady_disabled()
            && (self.wet + GAIN_SMOOTH * (0.0 - self.wet)).to_bits() == self.wet.to_bits()
            && self.processing_blend.is_finite()
            && (self.processing_blend
                + GAIN_SMOOTH * ((if p.bypass { 0.0 } else { 1.0 }) - self.processing_blend))
                .to_bits()
                == self.processing_blend.to_bits()
    }
    /// One sample of the voice gate after suppression: returns the gain for
    /// this sample. The level moves in dB, one-pole toward 0 dB (attack) or
    /// the reduction (release), so closing does not drop fast and then crawl.
    /// An open gate stays open down to the close threshold. A missing score
    /// (the delayed original is playing) opens the gate so the fallback is
    /// never cut. The gain is exactly 1 while the gate is off and open.
    #[inline]
    fn gate_step(&mut self, p: &Prepared, voice: Option<f32>, m: &mut Meter) -> f64 {
        if let Some(v) = voice {
            m.voice = m.voice.max(f64::from(v));
        }
        let threshold = if self.gate_voiced {
            p.gate_close
        } else {
            p.gate_threshold
        };
        self.gate_voiced = !p.gate_on || voice.is_none_or(|v| v >= threshold);
        if self.gate_voiced {
            self.gate_hold = p.gate_hold;
        } else {
            self.gate_hold = self.gate_hold.saturating_sub(1);
        }
        let floor = if p.gate_on { -p.gate_reduction_db } else { 0.0 };
        let target = if self.gate_voiced || self.gate_hold > 0 {
            0.0
        } else {
            floor
        };
        if self.gate_db != target {
            let coeff = if target > self.gate_db {
                p.gate_attack
            } else {
                p.gate_release
            };
            self.gate_db = target + (self.gate_db - target) * coeff;
            if (self.gate_db - target).abs() < 1e-4 {
                self.gate_db = target;
            }
            // Only a moving level needs a new gain.
            self.gate_gain = if self.gate_db == 0.0 {
                1.0
            } else {
                db_gain(self.gate_db)
            };
        }
        // The position is a function of the level and the floor alone, so the
        // division is done only when one of them has changed.
        let key = [self.gate_db.to_bits(), floor.to_bits()];
        if key != self.gate_open_for {
            self.gate_open_for = key;
            self.gate_open = if floor < 0.0 {
                (1.0 - self.gate_db / floor).clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
        m.gate_open = m.gate_open.max(self.gate_open);
        self.gate_gain
    }
    /// `gate_step` for the first of `scores` and for every score after it
    /// that leaves the gate where the first put it: returns the gain they
    /// share and how many samples that is, at least one. `scores` is not
    /// empty.
    #[inline]
    fn gate_run(&mut self, p: &Prepared, scores: &[Option<f32>], m: &mut Meter) -> (f64, usize) {
        let gain = self.gate_step(p, scores[0], m);
        (gain, 1 + self.gate_rest(p, &scores[1..], m))
    }
    /// How many of `scores`, from the first, `gate_step` would take without
    /// changing the gate or its gain; their scores go to the meter here.
    /// Call it straight after a `gate_step` with the same `p`.
    ///
    /// That step left the hold and the kept position as `p` makes them. A
    /// further step changes nothing when the gate is open and at rest (voiced,
    /// the hold full, the level at 0 dB) and the next score counts as voice
    /// again, or shut and at rest (not voiced, the hold run out, the level on
    /// the floor) and the next score does not: the same verdict keeps the
    /// hold, the target is the level the gate is at, and the position
    /// `gate_step` gave the meter is the one it would give again. The first
    /// score with the other verdict ends the count and is left to
    /// `gate_step`.
    #[inline]
    fn gate_rest(&self, p: &Prepared, scores: &[Option<f32>], m: &mut Meter) -> usize {
        let at_rest = if self.gate_voiced {
            self.gate_hold == p.gate_hold && self.gate_db == 0.0
        } else {
            // Not voiced, so the gate is on and its floor is the reduction.
            p.gate_on && self.gate_hold == 0 && self.gate_db == -p.gate_reduction_db
        };
        if !at_rest {
            return 0;
        }
        let threshold = if self.gate_voiced {
            p.gate_close
        } else {
            p.gate_threshold
        };
        let mut count = 0;
        for score in scores {
            let voiced = !p.gate_on || score.is_none_or(|v| v >= threshold);
            if voiced != self.gate_voiced {
                break;
            }
            if let Some(v) = *score {
                m.voice = m.voice.max(f64::from(v));
            }
            count += 1;
        }
        count
    }
    /// `gate_step` as it was before the gate position was kept between
    /// samples: the reference for it in the tests, and what `frame` calls.
    #[cfg(test)]
    fn gate_step_reference(&mut self, p: &Prepared, voice: Option<f32>, m: &mut Meter) -> f64 {
        if let Some(v) = voice {
            m.voice = m.voice.max(f64::from(v));
        }
        let threshold = if self.gate_voiced {
            p.gate_close
        } else {
            p.gate_threshold
        };
        self.gate_voiced = !p.gate_on || voice.is_none_or(|v| v >= threshold);
        if self.gate_voiced {
            self.gate_hold = p.gate_hold;
        } else {
            self.gate_hold = self.gate_hold.saturating_sub(1);
        }
        let floor = if p.gate_on { -p.gate_reduction_db } else { 0.0 };
        let target = if self.gate_voiced || self.gate_hold > 0 {
            0.0
        } else {
            floor
        };
        if self.gate_db != target {
            let coeff = if target > self.gate_db {
                p.gate_attack
            } else {
                p.gate_release
            };
            self.gate_db = target + (self.gate_db - target) * coeff;
            if (self.gate_db - target).abs() < 1e-4 {
                self.gate_db = target;
            }
            self.gate_gain = if self.gate_db == 0.0 {
                1.0
            } else {
                db_gain(self.gate_db)
            };
        }
        m.gate_open = m.gate_open.max(if floor < 0.0 {
            (1.0 - self.gate_db / floor).clamp(0.0, 1.0)
        } else {
            1.0
        });
        self.gate_gain
    }
    #[inline]
    fn identity_frame(input: [f32; 2], channels: usize, meter: &mut Meter) -> [f32; 2] {
        let mut output = [0.0; 2];
        for ch in 0..channels {
            let raw = input[ch] as f64;
            if raw.is_finite() {
                meter.input_peak = meter.input_peak.max(raw.abs());
            }
            let x = safe(raw, 16.0, meter);
            // The ordinary bypass mix adds positive zero even for -0 input.
            // Preserve its output bits, both safety stages and meter order.
            let y = safe(x + 0.0, 1.0, meter);
            output[ch] = y as f32;
            meter.output_peak = meter.output_peak.max(y.abs());
            meter.sum_squares += y * y;
            meter.samples += 1;
        }
        output
    }
    /// `frame` for `n` samples per channel, one stage at a time. Every sample
    /// sees the same operations in the same order as `frame`, so the output,
    /// meter and state match it bit for bit. Mono leaves `x[1]` at zero.
    ///
    /// Each pass that `frame` guards with `safe` runs here without it and is
    /// followed by one check of the block's largest magnitude. A block that
    /// passes is exactly what `safe` would have returned, with nothing
    /// counted; any other block takes the per-sample `safe` loop. Between
    /// stages every sample is therefore finite and within 16, as in `frame`.
    fn block(&mut self, p: &Prepared, x: &mut [[f64; BLOCK]; 2], n: usize, m: &mut Meter) {
        let channels = p.channels();
        let mut bypass = [[0.0; BLOCK]; 2];
        for ch in 0..channels {
            let samples = &mut x[ch][..n];
            let peak = max_abs_bits(samples);
            if peak <= 16.0_f64.to_bits() {
                // All finite and within the limit: the peak is their largest
                // magnitude and `safe` would change none of them.
                m.input_peak = m.input_peak.max(f64::from_bits(peak));
            } else {
                for sample in samples.iter_mut() {
                    let raw = *sample;
                    if raw.is_finite() {
                        m.input_peak = m.input_peak.max(raw.abs());
                    }
                    *sample = safe(raw, 16.0, m);
                }
            }
            if self.latency == 0 {
                bypass[ch][..n].copy_from_slice(samples);
                continue;
            }
            // The delayed original: read each ring slot, then replace it,
            // one straight run of the ring at a time.
            let c = &mut self.channels[ch];
            let mut t = 0;
            while t < n {
                let run = (n - t).min(self.latency - c.bypass_position);
                let ring = &mut c.bypass_dry[c.bypass_position..c.bypass_position + run];
                for ((dry, held), sample) in bypass[ch][t..t + run]
                    .iter_mut()
                    .zip(ring)
                    .zip(&samples[t..t + run])
                {
                    *dry = *held as f64;
                    *held = *sample as f32;
                }
                t += run;
                c.bypass_position += run;
                if c.bypass_position == self.latency {
                    c.bypass_position = 0;
                }
            }
        }
        let gain = Ramp::advance(&mut self.gain, if p.gain_on { p.gain } else { 1.0 }, n);
        let wet = Ramp::advance(&mut self.wet, if p.noise_on { p.wet } else { 0.0 }, n);
        let blend = Ramp::advance(
            &mut self.processing_blend,
            if p.bypass { 0.0 } else { 1.0 },
            n,
        );
        let mut reduction = [0.0; BLOCK];
        for stage in p.active_order[..p.active_count].iter().copied() {
            match stage {
                Effect::Highpass => {
                    if p.hp_on {
                        for (state, samples) in
                            self.channels.iter_mut().zip(x.iter_mut()).take(channels)
                        {
                            state.hp.process_block(&p.hp, &mut samples[..n], 16.0, m);
                        }
                    }
                }
                Effect::Suppression => {
                    let mut voice = [None; BLOCK];
                    let mut wet32 = [0.0; BLOCK];
                    wet.each(n, |t, wet| wet32[t] = wet as f32);
                    let mut first = true;
                    for (state, samples) in
                        self.channels.iter_mut().zip(x.iter_mut()).take(channels)
                    {
                        if let Some(provider) = state.suppression.as_mut() {
                            suppress(
                                provider.as_mut(),
                                &mut samples[..n],
                                &wet32[..n],
                                first,
                                &mut voice[..n],
                                m,
                            );
                            first = false;
                        }
                    }
                    let mut t = 0;
                    while t < n {
                        let (gain, run) = self.gate_run(p, &voice[t..n], m);
                        for samples in x.iter_mut().take(channels) {
                            for sample in &mut samples[t..t + run] {
                                *sample *= gain;
                            }
                        }
                        t += run;
                    }
                }
                Effect::Eq => {
                    let fading = self.eq_remaining.min(n);
                    if fading == 0 {
                        let bank = &mut self.eq[self.eq_current];
                        bank.process_block(x, n, channels, m);
                        if bank.is_on() {
                            // An enabled bank ends by limiting its own
                            // output to 16.
                            continue;
                        }
                    } else {
                        // The previous bank fades out over the first samples.
                        let mut prior = *x;
                        self.eq[1 - self.eq_current].process_block(&mut prior, fading, channels, m);
                        self.eq[self.eq_current].process_block(x, n, channels, m);
                        for t in 0..fading {
                            let mix = (EQ_FADE_SAMPLES - self.eq_remaining + 1) as f64
                                / EQ_FADE_SAMPLES as f64;
                            for ch in 0..channels {
                                x[ch][t] = prior[ch][t] + mix * (x[ch][t] - prior[ch][t]);
                            }
                            self.eq_remaining -= 1;
                        }
                    }
                }
                Effect::Compressor => {
                    if p.comp_on {
                        for t in 0..n {
                            let peak = x[0][t].abs().max(x[1][t].abs());
                            let coeff = if peak > self.envelope {
                                p.comp_attack
                            } else {
                                p.comp_release
                            };
                            self.envelope = coeff * self.envelope + (1.0 - coeff) * peak;
                            let over = if self.envelope > p.comp_threshold {
                                (20.0 * self.envelope.max(1e-20).log10() - p.threshold_db).max(0.0)
                            } else {
                                0.0
                            };
                            let gr = over * (1.0 - 1.0 / p.ratio);
                            let scale = (if gr > 0.0 { db_gain(-gr) } else { 1.0 }) * p.makeup;
                            reduction[t] += gr;
                            for samples in x.iter_mut().take(channels) {
                                samples[t] *= scale;
                            }
                        }
                    }
                }
                Effect::Gain => {
                    if gain.is_one() {
                        // Finite samples times exactly 1 are unchanged, and
                        // the stage before left them within the limit.
                        continue;
                    }
                    for samples in x.iter_mut().take(channels) {
                        let samples = &mut samples[..n];
                        gain.each(n, |t, gain| samples[t] *= gain);
                    }
                }
                Effect::Limiter => {
                    if p.limit_on {
                        for t in 0..n {
                            let peak = x[0][t].abs().max(x[1][t].abs());
                            let needed = if peak > p.ceiling {
                                p.ceiling / peak
                            } else {
                                1.0
                            };
                            self.limiter_gain =
                                needed.min(1.0 - (1.0 - self.limiter_gain) * p.limiter_release);
                            if self.limiter_gain < 1.0 {
                                // After its first reduction the gain never
                                // gets back to exactly 1: it stops just
                                // below and holds that value. The logarithm,
                                // which only the meter reads, is taken when
                                // the gain changes, not for every sample.
                                if self.limiter_gain.to_bits()
                                    != self.limiter_reduction_for.to_bits()
                                {
                                    self.limiter_reduction_for = self.limiter_gain;
                                    self.limiter_reduction =
                                        -20.0 * self.limiter_gain.max(1e-20).log10();
                                }
                                reduction[t] += self.limiter_reduction;
                            }
                            for samples in x.iter_mut().take(channels) {
                                samples[t] *= self.limiter_gain;
                            }
                        }
                    }
                }
            }
            for samples in x.iter_mut().take(channels) {
                clamp(&mut samples[..n], 16.0, m);
            }
        }
        if p.comp_on || p.limit_on {
            for value in &reduction[..n] {
                m.gain_reduction = m.gain_reduction.max(*value);
            }
        }
        let mut peak = 0;
        for ch in 0..channels {
            let (mixed, dry) = (&mut x[ch][..n], &bypass[ch][..n]);
            blend.each(n, |t, blend| {
                mixed[t] = dry[t] + (mixed[t] - dry[t]) * blend
            });
            peak = peak.max(max_abs_bits(mixed));
        }
        if peak <= 1.0_f64.to_bits() {
            // No sample needs `safe`. The peak is order-independent; the sum
            // of squares keeps `frame`'s order, time outside and channel
            // inside, because floating-point addition is not associative.
            m.output_peak = m.output_peak.max(f64::from_bits(peak));
            let mut sum = m.sum_squares;
            for t in 0..n {
                for samples in x.iter().take(channels) {
                    sum += samples[t] * samples[t];
                }
            }
            m.sum_squares = sum;
            m.samples += (n * channels) as u64;
            return;
        }
        for t in 0..n {
            for samples in x.iter_mut().take(channels) {
                let y = safe(samples[t], 1.0, m);
                samples[t] = y;
                m.output_peak = m.output_peak.max(y.abs());
                m.sum_squares += y * y;
                m.samples += 1;
            }
        }
    }
    #[cfg(test)]
    fn frame(&mut self, p: &Prepared, input: [f32; 2], m: &mut Meter) -> [f32; 2] {
        let mut x = [0.0_f64; 2];
        let mut bypass = [0.0_f64; 2];
        let mut reduction = 0.0;
        for ch in 0..p.channels() {
            let raw = input[ch] as f64;
            if raw.is_finite() {
                m.input_peak = m.input_peak.max(raw.abs());
            }
            x[ch] = safe(raw, 16.0, m);
            let c = &mut self.channels[ch];
            bypass[ch] = x[ch];
            if self.latency != 0 {
                bypass[ch] = c.bypass_dry[c.bypass_position] as f64;
                c.bypass_dry[c.bypass_position] = x[ch] as f32;
                c.bypass_position += 1;
                if c.bypass_position == self.latency {
                    c.bypass_position = 0;
                }
            }
        }
        self.gain += GAIN_SMOOTH * ((if p.gain_on { p.gain } else { 1.0 }) - self.gain);
        self.wet += GAIN_SMOOTH * ((if p.noise_on { p.wet } else { 0.0 }) - self.wet);
        self.processing_blend +=
            GAIN_SMOOTH * ((if p.bypass { 0.0 } else { 1.0 }) - self.processing_blend);
        for stage in p.active_order[..p.active_count].iter().copied() {
            match stage {
                Effect::Highpass => {
                    if p.hp_on {
                        for (ch, sample) in x.iter_mut().enumerate().take(p.channels()) {
                            *sample = self.channels[ch].hp.process(&p.hp, *sample, 16.0, m);
                        }
                    }
                }
                Effect::Suppression => {
                    let mut voice = None;
                    let mut first = true;
                    for (ch, sample) in x.iter_mut().enumerate().take(p.channels()) {
                        if let Some(provider) = self.channels[ch].suppression.as_mut() {
                            *sample = provider
                                .process_sample(safe(*sample, 16.0, m) as f32, self.wet as f32)
                                as f64;
                            voice = merge_voice(first, voice, provider.voice());
                            first = false;
                        }
                    }
                    let gain = self.gate_step_reference(p, voice, m);
                    for sample in x.iter_mut().take(p.channels()) {
                        *sample *= gain;
                    }
                }
                Effect::Eq => {
                    if self.eq_remaining != 0 {
                        let mut prior = x;
                        self.eq[1 - self.eq_current].process(&mut prior, p.channels(), m);
                        self.eq[self.eq_current].process(&mut x, p.channels(), m);
                        let mix = (EQ_FADE_SAMPLES - self.eq_remaining + 1) as f64
                            / EQ_FADE_SAMPLES as f64;
                        for ch in 0..p.channels() {
                            x[ch] = prior[ch] + mix * (x[ch] - prior[ch]);
                        }
                        self.eq_remaining -= 1;
                    } else {
                        self.eq[self.eq_current].process(&mut x, p.channels(), m);
                    }
                }
                Effect::Compressor => {
                    if p.comp_on {
                        let peak = x[0].abs().max(x[1].abs());
                        let coeff = if peak > self.envelope {
                            p.comp_attack
                        } else {
                            p.comp_release
                        };
                        self.envelope = coeff * self.envelope + (1.0 - coeff) * peak;
                        let over = if self.envelope > p.comp_threshold {
                            (20.0 * self.envelope.max(1e-20).log10() - p.threshold_db).max(0.0)
                        } else {
                            0.0
                        };
                        let gr = over * (1.0 - 1.0 / p.ratio);
                        let gain = (if gr > 0.0 { db_gain(-gr) } else { 1.0 }) * p.makeup;
                        reduction += gr;
                        for sample in x.iter_mut().take(p.channels()) {
                            *sample *= gain;
                        }
                    }
                }
                Effect::Gain => {
                    for sample in x.iter_mut().take(p.channels()) {
                        *sample *= self.gain;
                    }
                }
                Effect::Limiter => {
                    if p.limit_on {
                        let peak = x[0].abs().max(x[1].abs());
                        let needed = if peak > p.ceiling {
                            p.ceiling / peak
                        } else {
                            1.0
                        };
                        self.limiter_gain =
                            needed.min(1.0 - (1.0 - self.limiter_gain) * p.limiter_release);
                        if self.limiter_gain < 1.0 {
                            reduction += -20.0 * self.limiter_gain.max(1e-20).log10();
                        }
                        for sample in x.iter_mut().take(p.channels()) {
                            *sample *= self.limiter_gain;
                        }
                    }
                }
            }
            for sample in x.iter_mut().take(p.channels()) {
                *sample = safe(*sample, 16.0, m);
            }
        }
        m.gain_reduction = m.gain_reduction.max(reduction);
        let mut output = [0.0; 2];
        for ch in 0..p.channels() {
            let y = safe(
                bypass[ch] + (x[ch] - bypass[ch]) * self.processing_blend,
                1.0,
                m,
            );
            output[ch] = y as f32;
            m.output_peak = m.output_peak.max(y.abs());
            m.sum_squares += y * y;
            m.samples += 1;
        }
        output
    }
}

/// The largest magnitude in `x`, as bits. For finite values the bit order of
/// `abs` is the numeric order, and every infinity and NaN sorts above them. So
/// `max_abs_bits(x) <= limit.to_bits()`, for a finite positive `limit`, says
/// every sample is finite and within `limit`: `safe` would return each one
/// unchanged and count nothing. The comparison has to stay on the integers;
/// as floats it would let a NaN through.
#[inline(always)]
pub(crate) fn max_abs_bits(x: &[f64]) -> u64 {
    x.iter().fold(0, |most, sample| {
        most.max(sample.to_bits() & (u64::MAX >> 1))
    })
}

/// `safe` on every sample, bit for bit: one check clears a clean block, and
/// any other block takes the per-sample loop.
#[inline(always)]
pub(crate) fn clamp(samples: &mut [f64], limit: f64, m: &mut Meter) {
    if max_abs_bits(samples) > limit.to_bits() {
        for sample in samples {
            *sample = safe(*sample, limit, m);
        }
    }
}

/// One smoothed gain over a block: a single value once the smoother has
/// stopped moving, otherwise its value after each sample's step.
///
/// The steps stay inline: this lives on the callback's stack for one block,
/// where nothing may allocate, and a flat ramp never writes them.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Ramp {
    Flat(f64),
    Moving([f64; BLOCK]),
}
impl Ramp {
    /// Steps `value` towards `target` once per sample, `n` times. A value that
    /// one step leaves bit-identical is a fixed point of the step, so every
    /// later step returns it too and the state does not change. The test is
    /// on the bits of the step, not `value == target`: the smoother can stall
    /// an ulp short of its target, and a negative zero has one more step to
    /// take.
    #[inline(always)]
    pub(crate) fn advance(value: &mut f64, target: f64, n: usize) -> Self {
        if (*value + GAIN_SMOOTH * (target - *value)).to_bits() == value.to_bits() {
            return Self::Flat(*value);
        }
        let mut steps = [0.0; BLOCK];
        for step in &mut steps[..n] {
            *value += GAIN_SMOOTH * (target - *value);
            *step = *value;
        }
        Self::Moving(steps)
    }
    /// True when every sample's gain is exactly 1.
    #[inline(always)]
    pub(crate) fn is_one(&self) -> bool {
        matches!(self, Self::Flat(value) if *value == 1.0)
    }
    #[cfg(test)]
    fn at(&self, t: usize) -> f64 {
        match self {
            Self::Flat(value) => *value,
            Self::Moving(steps) => steps[t],
        }
    }
    /// Calls `apply(t, gain)` for each of the first `n` samples, in order.
    #[inline(always)]
    pub(crate) fn each(&self, n: usize, mut apply: impl FnMut(usize, f64)) {
        match self {
            Self::Flat(value) => (0..n).for_each(|t| apply(t, *value)),
            Self::Moving(steps) => steps[..n]
                .iter()
                .enumerate()
                .for_each(|(t, value)| apply(t, *value)),
        }
    }
}

/// One channel's block through its suppression provider, in place. Each
/// sample sees what `frame` does to it: `safe` at 16, the conversion to the
/// provider's f32, the provider with that sample's `wet`, the conversion
/// back, and its voice score merged into `voice`, which holds the scores of
/// the channels before this one (`first` when there are none).
///
/// `safe` changes no provider state and the provider reads no meter, so the
/// whole block is limited first and the provider then takes it in one call.
#[inline]
fn suppress(
    provider: &mut dyn Suppression,
    samples: &mut [f64],
    wet: &[f32],
    first: bool,
    voice: &mut [Option<f32>],
    m: &mut Meter,
) {
    let n = samples.len();
    clamp(samples, 16.0, m);
    let mut narrow = [0.0_f32; BLOCK];
    let narrow = &mut narrow[..n];
    for (narrow, sample) in narrow.iter_mut().zip(samples.iter()) {
        *narrow = *sample as f32;
    }
    if first {
        // Nothing to merge with: the scores are this channel's own.
        provider.process_block(narrow, wet, voice);
    } else {
        let mut scores = [None; BLOCK];
        let scores = &mut scores[..n];
        provider.process_block(narrow, wet, scores);
        for (so_far, score) in voice.iter_mut().zip(scores.iter()) {
            *so_far = merge_voice(false, *so_far, *score);
        }
    }
    for (sample, narrow) in samples.iter_mut().zip(narrow.iter()) {
        *sample = f64::from(*narrow);
    }
}

/// Combines channel voice scores for one sample: the lower one (a stereo
/// pair opens only on voice in both), and none if any channel has no score.
fn merge_voice(first: bool, so_far: Option<f32>, next: Option<f32>) -> Option<f32> {
    if first {
        next
    } else {
        so_far.zip(next).map(|(a, b)| a.min(b))
    }
}

#[cfg(test)]
mod block_tests;
#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod provider_block_tests;
