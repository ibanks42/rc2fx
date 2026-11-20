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
/// Samples over which the highpass, compressor and limiter fade in when they
/// are switched on and out when they are switched off: 10 ms, the time
/// constant of the gain smoother, so a switch sounds like a gain change.
/// The stage's output is mixed with its input linearly, so the fade ends on
/// an exact sample and the stage then costs nothing more.
pub const STAGE_FADE_SAMPLES: u32 = 480;
pub const MAX_SUPPRESSION_DELAY: usize = 2368;

/// Where a processor's output goes when it hands off to whatever replaces
/// it (`Dsp::hand_off`): to silence, before a processor with another delay;
/// or to its dry signal, before the unprocessed path, which plays the same
/// signal when the processor has no delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandOff {
    Silence,
    Dry,
}
/// Samples per channel each stage processes at a time; covers the mixer's
/// 112-sample period in one pass.
pub(crate) const BLOCK: usize = 128;
pub(crate) const GAIN_SMOOTH: f64 = 0.0020811647007;
pub(crate) const EQ_INTERNAL_LIMIT: f64 = 1.3407807929942597e154; // 2^512

/// Callback-facing owned worker endpoint. Its implementation must not allocate,
/// lock, wait, run inference, or call the control service in `process_sample`
/// or `process_block`: the model runs on the worker's own thread.
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
    /// `1 - 1 / ratio`: the share of the level over the threshold that the
    /// compressor takes off.
    comp_slope: f64,
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
            comp_slope: 1.0 - 1.0 / target.compressor.ratio,
            comp_threshold: db_gain(target.compressor.threshold),
            comp_attack: (-1.0 / (target.compressor.attack * 0.001 * SAMPLE_RATE)).exp(),
            comp_release: (-1.0 / (target.compressor.release * 0.001 * SAMPLE_RATE)).exp(),
            limiter_release: (-1.0 / (target.limiter.release * 0.001 * SAMPLE_RATE)).exp(),
        };
        // Every stage but an absent suppression provider stays in the order:
        // a highpass, compressor or limiter that is switched off fades out
        // over `STAGE_FADE_SAMPLES` and is skipped once it is silent, and EQ
        // and gain keep smoothing while off. An allocated microphone provider
        // continues while the chain is bypassed. Order changes without a
        // provider retain the existing filter/envelope histories and take
        // effect on the next block. Enabling/disabling suppression changes
        // geometry and requires restart.
        for stage in target.order.iter().copied() {
            let include = match stage {
                Effect::Suppression => geometry.is_some(),
                Effect::Highpass
                | Effect::Compressor
                | Effect::Limiter
                | Effect::Eq
                | Effect::Gain => true,
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
    pub fn requires_reset(&self, next: &Self) -> bool {
        self.signature != next.signature
    }
}

struct ChannelState {
    bypass_dry: Box<[f32]>,
    bypass_position: usize,
}

/// What the highpass, EQ, compressor and limiter keep from one sample to the
/// next. A change of effect order crossfades from a copy of it running the
/// old order to the original running the new one (`Dsp::order_remaining`).
#[derive(Clone, Copy)]
struct Stages {
    hp: [Filter; 2],
    eq: [EqBank; 2],
    eq_current: usize,
    eq_remaining: usize,
    envelope: f64,
    limiter_gain: f64,
    /// The limiter's reduction in dB for the meter, and the gain it was
    /// worked out from.
    limiter_reduction: f64,
    limiter_reduction_for: f64,
    /// Where each switchable stage is in its fade, 0 (off, skipped) to
    /// `STAGE_FADE_SAMPLES` (on, its output alone). See `fade_step`.
    hp_fade: u32,
    comp_fade: u32,
    limiter_fade: u32,
    /// The compressor's threshold in dB, slope and makeup gain, and the
    /// limiter's ceiling and release coefficient, each smoothed toward the
    /// prepared value with the gain's time constant (`Ramp::approach`), so
    /// that moving them changes the gain gradually. They move only while
    /// their stage runs, and take the prepared value outright when the stage
    /// is switched on from silence.
    comp_threshold_db: f64,
    comp_slope: f64,
    comp_makeup: f64,
    limiter_ceiling: f64,
    limiter_release: f64,
    /// While a compressor or limiter fades out, it runs on a copy of its
    /// detector (the envelope or the limiter gain), so the stage keeps the
    /// history it had when it was switched off, as it always has. Switched
    /// back on before the fade ends, it continues from the copy instead, so
    /// its gain does not jump.
    comp_scratch: Option<f64>,
    limiter_scratch: Option<f64>,
}

/// One owner serializes processing and snapshot handoff. No shared reference
/// counts, mutexes or service callbacks are part of this processor.
pub struct Dsp {
    signature: Signature,
    retired: bool,
    channels: [ChannelState; 2],
    latency: usize,
    /// The suppression provider. Validation admits suppression on a mono
    /// microphone only (the Chat voice mix is one too), so there is at most
    /// one, for channel 0.
    suppression: Option<Box<dyn Suppression>>,
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
    processing_blend: f64,
    stages: Stages,
    /// The effect order `stages` runs. A different order is taken up in
    /// `begin_block`: `prior` becomes a copy of `stages` that keeps running
    /// the old order (`prior_order`) for `order_remaining` more samples, and
    /// the output fades from its result to the new order's over
    /// `EQ_FADE_SAMPLES`. An order arriving during that fade waits for it to
    /// end. `prior` is allocated with the processor, never in a callback.
    order: [Effect; 6],
    order_count: usize,
    prior: Box<Stages>,
    prior_order: [Effect; 6],
    prior_count: usize,
    order_remaining: usize,
    /// The output's hand-off fade: 0 (handed off: silent, or the dry signal
    /// when `handoff_dry`) to `STAGE_FADE_SAMPLES` (the processed output),
    /// moving toward `handoff_on`, after `handoff_hold` samples of silence.
    /// See `start_silent`, `start_dry` and `hand_off`.
    handoff_fade: u32,
    handoff_on: bool,
    handoff_dry: bool,
    handoff_hold: usize,
    /// The serial of the `Prepared` whose `begin_block` would change nothing
    /// if it ran again, or 0. Only `begin_block` and `block` change the EQ
    /// banks, the fades and the filters; anything else that does must set
    /// this back to 0.
    seen: u64,
}
impl Stages {
    fn new(p: &Prepared) -> Self {
        Self {
            hp: [Filter::with_coefficients(p.hp); 2],
            eq: [EqBank::new(p, None), EqBank::default()],
            eq_current: 0,
            eq_remaining: 0,
            envelope: 0.0,
            limiter_gain: 1.0,
            // A gain of 1 has no reduction and is never looked up.
            limiter_reduction: 0.0,
            limiter_reduction_for: 1.0,
            hp_fade: if p.hp_on { STAGE_FADE_SAMPLES } else { 0 },
            comp_fade: if p.comp_on { STAGE_FADE_SAMPLES } else { 0 },
            limiter_fade: if p.limit_on { STAGE_FADE_SAMPLES } else { 0 },
            comp_threshold_db: p.threshold_db,
            comp_slope: p.comp_slope,
            comp_makeup: p.makeup,
            limiter_ceiling: p.ceiling,
            limiter_release: p.limiter_release,
            comp_scratch: None,
            limiter_scratch: None,
        }
    }
    /// `Dsp::begin_block` for these stages: takes up `p`'s EQ shape and
    /// targets and decides which filters smooth their coefficients. Returns
    /// whether another pass with the same `p` would change nothing.
    fn begin(&mut self, p: &Prepared) -> bool {
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
                self.hp[ch].begin(p.hp);
                steady &= !self.hp[ch].is_smoothing();
            }
            for bank in 0..2 {
                if bank == self.eq_current || self.eq_remaining != 0 {
                    self.eq[bank].begin(ch);
                }
            }
            steady &= !self.eq[self.eq_current].is_smoothing(ch);
        }
        steady
    }
    /// One stage other than suppression over `n` samples of `x`, as `block`
    /// runs it. Returns false when the stage left every sample within the
    /// stage limit, so the clamp after it can be skipped. `dynamics` is set
    /// when the compressor or limiter ran and wrote `reduction`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn block(
        &mut self,
        stage: Effect,
        p: &Prepared,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        gain: &Ramp,
        reduction: &mut [f64; BLOCK],
        dynamics: &mut bool,
        m: &mut Meter,
    ) -> bool {
        let channels = p.channels();
        match stage {
            Effect::Highpass => {
                if !p.hp_on && self.hp_fade == 0 {
                    // Off and faded out: nothing to do, and the samples are
                    // as the stage before left them.
                    return false;
                }
                if p.hp_on && self.hp_fade == STAGE_FADE_SAMPLES {
                    for (filter, samples) in self.hp.iter_mut().zip(x.iter_mut()).take(channels) {
                        filter.process_block(&p.hp, &mut samples[..n], 16.0, m);
                    }
                } else {
                    for t in 0..n {
                        let y = self.highpass_step(p, [x[0][t], x[1][t]], channels, m);
                        (x[0][t], x[1][t]) = (y[0], y[1]);
                    }
                }
            }
            Effect::Eq => {
                let fading = self.eq_remaining.min(n);
                if fading == 0 {
                    let bank = &mut self.eq[self.eq_current];
                    bank.process_block(x, n, channels, m);
                    // An enabled bank ends by limiting its own output to 16.
                    return !bank.is_on();
                }
                // The previous bank fades out over the first samples.
                let mut prior = *x;
                self.eq[1 - self.eq_current].process_block(&mut prior, fading, channels, m);
                self.eq[self.eq_current].process_block(x, n, channels, m);
                for t in 0..fading {
                    let mix =
                        (EQ_FADE_SAMPLES - self.eq_remaining + 1) as f64 / EQ_FADE_SAMPLES as f64;
                    for ch in 0..channels {
                        x[ch][t] = prior[ch][t] + mix * (x[ch][t] - prior[ch][t]);
                    }
                    self.eq_remaining -= 1;
                }
            }
            Effect::Compressor => {
                if !p.comp_on && self.comp_fade == 0 {
                    return false;
                }
                *dynamics = true;
                if p.comp_on && self.comp_fade == STAGE_FADE_SAMPLES {
                    self.compress_block(p, x, n, channels, reduction);
                } else {
                    for t in 0..n {
                        let y = self.compressor_step(
                            p,
                            [x[0][t], x[1][t]],
                            channels,
                            &mut reduction[t],
                        );
                        (x[0][t], x[1][t]) = (y[0], y[1]);
                    }
                }
            }
            Effect::Gain => {
                if gain.is_one() {
                    // Finite samples times exactly 1 are unchanged, and the
                    // stage before left them within the limit.
                    return false;
                }
                for samples in x.iter_mut().take(channels) {
                    let samples = &mut samples[..n];
                    gain.each(n, |t, gain| samples[t] *= gain);
                }
            }
            Effect::Limiter => {
                if !p.limit_on && self.limiter_fade == 0 {
                    return false;
                }
                *dynamics = true;
                if p.limit_on && self.limiter_fade == STAGE_FADE_SAMPLES {
                    self.limit_block(p, x, n, channels, reduction);
                } else {
                    for t in 0..n {
                        let y =
                            self.limiter_step(p, [x[0][t], x[1][t]], channels, &mut reduction[t]);
                        (x[0][t], x[1][t]) = (y[0], y[1]);
                    }
                }
            }
            Effect::Suppression => unreachable!("suppression belongs to the processor"),
        }
        true
    }
    /// One stage other than suppression for one sample, as `Dsp::frame`
    /// runs it; `gain` is that sample's smoothed gain.
    #[cfg(test)]
    fn frame(
        &mut self,
        stage: Effect,
        p: &Prepared,
        x: &mut [f64; 2],
        gain: f64,
        reduction: &mut f64,
        m: &mut Meter,
    ) {
        let channels = p.channels();
        match stage {
            Effect::Highpass => *x = self.highpass_step(p, *x, channels, m),
            Effect::Eq => {
                if self.eq_remaining != 0 {
                    let mut prior = *x;
                    self.eq[1 - self.eq_current].process(&mut prior, channels, m);
                    self.eq[self.eq_current].process(x, channels, m);
                    let mix =
                        (EQ_FADE_SAMPLES - self.eq_remaining + 1) as f64 / EQ_FADE_SAMPLES as f64;
                    for ch in 0..channels {
                        x[ch] = prior[ch] + mix * (x[ch] - prior[ch]);
                    }
                    self.eq_remaining -= 1;
                } else {
                    self.eq[self.eq_current].process(x, channels, m);
                }
            }
            Effect::Compressor => *x = self.compressor_step(p, *x, channels, reduction),
            Effect::Gain => {
                for sample in x.iter_mut().take(channels) {
                    *sample *= gain;
                }
            }
            Effect::Limiter => *x = self.limiter_step(p, *x, channels, reduction),
            Effect::Suppression => unreachable!("suppression belongs to the processor"),
        }
    }
    /// Moves a stage's fade one sample toward on or off and returns where it
    /// now is: 0 skips the stage for this sample.
    #[inline(always)]
    fn fade_step(position: &mut u32, on: bool) -> u32 {
        if on {
            if *position < STAGE_FADE_SAMPLES {
                *position += 1;
            }
        } else if *position > 0 {
            *position -= 1;
        }
        *position
    }
    /// What a stage at fade `position` (not 0) passes on, from its input `x`
    /// and its own output `y`: `y` alone once fully on.
    #[inline(always)]
    fn faded(x: f64, y: f64, position: u32) -> f64 {
        if position == STAGE_FADE_SAMPLES {
            y
        } else {
            x + (y - x) * (position as f64 / STAGE_FADE_SAMPLES as f64)
        }
    }
    /// One sample of the highpass stage: its fade, and the filter while it
    /// runs. Switched on from silence, the filter starts at rest on the
    /// prepared coefficients instead of the history and coefficients it was
    /// left with, which belong to audio long gone and would ring out. `frame`
    /// takes every sample here; the block path takes the samples of a fade.
    #[inline]
    fn highpass_step(
        &mut self,
        p: &Prepared,
        x: [f64; 2],
        channels: usize,
        m: &mut Meter,
    ) -> [f64; 2] {
        if p.hp_on && self.hp_fade == 0 {
            for filter in self.hp.iter_mut().take(channels) {
                *filter = Filter::at_rest(p.hp);
            }
        }
        let position = Self::fade_step(&mut self.hp_fade, p.hp_on);
        let mut y = x;
        if position == 0 {
            return y;
        }
        for (ch, sample) in y.iter_mut().enumerate().take(channels) {
            let filtered = self.hp[ch].process(&p.hp, x[ch], 16.0, m);
            *sample = Self::faded(x[ch], filtered, position);
        }
        y
    }
    /// One sample of the compressor stage: its fade, its smoothed settings,
    /// its detector (or the copy of it a fade-out runs on) and its gain.
    /// `reduction` gets the reduction in dB. `frame` takes every sample here;
    /// the block path takes the samples of a fade.
    #[inline]
    fn compressor_step(
        &mut self,
        p: &Prepared,
        x: [f64; 2],
        channels: usize,
        reduction: &mut f64,
    ) -> [f64; 2] {
        if p.comp_on && self.comp_fade == 0 {
            self.comp_threshold_db = p.threshold_db;
            self.comp_slope = p.comp_slope;
            self.comp_makeup = p.makeup;
        }
        let position = Self::fade_step(&mut self.comp_fade, p.comp_on);
        if position == 0 {
            self.comp_scratch = None;
            return x;
        }
        Ramp::approach_step(&mut self.comp_threshold_db, p.threshold_db);
        Ramp::approach_step(&mut self.comp_slope, p.comp_slope);
        Ramp::approach_step(&mut self.comp_makeup, p.makeup);
        let envelope = if p.comp_on {
            if let Some(kept) = self.comp_scratch.take() {
                self.envelope = kept;
            }
            &mut self.envelope
        } else {
            self.comp_scratch.get_or_insert(self.envelope)
        };
        let (scale, gr) = compressor_gain(
            envelope,
            x[0].abs().max(x[1].abs()),
            p,
            [self.comp_threshold_db, self.comp_slope, self.comp_makeup],
            self.comp_threshold_db.to_bits() != p.threshold_db.to_bits(),
        );
        *reduction += gr;
        let mut y = x;
        for (ch, sample) in y.iter_mut().enumerate().take(channels) {
            *sample = Self::faded(x[ch], x[ch] * scale, position);
        }
        y
    }
    /// One sample of the limiter stage, as `compressor_step` is for the
    /// compressor.
    #[inline]
    fn limiter_step(
        &mut self,
        p: &Prepared,
        x: [f64; 2],
        channels: usize,
        reduction: &mut f64,
    ) -> [f64; 2] {
        if p.limit_on && self.limiter_fade == 0 {
            self.limiter_ceiling = p.ceiling;
            self.limiter_release = p.limiter_release;
        }
        let position = Self::fade_step(&mut self.limiter_fade, p.limit_on);
        if position == 0 {
            self.limiter_scratch = None;
            return x;
        }
        Ramp::approach_step(&mut self.limiter_ceiling, p.ceiling);
        Ramp::approach_step(&mut self.limiter_release, p.limiter_release);
        let held = if p.limit_on {
            if let Some(kept) = self.limiter_scratch.take() {
                self.limiter_gain = kept;
            }
            self.limiter_gain
        } else {
            *self.limiter_scratch.get_or_insert(self.limiter_gain)
        };
        let gain = limiter_gain(
            held,
            x[0].abs().max(x[1].abs()),
            self.limiter_ceiling,
            self.limiter_release,
        );
        if p.limit_on {
            self.limiter_gain = gain;
        } else {
            self.limiter_scratch = Some(gain);
        }
        if gain < 1.0 {
            *reduction += self.limiter_reduction_of(gain);
        }
        let mut y = x;
        for (ch, sample) in y.iter_mut().enumerate().take(channels) {
            *sample = Self::faded(x[ch], x[ch] * gain, position);
        }
        y
    }
    /// The limiter's reduction in dB at `gain`. After its first reduction
    /// the gain never gets back to exactly 1: it stops just below and holds
    /// that value. The logarithm, which only the meter reads, is taken when
    /// the gain changes, not for every sample.
    #[inline(always)]
    fn limiter_reduction_of(&mut self, gain: f64) -> f64 {
        if gain.to_bits() != self.limiter_reduction_for.to_bits() {
            self.limiter_reduction_for = gain;
            self.limiter_reduction = -20.0 * gain.max(1e-20).log10();
        }
        self.limiter_reduction
    }
    /// The compressor over a block while it is fully on and stays on:
    /// `compressor_step` for every sample, bit for bit.
    #[inline]
    fn compress_block(
        &mut self,
        p: &Prepared,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        channels: usize,
        reduction: &mut [f64; BLOCK],
    ) {
        let threshold = Ramp::approach(&mut self.comp_threshold_db, p.threshold_db, n);
        let slope = Ramp::approach(&mut self.comp_slope, p.comp_slope, n);
        let makeup = Ramp::approach(&mut self.comp_makeup, p.makeup, n);
        let mut sample = |t: usize, settings: [f64; 3], moving: bool| {
            let peak = x[0][t].abs().max(x[1][t].abs());
            let (scale, gr) = compressor_gain(&mut self.envelope, peak, p, settings, moving);
            reduction[t] += gr;
            for samples in x.iter_mut().take(channels) {
                samples[t] *= scale;
            }
        };
        if let (Ramp::Flat(threshold), Ramp::Flat(slope), Ramp::Flat(makeup)) =
            (&threshold, &slope, &makeup)
        {
            // Settled: the settings of the block are the prepared ones (a
            // flat `approach` has reached its target).
            for t in 0..n {
                sample(t, [*threshold, *slope, *makeup], false);
            }
        } else {
            for t in 0..n {
                let threshold = threshold.at(t);
                sample(
                    t,
                    [threshold, slope.at(t), makeup.at(t)],
                    threshold.to_bits() != p.threshold_db.to_bits(),
                );
            }
        }
    }
    /// The limiter over a block while it is fully on and stays on:
    /// `limiter_step` for every sample, bit for bit.
    #[inline]
    fn limit_block(
        &mut self,
        p: &Prepared,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        channels: usize,
        reduction: &mut [f64; BLOCK],
    ) {
        let ceiling = Ramp::approach(&mut self.limiter_ceiling, p.ceiling, n);
        let release = Ramp::approach(&mut self.limiter_release, p.limiter_release, n);
        let mut sample = |dsp: &mut Self, t: usize, ceiling: f64, release: f64| {
            let peak = x[0][t].abs().max(x[1][t].abs());
            dsp.limiter_gain = limiter_gain(dsp.limiter_gain, peak, ceiling, release);
            if dsp.limiter_gain < 1.0 {
                reduction[t] += dsp.limiter_reduction_of(dsp.limiter_gain);
            }
            for samples in x.iter_mut().take(channels) {
                samples[t] *= dsp.limiter_gain;
            }
        };
        if let (Ramp::Flat(ceiling), Ramp::Flat(release)) = (&ceiling, &release) {
            for t in 0..n {
                sample(self, t, *ceiling, *release);
            }
        } else {
            for t in 0..n {
                sample(self, t, ceiling.at(t), release.at(t));
            }
        }
    }
}

