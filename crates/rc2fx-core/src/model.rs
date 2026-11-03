use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA_VERSION: u32 = 9;
pub const SAMPLE_RATE: f64 = 48_000.0;
pub const MAX_TARGETS: usize = 32;
pub const MAX_BANDS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Mic,
    Playback,
    Output,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Highpass,
    Suppression,
    Eq,
    Compressor,
    Gain,
    Limiter,
}
impl Effect {
    pub const ALL: [Self; 6] = [
        Self::Highpass,
        Self::Suppression,
        Self::Eq,
        Self::Compressor,
        Self::Gain,
        Self::Limiter,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Highpass => "High-pass",
            Self::Suppression => "Noise suppression",
            Self::Eq => "Equalizer",
            Self::Compressor => "Compressor",
            Self::Gain => "Gain",
            Self::Limiter => "Limiter",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BandType {
    Off,
    Bell,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
}
impl BandType {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Bell,
        Self::LowShelf,
        Self::HighShelf,
        Self::LowPass,
        Self::HighPass,
        Self::Notch,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Bell => "Bell",
            Self::LowShelf => "Low shelf",
            Self::HighShelf => "High shelf",
            Self::LowPass => "Low-pass",
            Self::HighPass => "High-pass",
            Self::Notch => "Notch",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BandMode {
    ApoDr,
    RlcBt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub revision: u64,
    pub selected_targets: Vec<String>,
    pub targets: Vec<Target>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub kind: TargetKind,
    pub channels: u8,
    pub bypass: bool,
    pub order: Vec<Effect>,
    pub highpass: Highpass,
    pub suppression: Suppression,
    pub eq: EqSettings,
    pub compressor: Compressor,
    pub gain: Gain,
    pub limiter: Limiter,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Highpass {
    pub enabled: bool,
    pub frequency: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suppression {
    pub enabled: bool,
    pub wet: f64,
    /// DeepFilterNet3's lookahead in hops: 0 (the default, and what settings
    /// saved before this field existed mean) or 1, which adds exactly one hop
    /// of latency.
    #[serde(default)]
    pub lookahead: u8,
    pub gate: VoiceGate,
}
/// Turns the suppressed microphone down while the model hears no voice. It
/// acts only while suppression is enabled, since the voice score comes from
/// the suppression model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceGate {
    /// 0 (off) to 1 (strict); sets every parameter unless `advanced`.
    pub strength: f64,
    /// Use the parameters below instead of the ones `strength` stands for.
    pub advanced: bool,
    /// Voice score (0 to 1) at or above which the gate opens.
    pub threshold: f64,
    /// How far the closed gate turns the signal down, in dB; 0 is off.
    pub reduction: f64,
    /// Opening time constant, in ms.
    pub attack: f64,
    /// How long the gate stays open after the score last reached the
    /// threshold, in ms.
    pub hold: f64,
    /// Closing time constant after the hold, in ms.
    pub release: f64,
}
/// The parameters the gate runs with. Its smoothing runs in dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateShape {
    /// Voice score that opens the gate.
    pub threshold: f64,
    /// Voice score an open gate stays open down to (`threshold` minus
    /// `GATE_HYSTERESIS`), so a score hovering near the threshold, as in soft
    /// consonants, does not flutter it.
    pub close_threshold: f64,
    pub reduction_db: f64,
    pub attack_ms: f64,
    pub hold_ms: f64,
    pub release_ms: f64,
}
pub const GATE_HYSTERESIS: f64 = 0.15;
impl GateShape {
    /// What a strength from 0 (off) through gentle (keeps breaths) to 1
    /// (strict) stands for. Depth grows from nothing, like a blend, reaching
    /// 20 dB at half strength. Strictness comes mostly from the threshold and
    /// depth, so the gate never becomes a hard mute: the residual after
    /// neural suppression is already low, and deeper reduction only makes the
    /// switching audible. Hold and release shrink less; the release moves
    /// geometrically so equal slider steps sound even. Opening is always 5 ms,
    /// inside the model's 10-30 ms lookahead.
    pub fn for_strength(strength: f64) -> Self {
        let s = strength.clamp(0.0, 1.0);
        let linear = |gentle: f64, strict: f64| gentle + (strict - gentle) * s;
        let geometric = |gentle: f64, strict: f64| gentle * (strict / gentle).powf(s);
        Self::new(
            linear(0.3, 0.7),
            30.0 * s.powf(1.5_f64.log2()),
            5.0,
            linear(350.0, 150.0),
            geometric(300.0, 90.0),
        )
    }
    fn new(
        threshold: f64,
        reduction_db: f64,
        attack_ms: f64,
        hold_ms: f64,
        release_ms: f64,
    ) -> Self {
        Self {
            threshold,
            close_threshold: (threshold - GATE_HYSTERESIS).max(0.0),
            reduction_db,
            attack_ms,
            hold_ms,
            release_ms,
        }
    }
}
impl VoiceGate {
    /// A gate at `strength`, its advanced parameters matching it.
    pub fn with_strength(strength: f64) -> Self {
        let mut gate = Self {
            strength,
            advanced: false,
            threshold: 0.0,
            reduction: 0.0,
            attack: 0.0,
            hold: 0.0,
            release: 0.0,
        };
        gate.set_advanced(true);
        gate.advanced = false;
        gate
    }
    /// Whether the gate turns anything down.
    pub fn active(&self) -> bool {
        self.shape().reduction_db > 0.0
    }
    pub fn shape(&self) -> GateShape {
        if self.advanced {
            GateShape::new(
                self.threshold,
                self.reduction,
                self.attack,
                self.hold,
                self.release,
            )
        } else {
            GateShape::for_strength(self.strength)
        }
    }
    /// Switches between the strength slider and the separate parameters.
    /// Entering advanced mode starts the parameters from the current
    /// strength, so the sound does not jump; leaving keeps them for later.
    pub fn set_advanced(&mut self, advanced: bool) {
        if advanced && !self.advanced {
            let shape = GateShape::for_strength(self.strength);
            self.threshold = shape.threshold;
            self.reduction = shape.reduction_db;
            self.attack = shape.attack_ms;
            self.hold = shape.hold_ms;
            self.release = shape.release_ms;
        }
        self.advanced = advanced;
    }
}
impl Suppression {
    /// 0 (lowest latency) or 1 (more context, one hop more).
    pub fn select_lookahead(&mut self, lookahead: u8) {
        self.lookahead = lookahead;
    }
    /// The geometry the saved settings stand for, and hence the latency they
    /// add, without a worker scheduling allowance.
    pub fn geometry(&self) -> Option<SuppressionGeometry> {
        Self::admitted_geometry(self.lookahead)
    }
    fn validate_lookahead(&self) -> Result<()> {
        if self.lookahead > 1 {
            return Err(CoreError("The lookahead must be 0 or 1".into()));
        }
        Ok(())
    }
}
/// DeepFilterNet3's STFT hop, in samples: one lookahead step adds exactly
/// this much latency (5.0 ms at 48 kHz), in both the one- and two-hop builds.
pub const DFN_HOP_SAMPLES: u32 = 240;

/// The lookahead values DeepFilterNet3 admits, lowest latency first.
pub const LOOKAHEADS: [u8; 2] = [0, 1];

/// Model-defined buffering, independent of any worker scheduling allowance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelGeometry {
    pub frame_samples: u32,
    pub intrinsic_delay_samples: u32,
}

/// A resolved processing geometry: the model's buffering plus the worker's
/// scheduling allowance. Factories use `admitted_geometry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuppressionGeometry {
    pub sample_rate: u32,
    pub frame_samples: u32,
    pub intrinsic_delay_samples: u32,
    pub worker_lead_samples: u32,
}
impl SuppressionGeometry {
    pub fn latency_samples(self) -> u32 {
        self.frame_samples + self.intrinsic_delay_samples + self.worker_lead_samples
    }
    /// The same geometry with a different worker scheduling allowance, for
    /// tests and benchmarks: the model's own buffering is unchanged.
    pub fn with_worker_lead(self, worker_lead_samples: u32) -> Self {
        Self {
            worker_lead_samples,
            ..self
        }
    }
}
impl Suppression {
    pub fn model_geometry_with_lookahead(lookahead: u8) -> ModelGeometry {
        ModelGeometry {
            frame_samples: DFN_HOP_SAMPLES,
            intrinsic_delay_samples: 120 + DFN_HOP_SAMPLES * u32::from(lookahead),
        }
    }
    pub fn admitted_geometry(lookahead: u8) -> Option<SuppressionGeometry> {
        if lookahead > 1 {
            return None;
        }
        let model = Self::model_geometry_with_lookahead(lookahead);
        Some(SuppressionGeometry {
            sample_rate: 48_000,
            frame_samples: model.frame_samples,
            intrinsic_delay_samples: model.intrinsic_delay_samples,
            worker_lead_samples: 320,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EqSettings {
    pub enabled: bool,
    pub input_gain_db: f64,
    pub output_gain_db: f64,
    pub bands: Vec<Band>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bands"
    )]
    pub right_bands: Option<Vec<Band>>,
}
fn present_bands<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Option<Vec<Band>>, D::Error> {
    Vec::<Band>::deserialize(decoder).map(Some)
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    pub id: String,
    pub enabled: bool,
    #[serde(rename = "type")]
    pub band_type: BandType,
    pub mode: BandMode,
    pub solo: bool,
    pub frequency: f64,
    pub gain: f64,
    pub q: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compressor {
    pub enabled: bool,
    pub threshold: f64,
    pub ratio: f64,
    pub attack: f64,
    pub release: f64,
    pub makeup: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gain {
    pub enabled: bool,
    pub db: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limiter {
    pub enabled: bool,
    pub ceiling: f64,
    pub release: f64,
}

pub(crate) fn range(value: f64, min: f64, max: f64, name: &str) -> Result<()> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        Err(CoreError(format!("{name} must be within {min}..{max}")))
    } else {
        Ok(())
    }
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
pub fn validate_bands(bands: &[Band]) -> Result<()> {
    if bands.len() > MAX_BANDS {
        return Err(CoreError(
            "At most 32 bands are supported per channel".into(),
        ));
    }
    let mut ids = HashSet::new();
    for band in bands {
        band.validate()?;
        if !ids.insert(&band.id) {
            return Err(CoreError(format!("Duplicate band ID: {}", band.id)));
        }
    }
    Ok(())
}
impl Band {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            band_type: BandType::Bell,
            mode: BandMode::ApoDr,
            solo: false,
            frequency: 1000.0,
            gain: 0.0,
            q: 1.0,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            return Err(CoreError("Invalid band ID".into()));
        }
        range(self.frequency, 10.0, 24000.0, "Frequency")?;
        range(self.gain, -36.0, 36.0, "Band gain")?;
        range(self.q, 0.0, 100.0, "Q")
    }
}
impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            bands: Vec::new(),
            right_bands: None,
        }
    }
}
impl EqSettings {
    pub fn validate(&self, channels: u8) -> Result<()> {
        if !(1..=2).contains(&channels) {
            return Err(CoreError("Channels must be 1 or 2".into()));
        }
        range(self.input_gain_db, -36.0, 36.0, "EQ input gain")?;
        range(self.output_gain_db, -36.0, 36.0, "EQ output gain")?;
        validate_bands(&self.bands)?;
        if let Some(right) = &self.right_bands {
            if channels != 2 {
                return Err(CoreError(
                    "Independent right EQ requires a stereo target".into(),
                ));
            }
            validate_bands(right)?;
        }
        Ok(())
    }
    pub fn channel(&self, side: Channel) -> &[Band] {
        match side {
            Channel::Right => self.right_bands.as_deref().unwrap_or(&self.bands),
            Channel::Left => &self.bands,
        }
    }
    pub fn channel_mut(&mut self, side: Channel) -> &mut Vec<Band> {
        match side {
            Channel::Right if self.right_bands.is_some() => self.right_bands.as_mut().unwrap(),
            _ => &mut self.bands,
        }
    }
    pub fn add_band(&mut self, side: Channel) -> Result<&mut Band> {
        let bands = self.channel_mut(side);
        if bands.len() >= MAX_BANDS {
            return Err(CoreError(
                "At most 32 bands are supported per channel".into(),
            ));
        }
        let next = bands
            .iter()
            .filter_map(|b| b.id.strip_prefix("band-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| CoreError("Band ID space exhausted".into()))?;
        bands.push(Band::new(format!("band-{next}")));
        Ok(bands.last_mut().unwrap())
    }
    pub fn remove_band(&mut self, side: Channel, id: &str) -> bool {
        let b = self.channel_mut(side);
        if let Some(i) = b.iter().position(|b| b.id == id) {
            b.remove(i);
            true
        } else {
            false
        }
    }
}
impl Target {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: TargetKind,
        channels: u8,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            channels,
            bypass: false,
            order: Effect::ALL.to_vec(),
            highpass: Highpass {
                enabled: false,
                frequency: 80.0,
            },
            suppression: Suppression {
                enabled: false,
                wet: 1.0,
                lookahead: 0,
                gate: VoiceGate::with_strength(0.0),
            },
            // Processing is opt-in: a new path passes audio through unchanged
            // until each stage is explicitly enabled.
            eq: EqSettings {
                enabled: false,
                ..EqSettings::default()
            },
            compressor: Compressor {
                enabled: false,
                threshold: -18.0,
                ratio: 3.0,
                attack: 10.0,
                release: 120.0,
                makeup: 0.0,
            },
            gain: Gain {
                enabled: false,
                db: 0.0,
            },
            limiter: Limiter {
                enabled: false,
                ceiling: -1.0,
                release: 100.0,
            },
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.suppression.validate_lookahead()?;
        if self.suppression.geometry().is_none() {
            return Err(CoreError(format!(
                "Lookahead {} is not admitted for saved settings",
                self.suppression.lookahead
            )));
        }
        self.validate_fields()
    }
    /// Field validation shared by every settings entry point.
    pub(crate) fn validate_fields(&self) -> Result<()> {
        if !valid_id(&self.id)
            || self.name.is_empty()
            || self.name.len() > 128
            || !(1..=2).contains(&self.channels)
        {
            return Err(CoreError(format!("Invalid target identity: {}", self.id)));
        }
        if self.order.len() != 6 || self.order.iter().collect::<HashSet<_>>().len() != 6 {
            return Err(CoreError(
                "Effects must contain each of the six stages once".into(),
            ));
        }
        self.eq.validate(self.channels)?;
        if self.suppression.enabled && !self.supports_suppression() {
            return Err(CoreError(
                "Noise suppression is supported on a mono microphone or the Chat voice mix only"
                    .into(),
            ));
        }
        range(self.highpass.frequency, 20.0, 1000.0, "High-pass frequency")?;
        range(self.suppression.wet, 0.0, 1.0, "Suppression blend")?;
        let gate = &self.suppression.gate;
        range(gate.strength, 0.0, 1.0, "Voice gate strength")?;
        range(gate.threshold, 0.0, 1.0, "Voice gate threshold")?;
        range(gate.reduction, 0.0, 60.0, "Voice gate reduction")?;
        range(gate.attack, 0.5, 50.0, "Voice gate attack")?;
        range(gate.hold, 0.0, 2000.0, "Voice gate hold")?;
        range(gate.release, 30.0, 2000.0, "Voice gate release")?;
        range(
            self.compressor.threshold,
            -60.0,
            0.0,
            "Compressor threshold",
        )?;
        range(self.compressor.ratio, 1.0, 20.0, "Compressor ratio")?;
        range(self.compressor.attack, 0.1, 200.0, "Compressor attack")?;
        range(self.compressor.release, 10.0, 2000.0, "Compressor release")?;
        range(self.compressor.makeup, 0.0, 24.0, "Compressor makeup")?;
        range(self.gain.db, -60.0, 12.0, "Gain")?;
        range(self.limiter.ceiling, -24.0, 0.0, "Limiter ceiling")?;
        range(self.limiter.release, 10.0, 2000.0, "Limiter release")
    }
    /// Chat suppression acts on a separate mono microphone stem, never on the
    /// completed stereo mix (which also carries pads and playback).
    pub fn is_chat_output(&self) -> bool {
        self.kind == TargetKind::Output
            && self.channels == 2
            && matches!(self.id.as_str(), "send-chat" | "duo-send-chat")
    }
    pub fn supports_suppression(&self) -> bool {
        (self.kind == TargetKind::Mic && self.channels == 1) || self.is_chat_output()
    }
    pub fn effect_enabled(&self, effect: Effect) -> bool {
        match effect {
            Effect::Highpass => self.highpass.enabled,
            Effect::Suppression => self.suppression.enabled,
            Effect::Eq => self.eq.enabled,
            Effect::Compressor => self.compressor.enabled,
            Effect::Gain => self.gain.enabled,
            Effect::Limiter => self.limiter.enabled,
        }
    }
    pub fn set_effect_enabled(&mut self, effect: Effect, enabled: bool) {
        match effect {
            Effect::Highpass => self.highpass.enabled = enabled,
            Effect::Suppression => self.suppression.enabled = enabled,
            Effect::Eq => self.eq.enabled = enabled,
            Effect::Compressor => self.compressor.enabled = enabled,
            Effect::Gain => self.gain.enabled = enabled,
            Effect::Limiter => self.limiter.enabled = enabled,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        self.validate_with(Target::validate)
    }
    pub(crate) fn validate_with(
        &self,
        validate_target: impl Fn(&Target) -> Result<()>,
    ) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION || self.revision == 0 {
            return Err(CoreError(
                "Expected schema version 6 and a positive revision".into(),
            ));
        }
        if self.targets.is_empty() || self.targets.len() > MAX_TARGETS {
            return Err(CoreError("Expected 1 to 24 targets".into()));
        }
        if self
            .targets
            .iter()
            .filter(|target| target.suppression.enabled)
            .count()
            > 1
        {
            return Err(CoreError(
                "Noise suppression can be enabled on only one microphone or the Chat voice mix at a time; turn it off on the other target first".into(),
            ));
        }
        let mut ids = HashSet::new();
        for target in &self.targets {
            validate_target(target)?;
            if !ids.insert(&target.id) {
                return Err(CoreError("Duplicate target ID".into()));
            }
        }
        self.validate_selection(&self.selected_targets)
    }
    /// Selection is distinct from saved effect settings. Physical eligibility
    /// is supplied by the connected controller, not inferred from this catalog.
    pub fn validate_selection(&self, selected: &[String]) -> Result<()> {
        let mut ids = HashSet::new();
        for id in selected {
            self.targets
                .iter()
                .find(|t| &t.id == id)
                .ok_or_else(|| CoreError(format!("Unknown processing path: {id}")))?;
            if !ids.insert(id) {
                return Err(CoreError(format!("Duplicate processing path: {id}")));
            }
        }
        Ok(())
    }
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected_targets.iter().any(|selected| selected == id)
    }
    pub fn selected_channels(&self) -> u32 {
        self.targets
            .iter()
            .filter(|t| self.is_selected(&t.id))
            .map(|t| u32::from(t.channels))
            .sum()
    }
    pub fn canonical_selection(&self, selected: &[String]) -> Result<Vec<String>> {
        self.validate_selection(selected)?;
        Ok(self
            .targets
            .iter()
            .filter(|t| selected.contains(&t.id))
            .map(|t| t.id.clone())
            .collect())
    }
    /// Strict parsing of saved settings in the current schema.
    pub fn from_json(text: &str) -> Result<Self> {
        let value = crate::strict_json::parse(text, 512 * 1024)?;
        let config: Self = serde_json::from_value(value)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate_update(&self, targets: &[Target]) -> Result<()> {
        if targets.len() != self.targets.len()
            || targets
                .iter()
                .any(|t| !self.targets.iter().any(|old| old.id == t.id))
        {
            return Err(CoreError(
                "Effects updates must retain the complete saved target catalog".into(),
            ));
        }
        Self {
            targets: targets.to_vec(),
            ..self.clone()
        }
        .validate()?;
        for target in targets {
            if let Some(old) = self.targets.iter().find(|x| x.id == target.id)
                && (old.kind != target.kind || old.channels != target.channels)
            {
                return Err(CoreError(format!(
                    "Kind and channels cannot change for {}",
                    target.id
                )));
            }
        }
        Ok(())
    }
}
impl Default for Config {
    fn default() -> Self {
        let mut targets = Vec::new();
        // This existing saved chain owns physical combo input 4. Do not move
        // its effects to input 1 merely because its durable ID says mic-1.
        targets.push(Target::new("mic-1", "Mic 4", TargetKind::Mic, 1));
        for i in 1..=3 {
            targets.push(Target::new(
                format!("analog-{i}"),
                format!("Mic {i}"),
                TargetKind::Mic,
                1,
            ));
        }
        for name in ["main", "chat", "secondary", "game", "music", "a", "b"] {
            targets.push(Target::new(
                format!("usb-{name}"),
                format!("USB {name}"),
                TargetKind::Playback,
                2,
            ));
        }
        for i in 1..=4 {
            targets.push(Target::new(
                format!("headphones-{i}"),
                format!("Headphones {i}"),
                TargetKind::Output,
                2,
            ));
        }
        for (id, name) in [
            ("monitors", "Monitors"),
            ("send-main", "Main Mix Out"),
            ("send-chat", "Chat Mix Out"),
            ("send-secondary", "Secondary Mix Out"),
        ] {
            targets.push(Target::new(id, name, TargetKind::Output, 2));
        }
        for i in 1..=3 {
            targets.push(Target::new(
                format!("callme-{i}"),
                format!("CallMe {i}"),
                TargetKind::Playback,
                2,
            ));
        }
        for i in 1..=2 {
            targets.push(Target::new(
                format!("wireless-{i}"),
                format!("Wireless {i}"),
                TargetKind::Mic,
                1,
            ));
        }
        targets.push(Target::new(
            "bluetooth",
            "Bluetooth",
            TargetKind::Playback,
            2,
        ));
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            selected_targets: Vec::new(),
            targets,
        }
    }
}
