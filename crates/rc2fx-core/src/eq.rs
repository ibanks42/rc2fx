//! Theoretical filter response, not an FFT or an audio processor.
use crate::{Band, BandMode, BandType, Channel, CoreError, EqSettings, Result, SAMPLE_RATE};
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}
impl Coefficients {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
    pub fn as_array(self) -> [f64; 5] {
        [self.b0, self.b1, self.b2, self.a1, self.a2]
    }
}
pub fn design_q(band: &Band) -> f64 {
    match (band.mode, band.band_type) {
        (BandMode::RlcBt, BandType::Bell) => {
            let g = 10.0_f64.powf(band.gain / 20.0);
            (1.0 + 2.0 * band.q) * g.sqrt() / (2.0 * (1.0 + g * g).sqrt())
        }
        (BandMode::RlcBt, _) => (1.0 + band.q) / 2.0,
        _ => band.q.max(0.1),
    }
}
pub fn coefficients(band: &Band) -> Result<Coefficients> {
    band.validate()?;
    if band.band_type == BandType::Off {
        return Ok(Coefficients::IDENTITY);
    }
    let w = 2.0 * PI * band.frequency.min(0.49 * SAMPLE_RATE) / SAMPLE_RATE;
    let c = w.cos();
    let alpha = w.sin() / (2.0 * design_q(band));
    let a = 10.0_f64.powf(band.gain / 40.0);
    let beta = 2.0 * a.sqrt() * alpha;
    let (n, d) = match band.band_type {
        BandType::Bell => (
            [1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a],
        ),
        BandType::LowPass => (
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
            [1.0 + alpha, -2.0 * c, 1.0 - alpha],
        ),
        BandType::HighPass => (
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + alpha, -2.0 * c, 1.0 - alpha],
        ),
        BandType::Notch => ([1.0, -2.0 * c, 1.0], [1.0 + alpha, -2.0 * c, 1.0 - alpha]),
        BandType::LowShelf => (
            [
                a * ((a + 1.0) - (a - 1.0) * c + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - beta),
            ],
            [
                (a + 1.0) + (a - 1.0) * c + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - beta,
            ],
        ),
        BandType::HighShelf => (
            [
                a * ((a + 1.0) + (a - 1.0) * c + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - beta),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - beta,
            ],
        ),
        BandType::Off => unreachable!(),
    };
    let value = Coefficients {
        b0: n[0] / d[0],
        b1: n[1] / d[0],
        b2: n[2] / d[0],
        a1: d[1] / d[0],
        a2: d[2] / d[0],
    };
    if value.as_array().iter().all(|v| v.is_finite()) {
        Ok(value)
    } else {
        Err(CoreError("Invalid filter coefficients".into()))
    }
}
pub fn magnitude_db(c: Coefficients, frequency: f64) -> f64 {
    let w = 2.0 * PI * frequency / SAMPLE_RATE;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    let nr = c.b0 + c.b1 * c1 + c.b2 * c2;
    let ni = c.b1 * s1 + c.b2 * s2;
    let dr = 1.0 + c.a1 * c1 + c.a2 * c2;
    let di = c.a1 * s1 + c.a2 * s2;
    let power = (nr * nr + ni * ni) / (dr * dr + di * di);
    if power.is_finite() && power > 1e-30 {
        10.0 * power.log10()
    } else {
        -300.0
    }
}
pub fn checked_response_db(
    eq: &EqSettings,
    channel: Channel,
    frequency: f64,
    include_gains: bool,
) -> Result<f64> {
    eq.validate(if eq.right_bands.is_some() { 2 } else { 1 })?;
    crate::model::range(frequency, 0.0, SAMPLE_RATE / 2.0, "Response frequency")?;
    if !eq.enabled {
        return Ok(0.0);
    }
    let bands = eq.channel(channel);
    let solo = bands.iter().any(|b| b.solo);
    let mut value = if include_gains {
        eq.input_gain_db + eq.output_gain_db
    } else {
        0.0
    };
    for band in bands
        .iter()
        .filter(|b| b.enabled && b.band_type != BandType::Off && (!solo || b.solo))
    {
        value += magnitude_db(coefficients(band)?, frequency);
    }
    Ok(value)
}
/// Convenience for a validated editor draft; invalid/transient edits plot flat.
pub fn response_db(eq: &EqSettings, channel: Channel, frequency: f64, include_gains: bool) -> f64 {
    checked_response_db(eq, channel, frequency, include_gains).unwrap_or(0.0)
}
/// [`response_db`] at many frequencies, validating the EQ and designing each
/// band once rather than once per point. Values match per-point calls.
pub fn response_curve(
    eq: &EqSettings,
    channel: Channel,
    frequencies: &[f64],
    include_gains: bool,
) -> Vec<f64> {
    let flat = || vec![0.0; frequencies.len()];
    if eq
        .validate(if eq.right_bands.is_some() { 2 } else { 1 })
        .is_err()
        || !eq.enabled
    {
        return flat();
    }
    let bands = eq.channel(channel);
    let solo = bands.iter().any(|b| b.solo);
    let Ok(designed) = bands
        .iter()
        .filter(|b| b.enabled && b.band_type != BandType::Off && (!solo || b.solo))
        .map(coefficients)
        .collect::<Result<Vec<_>>>()
    else {
        return flat();
    };
    let offset = if include_gains {
        eq.input_gain_db + eq.output_gain_db
    } else {
        0.0
    };
    frequencies
        .iter()
        .map(|&frequency| {
            if crate::model::range(frequency, 0.0, SAMPLE_RATE / 2.0, "Response frequency").is_err()
            {
                return 0.0;
            }
            designed
                .iter()
                .fold(offset, |value, c| value + magnitude_db(*c, frequency))
        })
        .collect()
}
pub fn log_frequencies(count: usize, min: f64, max: f64) -> Vec<f64> {
    if count == 0
        || count > 65_536
        || !min.is_finite()
        || !max.is_finite()
        || min <= 0.0
        || max < min
    {
        return Vec::new();
    }
    if count == 1 {
        return vec![min];
    }
    (0..count)
        .map(|i| min * (max / min).powf(i as f64 / (count - 1) as f64))
        .collect()
}