impl Dsp {
    /// This constructor is deliberately outside the audio callback. Only enabled
    /// suppression needs a provider; bypass retains that provider and its delay.
    /// A mono target takes it: validation admits suppression on no other.
    pub fn new(p: &Prepared, suppression: Option<Box<dyn Suppression>>) -> Result<Self, DspError> {
        if p.is_chat_voice() {
            // A completed Chat mix contains pads. Only Chat::process, with a
            // separately captured microphone stem, may use its suppressor.
            return Err(DspError::MissingChatStem);
        }
        let mut latency = 0;
        if p.signature.geometry.is_some() {
            let provider = suppression.as_ref().ok_or(DspError::MissingSuppression)?;
            let geometry = provider.geometry();
            if Some(geometry) != p.signature.geometry || p.channels() != 1 {
                return Err(DspError::SuppressionGeometry);
            }
            let delay = provider.latency_samples();
            if delay != geometry.latency_samples() as usize
                || delay == 0
                || delay > MAX_SUPPRESSION_DELAY
            {
                return Err(DspError::SuppressionGeometry);
            }
            latency = delay;
        } else if suppression.is_some() {
            return Err(DspError::UnexpectedSuppression);
        }
        Ok(Self {
            signature: p.signature,
            retired: false,
            channels: std::array::from_fn(|ch| ChannelState {
                bypass_dry: vec![0.0; if ch < p.channels() { latency } else { 0 }]
                    .into_boxed_slice(),
                bypass_position: 0,
            }),
            latency,
            suppression,
            gain: if p.gain_on { p.gain } else { 1.0 },
            wet: if p.noise_on { p.wet } else { 0.0 },
            gate_db: 0.0,
            gate_gain: 1.0,
            gate_hold: 0,
            gate_voiced: true,
            // An open gate with no floor reads fully open.
            gate_open: 1.0,
            gate_open_for: [0.0_f64.to_bits(); 2],
            processing_blend: if p.bypass { 0.0 } else { 1.0 },
            stages: Stages::new(p),
            order: p.active_order,
            order_count: p.active_count,
            prior: Box::new(Stages::new(p)),
            prior_order: p.active_order,
            prior_count: p.active_count,
            order_remaining: 0,
            handoff_fade: STAGE_FADE_SAMPLES,
            handoff_on: true,
            handoff_dry: false,
            handoff_hold: 0,
            seen: 0,
        })
    }
    /// Control, before the processor's first block: start silent, stay
    /// silent for `hold` samples, then fade in over `STAGE_FADE_SAMPLES`. A
    /// processor that replaces another passes its own delay, so the fade
    /// runs over the first delayed audio instead of its empty delay line.
    pub fn start_silent(&mut self, hold: usize) {
        self.handoff_fade = 0;
        self.handoff_on = true;
        self.handoff_dry = false;
        self.handoff_hold = hold;
    }
    /// Control, before the processor's first block: start at the dry signal
    /// and fade to the processed output over `STAGE_FADE_SAMPLES`. For a
    /// processor without a delay that takes over from the unprocessed path.
    pub fn start_dry(&mut self) {
        self.handoff_fade = 0;
        self.handoff_on = true;
        self.handoff_dry = true;
        self.handoff_hold = 0;
    }
    /// Callback, before a block: `Some` fades the output out to silence or
    /// to the dry signal over `STAGE_FADE_SAMPLES` (`handed_off` then says
    /// so); `None` fades it back in. A fade already under way keeps its
    /// destination, so the output never jumps. Costs a few stores.
    pub fn hand_off(&mut self, to: Option<HandOff>) {
        match to {
            Some(to) => {
                if self.handoff_fade == STAGE_FADE_SAMPLES {
                    self.handoff_dry = to == HandOff::Dry;
                }
                // Still silent from its start: handed off already.
                self.handoff_hold = 0;
                self.handoff_on = false;
            }
            None => self.handoff_on = true,
        }
    }
    /// Whether the output has faded out completely after `hand_off`.
    pub fn handed_off(&self) -> bool {
        !self.handoff_on && self.handoff_fade == 0 && self.handoff_hold == 0
    }
    /// One sample of the hand-off fade: the share of the processed output to
    /// keep, 1 once fully in.
    #[inline(always)]
    fn handoff_step(&mut self) -> f64 {
        if self.handoff_hold > 0 {
            self.handoff_hold -= 1;
            return 0.0;
        }
        Stages::fade_step(&mut self.handoff_fade, self.handoff_on) as f64
            / STAGE_FADE_SAMPLES as f64
    }
    /// `handoff_step` for `n` samples: flat 1 without a step while the
    /// output is fully in, as it is but for a switch.
    #[inline(always)]
    fn handoff_ramp(&mut self, n: usize) -> Ramp {
        if self.handoff_on && self.handoff_fade == STAGE_FADE_SAMPLES && self.handoff_hold == 0 {
            return Ramp::Flat(1.0);
        }
        let mut steps = [0.0; BLOCK];
        for step in &mut steps[..n] {
            *step = self.handoff_step();
        }
        Ramp::Moving(steps)
    }
    pub fn without_suppression(p: &Prepared) -> Result<Self, DspError> {
        Self::new(p, None)
    }
    pub fn latency_samples(&self) -> usize {
        self.latency
    }
    /// Irreversible control-side retirement after callback quiescence. Retry
    /// can observe a late join, but processing remains disabled even if the
    /// first call times out.
    pub fn drain_suppression(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<(), RetirementError> {
        self.retired = true;
        let end = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or(RetirementError::TimedOut)?;
        match &mut self.suppression {
            Some(provider) => {
                provider.drain(end.saturating_duration_since(std::time::Instant::now()))
            }
            None => Ok(()),
        }
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
    /// Takes up `p`'s effect order (starting a crossfade to it), EQ shape and
    /// targets, and decides which filters smooth their coefficients over
    /// this block.
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
        let mut steady = true;
        if p.active_order[..p.active_count] != self.order[..self.order_count] {
            if self.order_remaining == 0 {
                *self.prior = self.stages;
                self.prior_order = self.order;
                self.prior_count = self.order_count;
                self.order = p.active_order;
                self.order_count = p.active_count;
                self.order_remaining = EQ_FADE_SAMPLES;
            }
            // Otherwise the order is taken up after the running fade.
            steady = false;
        }
        steady &= self.stages.begin(p);
        if self.order_remaining != 0 {
            self.prior.begin(p);
            steady = false;
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
            && self.stages.hp_fade == 0
            && self.stages.comp_fade == 0
            && self.stages.limiter_fade == 0
            && self.order_remaining == 0
            && self.handoff_on
            && self.handoff_fade == STAGE_FADE_SAMPLES
            && self.handoff_hold == 0
            && (if p.gain_on { p.gain } else { 1.0 }) == 1.0
            && self.gain == 1.0
            && self.stages.eq_remaining == 0
            && self.stages.eq[self.stages.eq_current].steady_disabled()
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
    /// The effect order over `n` samples of `x`: the current one on
    /// `stages`, or with `prior` the old one on its copy during an order
    /// fade. `reduction` gets the compressor's and limiter's reductions;
    /// returns whether either ran.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn stages_block(
        &mut self,
        p: &Prepared,
        prior: bool,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        gain: &Ramp,
        wet: &Ramp,
        reduction: &mut [f64; BLOCK],
        m: &mut Meter,
    ) -> bool {
        let (order, count) = if prior {
            (self.prior_order, self.prior_count)
        } else {
            (self.order, self.order_count)
        };
        let mut dynamics = false;
        for stage in order[..count].iter().copied() {
            if stage == Effect::Suppression {
                // A provider fixes the order: it never changes, so only the
                // current order has this stage.
                debug_assert!(!prior);
                self.suppression_block(p, x, n, wet, m);
            } else {
                let stages = if prior {
                    &mut *self.prior
                } else {
                    &mut self.stages
                };
                if !stages.block(stage, p, x, n, gain, reduction, &mut dynamics, m) {
                    continue;
                }
            }
            for samples in x.iter_mut().take(p.channels()) {
                clamp(&mut samples[..n], 16.0, m);
            }
        }
        dynamics
    }
    /// `stages_block` while an order change fades in: the old order runs on
    /// its copy of the stages for the samples of the fade, from the same
    /// input, and the output moves from its result to the new order's.
    #[inline(never)]
    fn order_fade_block(
        &mut self,
        p: &Prepared,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        gain: &Ramp,
        wet: &Ramp,
        m: &mut Meter,
    ) {
        let fading = self.order_remaining.min(n);
        let mut prior = *x;
        let mut prior_reduction = [0.0; BLOCK];
        let prior_dynamics = self.stages_block(
            p,
            true,
            &mut prior,
            fading,
            gain,
            wet,
            &mut prior_reduction,
            m,
        );
        let mut reduction = [0.0; BLOCK];
        let dynamics = self.stages_block(p, false, x, n, gain, wet, &mut reduction, m);
        for t in 0..fading {
            let mix = (EQ_FADE_SAMPLES - self.order_remaining + 1) as f64 / EQ_FADE_SAMPLES as f64;
            for ch in 0..p.channels() {
                x[ch][t] = prior[ch][t] + mix * (x[ch][t] - prior[ch][t]);
            }
            self.order_remaining -= 1;
        }
        if dynamics {
            for value in &reduction[..n] {
                m.gain_reduction = m.gain_reduction.max(*value);
            }
        }
        if prior_dynamics {
            for value in &prior_reduction[..fading] {
                m.gain_reduction = m.gain_reduction.max(*value);
            }
        }
    }
    /// The suppression stage over `n` samples of `x`: the provider on the
    /// one channel, then the voice gate.
    #[inline]
    fn suppression_block(
        &mut self,
        p: &Prepared,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        wet: &Ramp,
        m: &mut Meter,
    ) {
        let channels = p.channels();
        let mut voice = [None; BLOCK];
        let mut wet32 = [0.0; BLOCK];
        wet.each(n, |t, wet| wet32[t] = wet as f32);
        if let Some(provider) = self.suppression.as_mut() {
            suppress(
                provider.as_mut(),
                &mut x[0][..n],
                &wet32[..n],
                &mut voice[..n],
                m,
            );
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
    /// The stages of the effect order for one sample, as `stages_block` runs
    /// them for a block. Returns the compressor's and limiter's reduction.
    #[cfg(test)]
    fn stages_frame(&mut self, p: &Prepared, prior: bool, x: &mut [f64; 2], m: &mut Meter) -> f64 {
        let (order, count) = if prior {
            (self.prior_order, self.prior_count)
        } else {
            (self.order, self.order_count)
        };
        let mut reduction = 0.0;
        for stage in order[..count].iter().copied() {
            if stage == Effect::Suppression {
                let mut voice = None;
                if let Some(provider) = self.suppression.as_mut() {
                    let sample = &mut x[0];
                    *sample = provider
                        .process_sample(safe(*sample, 16.0, m) as f32, self.wet as f32)
                        as f64;
                    voice = provider.voice();
                }
                let gain = self.gate_step_reference(p, voice, m);
                for sample in x.iter_mut().take(p.channels()) {
                    *sample *= gain;
                }
            } else {
                let gain = self.gain;
                let stages = if prior {
                    &mut *self.prior
                } else {
                    &mut self.stages
                };
                stages.frame(stage, p, x, gain, &mut reduction, m);
            }
            for sample in x.iter_mut().take(p.channels()) {
                *sample = safe(*sample, 16.0, m);
            }
        }
        reduction
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
        if self.order_remaining != 0 {
            self.order_fade_block(p, x, n, &gain, &wet, m);
        } else {
            let mut reduction = [0.0; BLOCK];
            if self.stages_block(p, false, x, n, &gain, &wet, &mut reduction, m) {
                for value in &reduction[..n] {
                    m.gain_reduction = m.gain_reduction.max(*value);
                }
            }
        }
        let handoff = self.handoff_ramp(n);
        let to_dry = self.handoff_dry;
        let mut peak = 0;
        for ch in 0..channels {
            let (mixed, dry) = (&mut x[ch][..n], &bypass[ch][..n]);
            blend.each(n, |t, blend| {
                mixed[t] = dry[t] + (mixed[t] - dry[t]) * blend
            });
            if !handoff.is_one() {
                handoff.each(n, |t, gain| {
                    mixed[t] = handed_off(mixed[t], dry[t], gain, to_dry)
                });
            }
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
        let reduction;
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
        if self.order_remaining != 0 {
            let mut prior = x;
            let prior_reduction = self.stages_frame(p, true, &mut prior, m);
            reduction = self.stages_frame(p, false, &mut x, m);
            let mix = (EQ_FADE_SAMPLES - self.order_remaining + 1) as f64 / EQ_FADE_SAMPLES as f64;
            for ch in 0..p.channels() {
                x[ch] = prior[ch] + mix * (x[ch] - prior[ch]);
            }
            self.order_remaining -= 1;
            m.gain_reduction = m.gain_reduction.max(prior_reduction);
        } else {
            reduction = self.stages_frame(p, false, &mut x, m);
        }
        m.gain_reduction = m.gain_reduction.max(reduction);
        let handoff = self.handoff_step();
        let mut output = [0.0; 2];
        for ch in 0..p.channels() {
            let y = safe(
                handed_off(
                    bypass[ch] + (x[ch] - bypass[ch]) * self.processing_blend,
                    bypass[ch],
                    handoff,
                    self.handoff_dry,
                ),
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

/// The output `y` with the hand-off gain applied: `y` itself at 1, otherwise
/// faded toward silence or toward `dry`.
#[inline(always)]
fn handed_off(y: f64, dry: f64, gain: f64, to_dry: bool) -> f64 {
    if gain == 1.0 {
        y
    } else if to_dry {
        dry + (y - dry) * gain
    } else {
        y * gain
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
    /// One step of `approach`.
    #[inline(always)]
    pub(crate) fn approach_step(value: &mut f64, target: f64) {
        if value.to_bits() != target.to_bits() {
            let next = *value + GAIN_SMOOTH * (target - *value);
            *value = if next.to_bits() == value.to_bits() {
                target
            } else {
                next
            };
        }
    }
    /// `advance` for a setting: a value the step can no longer move takes
    /// the target exactly (a jump of a few hundred ulps at most), so the
    /// setting ends on the prepared value instead of stalling short of it,
    /// and whatever compares the two finds them equal.
    #[inline(always)]
    pub(crate) fn approach(value: &mut f64, target: f64, n: usize) -> Self {
        if value.to_bits() == target.to_bits() {
            return Self::Flat(*value);
        }
        let mut steps = [0.0; BLOCK];
        for step in &mut steps[..n] {
            Self::approach_step(value, target);
            *step = *value;
        }
        Self::Moving(steps)
    }
    /// True when every sample's gain is exactly 1.
    #[inline(always)]
    pub(crate) fn is_one(&self) -> bool {
        matches!(self, Self::Flat(value) if *value == 1.0)
    }
    /// The gain of sample `t`.
    #[inline(always)]
    pub(crate) fn at(&self, t: usize) -> f64 {
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

/// One sample of the compressor's detector and gain curve: moves `envelope`
/// toward `peak` and returns the gain to apply and the reduction in dB. With
/// the threshold at the prepared value, the level is worked out only above
/// it; a threshold still on its way there (`moving`: not `p`'s, bit for bit)
/// works it out for every sample, so the curve moves with it.
#[inline(always)]
fn compressor_gain(
    envelope: &mut f64,
    peak: f64,
    p: &Prepared,
    [threshold_db, slope, makeup]: [f64; 3],
    moving: bool,
) -> (f64, f64) {
    let coeff = if peak > *envelope {
        p.comp_attack
    } else {
        p.comp_release
    };
    *envelope = coeff * *envelope + (1.0 - coeff) * peak;
    let over = if moving || *envelope > p.comp_threshold {
        (20.0 * envelope.max(1e-20).log10() - threshold_db).max(0.0)
    } else {
        0.0
    };
    let gr = over * slope;
    ((if gr > 0.0 { db_gain(-gr) } else { 1.0 }) * makeup, gr)
}

/// One sample of the limiter's gain: down at once to what keeps `peak` under
/// the ceiling, otherwise back toward 1 at the release rate from `held`.
#[inline(always)]
fn limiter_gain(held: f64, peak: f64, ceiling: f64, release: f64) -> f64 {
    let needed = if peak > ceiling { ceiling / peak } else { 1.0 };
    needed.min(1.0 - (1.0 - held) * release)
}

/// The channel's block through its suppression provider, in place. Each
/// sample sees what `frame` does to it: `safe` at 16, the conversion to the
/// provider's f32, the provider with that sample's `wet`, the conversion
/// back, and its voice score into `voice`.
///
/// `safe` changes no provider state and the provider reads no meter, so the
/// whole block is limited first and the provider then takes it in one call.
#[inline]
fn suppress(
    provider: &mut dyn Suppression,
    samples: &mut [f64],
    wet: &[f32],
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
    provider.process_block(narrow, wet, voice);
    for (sample, narrow) in samples.iter_mut().zip(narrow.iter()) {
        *sample = f64::from(*narrow);
    }
}

#[cfg(test)]
mod tests;
