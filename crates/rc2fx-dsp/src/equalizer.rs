#[cfg(test)]
use crate::filter::safe;
use crate::{
    BLOCK, EQ_INTERNAL_LIMIT, GAIN_SMOOTH, Meter, Prepared, Ramp, clamp,
    filter::{Filter, Filters},
};
use rc2fx_core::{BandMode, BandType, Channel, MAX_BANDS, Target, eq::Coefficients};

#[derive(Clone, Copy, PartialEq, Eq)]
struct BandShape {
    id: [u8; 48],
    length: u8,
    enabled: bool,
    solo: bool,
    kind: BandType,
    mode: BandMode,
}
impl Default for BandShape {
    fn default() -> Self {
        Self {
            id: [0; 48],
            length: 0,
            enabled: false,
            solo: false,
            kind: BandType::Off,
            mode: BandMode::ApoDr,
        }
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EqShape {
    on: bool,
    right_present: bool,
    count: [usize; 2],
    bands: [[BandShape; MAX_BANDS]; 2],
}
impl EqShape {
    pub fn same_as(&self, other: &Self) -> bool {
        self.on == other.on
            && self.right_present == other.right_present
            && self.count == other.count
            && (0..2)
                .all(|ch| self.bands[ch][..self.count[ch]] == other.bands[ch][..other.count[ch]])
    }
    pub fn new(t: &Target) -> Self {
        let mut s = Self {
            on: t.eq.enabled,
            right_present: t.eq.right_bands.is_some(),
            ..Self::default()
        };
        for ch in 0..t.channels as usize {
            let bands = t.eq.channel(if ch == 0 {
                Channel::Left
            } else {
                Channel::Right
            });
            s.count[ch] = bands.len();
            for (i, b) in bands.iter().enumerate() {
                let shape = &mut s.bands[ch][i];
                shape.id[..b.id.len()].copy_from_slice(b.id.as_bytes());
                shape.length = b.id.len() as u8;
                shape.enabled = b.enabled;
                shape.solo = b.solo;
                shape.kind = b.band_type;
                shape.mode = b.mode;
            }
        }
        s
    }
}
/// `safe(sample * gain, limit)` for every sample, bit for bit. A gain of
/// exactly 1 leaves a finite sample unchanged, and `clamp` turns every
/// infinity and NaN into what `safe` returns for it either way, so that
/// multiply is skipped.
#[inline(always)]
fn scale(samples: &mut [f64], gain: &Ramp, limit: f64, m: &mut Meter) {
    if !gain.is_one() {
        gain.each(samples.len(), |t, gain| samples[t] *= gain);
    }
    clamp(samples, limit, m);
}
#[derive(Clone, Copy)]
pub(crate) struct EqBank {
    pub shape: EqShape,
    active: [[bool; MAX_BANDS]; 2],
    filters: Filters,
    target: [[Coefficients; MAX_BANDS]; 2],
    input_gain: f64,
    output_gain: f64,
    input_target: f64,
    output_target: f64,
}
impl Default for EqBank {
    fn default() -> Self {
        Self {
            shape: EqShape::default(),
            active: [[false; MAX_BANDS]; 2],
            filters: Filters::default(),
            target: [[Coefficients::IDENTITY; MAX_BANDS]; 2],
            input_gain: 0.0,
            output_gain: 0.0,
            input_target: 0.0,
            output_target: 0.0,
        }
    }
}
impl EqBank {
    pub fn steady_disabled(&self) -> bool {
        !self.shape.on
            && (self.input_gain + GAIN_SMOOTH * (self.input_target - self.input_gain)).to_bits()
                == self.input_gain.to_bits()
            && (self.output_gain + GAIN_SMOOTH * (self.output_target - self.output_gain)).to_bits()
                == self.output_gain.to_bits()
    }
    pub fn new(p: &Prepared, old: Option<&Self>) -> Self {
        let mut s = Self {
            shape: p.eq_shape,
            active: p.eq_active,
            target: p.eq,
            input_gain: old.map_or(p.eq_input_gain, |e| e.input_gain),
            output_gain: old.map_or(p.eq_output_gain, |e| e.output_gain),
            input_target: p.eq_input_gain,
            output_target: p.eq_output_gain,
            ..Self::default()
        };
        for ch in 0..p.channels() {
            for i in 0..s.shape.count[ch] {
                s.filters.set(ch, i, Filter::with_coefficients(p.eq[ch][i]));
                if let Some(old) = old.filter(|old| old.shape.on && s.shape.on && s.active[ch][i]) {
                    let b = s.shape.bands[ch][i];
                    for j in 0..old.shape.count[ch] {
                        let a = old.shape.bands[ch][j];
                        if old.active[ch][j]
                            && a.id == b.id
                            && a.length == b.length
                            && a.kind == b.kind
                            && a.mode == b.mode
                        {
                            s.filters.set(ch, i, old.filters.get(ch, j));
                            break;
                        }
                    }
                }
            }
        }
        s
    }
    pub fn update_targets(&mut self, p: &Prepared) {
        self.input_target = p.eq_input_gain;
        self.output_target = p.eq_output_gain;
        // Disabled banks do not read coefficients. Re-enabling changes shape
        // and constructs a bank from the complete new Prepared value, so keep
        // gain smoothing but avoid copying every saved inactive band per block.
        if !self.shape.on {
            return;
        }
        for ch in 0..p.channels() {
            let count = self.shape.count[ch];
            self.target[ch][..count].copy_from_slice(&p.eq[ch][..count]);
        }
    }
    /// Whether any filter this bank runs on `ch` is smoothing its
    /// coefficients. A disabled bank runs none.
    pub fn is_smoothing(&self, ch: usize) -> bool {
        self.shape.on
            && (0..self.shape.count[ch])
                .any(|i| self.active[ch][i] && self.filters.is_smoothing(ch, i))
    }
    pub fn begin(&mut self, ch: usize) {
        if self.shape.on {
            for i in 0..self.shape.count[ch] {
                if self.active[ch][i] {
                    let mut filter = self.filters.get(ch, i);
                    filter.begin(self.target[ch][i]);
                    self.filters.set(ch, i, filter);
                }
            }
        }
    }
    /// `process` for `n` samples of each channel, band by band.
    pub fn process_block(
        &mut self,
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        channels: usize,
        m: &mut Meter,
    ) {
        let input_gain = Ramp::advance(&mut self.input_gain, self.input_target, n);
        let output_gain = Ramp::advance(&mut self.output_gain, self.output_target, n);
        if !self.shape.on {
            return;
        }
        let [left, right] = self.shape.count;
        self.filters.prefetch(&self.active, left.max(right), x);
        for samples in x.iter_mut().take(channels) {
            scale(&mut samples[..n], &input_gain, EQ_INTERNAL_LIMIT, m);
        }
        let stereo_done = channels == 2
            && self.filters.try_stereo_cascade(
                &self.active,
                self.shape.count,
                x,
                n,
                EQ_INTERNAL_LIMIT,
            );
        if !stereo_done {
            for (ch, samples) in x.iter_mut().enumerate().take(channels) {
                let count = self.shape.count[ch];
                self.filters.cascade(
                    ch,
                    &self.active[ch][..count],
                    &self.target[ch][..count],
                    &mut samples[..n],
                    EQ_INTERNAL_LIMIT,
                    m,
                );
            }
        }
        for samples in x.iter_mut().take(channels) {
            scale(&mut samples[..n], &output_gain, 16.0, m);
        }
    }
    pub fn is_on(&self) -> bool {
        self.shape.on
    }
    /// The gains, every filter, every coefficient target, the active bands
    /// and the size of the shape as bits, for comparing two processing paths.
    /// It covers everything `Dsp::begin_block` can write to a bank except the
    /// band identities in the shape, which `EqShape::same_as` compares.
    #[cfg(test)]
    pub fn bits(&self) -> Vec<u64> {
        [
            self.input_gain,
            self.output_gain,
            self.input_target,
            self.output_target,
        ]
        .iter()
        .map(|gain| gain.to_bits())
        .chain(
            (0..2 * MAX_BANDS).flat_map(|k| self.filters.get(k / MAX_BANDS, k % MAX_BANDS).bits()),
        )
        .chain(
            self.target
                .iter()
                .flatten()
                .flat_map(|target| target.as_array())
                .map(f64::to_bits),
        )
        .chain(self.active.iter().flatten().map(|on| u64::from(*on)))
        .chain([
            u64::from(self.shape.on),
            u64::from(self.shape.right_present),
            self.shape.count[0] as u64,
            self.shape.count[1] as u64,
        ])
        .collect()
    }
    /// How many of the filters this bank runs on `ch` have stopped short of
    /// their target: `begin` found them not smoothing, yet their coefficients
    /// are not the target's.
    #[cfg(test)]
    pub fn stalled(&self, ch: usize) -> usize {
        if !self.shape.on {
            return 0;
        }
        (0..self.shape.count[ch])
            .filter(|&i| self.active[ch][i] && self.filters.get(ch, i).stalled(self.target[ch][i]))
            .count()
    }
    #[cfg(test)]
    pub fn process(&mut self, x: &mut [f64; 2], channels: usize, m: &mut Meter) {
        self.input_gain += GAIN_SMOOTH * (self.input_target - self.input_gain);
        self.output_gain += GAIN_SMOOTH * (self.output_target - self.output_gain);
        if !self.shape.on {
            return;
        }
        for (ch, sample) in x.iter_mut().enumerate().take(channels) {
            *sample = safe(*sample * self.input_gain, EQ_INTERNAL_LIMIT, m);
            for i in 0..self.shape.count[ch] {
                if self.active[ch][i] {
                    let mut filter = self.filters.get(ch, i);
                    *sample = filter.process(&self.target[ch][i], *sample, EQ_INTERNAL_LIMIT, m);
                    self.filters.set(ch, i, filter);
                }
            }
            *sample = safe(*sample * self.output_gain, 16.0, m);
        }
    }
}
