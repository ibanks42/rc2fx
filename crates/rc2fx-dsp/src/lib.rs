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

pub const EQ_FADE_SAMPLES: usize = 240;
pub const MAX_SUPPRESSION_DELAY: usize = 2368;
/// Samples per channel each stage processes at a time; covers the mixer's
/// 112-sample period in one pass.
pub(crate) const BLOCK: usize = 128;
pub(crate) const GAIN_SMOOTH: f64 = 0.0020811647007;
pub(crate) const EQ_INTERNAL_LIMIT: f64 = 1.3407807929942597e154; // 2^512

/// Callback-facing owned worker endpoint. Its implementation must not allocate,
/// lock, wait, or call the control service in `process_sample`. Inference
/// there is allowed only for an inline endpoint whose model's worst frame
/// fits the calling thread's period budget (the mixer's DSP helper).
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
#[derive(Clone)]
pub struct Prepared {
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
        let mut p = Self {
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
    envelope: f64,
    limiter_gain: f64,
    processing_blend: f64,
    eq: [EqBank; 2],
    eq_current: usize,
    eq_remaining: usize,
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
            envelope: 0.0,
            limiter_gain: 1.0,
            processing_blend: if p.bypass { 0.0 } else { 1.0 },
            eq: [EqBank::new(p, None), EqBank::default()],
            eq_current: 0,
            eq_remaining: 0,
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
    fn begin_block(&mut self, p: &Prepared, frames: usize) {
        if frames == 0 {
            return;
        }
        let current = self.eq_current;
        if !self.eq[current].shape.same_as(&p.eq_shape) {
            if self.eq_remaining == 0 {
                self.eq[1 - current] = EqBank::new(p, Some(&self.eq[current]));
                self.eq_current = 1 - current;
                self.eq_remaining = EQ_FADE_SAMPLES;
            }
        } else {
            self.eq[current].update_targets(p);
        }
        for ch in 0..p.channels() {
            if p.hp_on {
                self.channels[ch].hp.begin(p.hp);
            }
            for bank in 0..2 {
                if bank == self.eq_current || self.eq_remaining != 0 {
                    self.eq[bank].begin(ch);
                }
            }
        }
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
    fn block(&mut self, p: &Prepared, x: &mut [[f64; BLOCK]; 2], n: usize, m: &mut Meter) {
        let channels = p.channels();
        let mut bypass = [[0.0; BLOCK]; 2];
        for ch in 0..channels {
            let c = &mut self.channels[ch];
            for t in 0..n {
                let raw = x[ch][t];
                if raw.is_finite() {
                    m.input_peak = m.input_peak.max(raw.abs());
                }
                x[ch][t] = safe(raw, 16.0, m);
                bypass[ch][t] = x[ch][t];
                if self.latency != 0 {
                    bypass[ch][t] = c.bypass_dry[c.bypass_position] as f64;
                    c.bypass_dry[c.bypass_position] = x[ch][t] as f32;
                    c.bypass_position += 1;
                    if c.bypass_position == self.latency {
                        c.bypass_position = 0;
                    }
                }
            }
        }
        let mut gain = [0.0; BLOCK];
        let mut wet = [0.0; BLOCK];
        let mut blend = [0.0; BLOCK];
        for t in 0..n {
            self.gain += GAIN_SMOOTH * ((if p.gain_on { p.gain } else { 1.0 }) - self.gain);
            self.wet += GAIN_SMOOTH * ((if p.noise_on { p.wet } else { 0.0 }) - self.wet);
            self.processing_blend +=
                GAIN_SMOOTH * ((if p.bypass { 0.0 } else { 1.0 }) - self.processing_blend);
            gain[t] = self.gain;
            wet[t] = self.wet;
            blend[t] = self.processing_blend;
        }
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
                    let mut first = true;
                    for (state, samples) in
                        self.channels.iter_mut().zip(x.iter_mut()).take(channels)
                    {
                        if let Some(provider) = state.suppression.as_mut() {
                            for ((sample, wet), score) in
                                samples[..n].iter_mut().zip(&wet).zip(&mut voice)
                            {
                                *sample = provider
                                    .process_sample(safe(*sample, 16.0, m) as f32, *wet as f32)
                                    as f64;
                                *score = merge_voice(first, *score, provider.voice());
                            }
                            first = false;
                        }
                    }
                    for (t, score) in voice[..n].iter().enumerate() {
                        let gain = self.gate_step(p, *score, m);
                        for samples in x.iter_mut().take(channels) {
                            samples[t] *= gain;
                        }
                    }
                }
                Effect::Eq => {
                    // The previous bank fades out over the first samples.
                    let fading = self.eq_remaining.min(n);
                    let mut prior = *x;
                    if fading != 0 {
                        self.eq[1 - self.eq_current].process_block(&mut prior, fading, channels, m);
                    }
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
                    for samples in x.iter_mut().take(channels) {
                        for (sample, gain) in samples[..n].iter_mut().zip(&gain) {
                            *sample *= gain;
                        }
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
                                reduction[t] += -20.0 * self.limiter_gain.max(1e-20).log10();
                            }
                            for samples in x.iter_mut().take(channels) {
                                samples[t] *= self.limiter_gain;
                            }
                        }
                    }
                }
            }
            for samples in x.iter_mut().take(channels) {
                for sample in &mut samples[..n] {
                    *sample = safe(*sample, 16.0, m);
                }
            }
        }
        for t in 0..n {
            m.gain_reduction = m.gain_reduction.max(reduction[t]);
            for ch in 0..channels {
                let y = safe(
                    bypass[ch][t] + (x[ch][t] - bypass[ch][t]) * blend[t],
                    1.0,
                    m,
                );
                x[ch][t] = y;
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
                    let gain = self.gate_step(p, voice, m);
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
mod identity_tests;
