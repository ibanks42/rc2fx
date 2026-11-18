//! Biquad coefficients of the EQ bands at 48 kHz.
use crate::{Band, BandMode, BandType, CoreError, Result, SAMPLE_RATE};
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
