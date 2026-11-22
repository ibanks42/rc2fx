//! DeepFilterNet STFT framing and feature extraction, ported from libDF 0.5.6
//! (`DFState`) with all scratch buffers preallocated. Computes the windowed
//! STFT, dB ERB band energies, and low-frequency complex bins with exponential
//! running normalization. Framing and band counts are read from the weights file.
//!
//! Analysis and synthesis windows can be asymmetric: analysis spans the full
//! FFT (looking further into the past adds no latency), while only the last
//! `synthesis` samples are overlap-added at each hop, giving `synthesis - hop`
//! samples of latency. When `synthesis == fft == 2 * hop`, both windows reduce
//! to libDF's symmetric Vorbis window.
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub(crate) const SAMPLE_RATE: usize = 48_000;
const MIN_BAND_BINS: usize = 2;
/// Initial endpoints (low to high band) for the running normalizers.
const MEAN_NORM_INIT: [f32; 2] = [-60., -90.];
const UNIT_NORM_INIT: [f32; 2] = [0.001, 0.0001];
const NORM_TAU: f32 = 1.;

pub(crate) struct Dsp {
    fft: usize,
    synthesis: usize,
    hop: usize,
    bins: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    analysis_window: Vec<f32>,
    synthesis_window: Vec<f32>,
    wnorm: f32,
    /// STFT bin count per ERB band.
    erb: Vec<usize>,
    alpha: f32,
    analysis_mem: Vec<f32>,
    /// `synthesis - hop` overlap tail carried into the next hop.
    synthesis_mem: Vec<f32>,
    time: Vec<f32>,
    scratch: Vec<Complex32>,
    mean_norm: Vec<f32>,
    unit_norm: Vec<f32>,
}

/// Checks that the framing is valid: even FFT, `hop <= synthesis <= 2 * hop`
/// (at most two overlapping frames), and `fft >= synthesis`.
pub(crate) fn framing_is_valid(fft: usize, synthesis: usize, hop: usize) -> bool {
    hop > 0 && fft.is_multiple_of(2) && synthesis >= hop && synthesis <= 2 * hop && fft >= synthesis
}

/// Rising half of libDF's Vorbis window (`sin(π/2 · sin²(π/2 · (i + ½) / len))`).
/// Power-complementary with its reverse: `rise(i)² + rise(len - 1 - i)² = 1`.
fn vorbis_rise(len: usize) -> impl Fn(usize) -> f64 {
    move |i| {
        let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / len as f64).sin();
        (0.5 * std::f64::consts::PI * s * s).sin()
    }
}

/// Builds the analysis and synthesis windows for a valid framing.
///
/// Over the last `synthesis` samples, `analysis * synthesis` forms a flat-top
/// window with squared-Vorbis tapers of length `synthesis - hop`, so hop-spaced
/// copies sum to 1 (COLA). Before the falling taper, the analysis window rises
/// smoothly over the rest of the FFT without adding latency. When
/// `synthesis == fft == 2 * hop`, both windows are the standard Vorbis window.
pub(crate) fn windows(fft: usize, synthesis: usize, hop: usize) -> (Vec<f32>, Vec<f32>) {
    debug_assert!(framing_is_valid(fft, synthesis, hop));
    let overlap = synthesis - hop;
    let taper = vorbis_rise(overlap);
    let rise = vorbis_rise(fft - overlap);
    let analysis: Vec<f64> = (0..fft)
        .map(|i| {
            if i < fft - overlap {
                rise(i)
            } else {
                taper(fft - 1 - i)
            }
        })
        .collect();
    let synthesis_window = (0..synthesis)
        .map(|j| {
            let product = if j < overlap {
                let t = taper(j);
                t * t
            } else if j < hop {
                1.
            } else {
                let t = taper(synthesis - 1 - j);
                t * t
            };
            (product / analysis[fft - synthesis + j]) as f32
        })
        .collect();
    (
        analysis.into_iter().map(|w| w as f32).collect(),
        synthesis_window,
    )
}

impl Dsp {
    pub(crate) fn new(
        fft: usize,
        synthesis: usize,
        hop: usize,
        nb_erb: usize,
        nb_df: usize,
    ) -> Self {
        assert!(
            framing_is_valid(fft, synthesis, hop),
            "invalid framing: fft {fft}, synthesis {synthesis}, hop {hop}"
        );
        let bins = fft / 2 + 1;
        assert!(nb_erb > 0 && nb_df > 0 && nb_df <= bins);
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft);
        let inverse = planner.plan_fft_inverse(fft);
        let scratch_len = forward.get_scratch_len().max(inverse.get_scratch_len());
        let (analysis_window, synthesis_window) = windows(fft, synthesis, hop);
        let mut dsp = Self {
            fft,
            synthesis,
            hop,
            bins,
            forward,
            inverse,
            analysis_window,
            synthesis_window,
            // Compensate for the unnormalized inverse FFT (1/fft) so the
            // analysis*synthesis window product alone determines unity gain.
            wnorm: 1. / fft as f32,
            erb: erb_bands(nb_erb, bins),
            alpha: norm_alpha(hop),
            analysis_mem: vec![0.; fft - hop],
            synthesis_mem: vec![0.; synthesis - hop],
            time: vec![0.; fft],
            scratch: vec![Complex32::default(); scratch_len],
            mean_norm: vec![0.; nb_erb],
            unit_norm: vec![0.; nb_df],
        };
        dsp.reset();
        dsp
    }

    pub(crate) fn reset(&mut self) {
        self.analysis_mem.fill(0.);
        self.synthesis_mem.fill(0.);
        let ramp = |out: &mut [f32], [a, b]: [f32; 2]| {
            let step = (b - a) / (out.len().max(2) - 1) as f32;
            for (i, v) in out.iter_mut().enumerate() {
                *v = a + i as f32 * step;
            }
        };
        ramp(&mut self.mean_norm, MEAN_NORM_INIT);
        ramp(&mut self.unit_norm, UNIT_NORM_INIT);
    }

    /// Forward STFT of one hop, scaled by `wnorm`.
    pub(crate) fn analysis(&mut self, input: &[f32], spec: &mut [Complex32]) {
        let (fft, hop) = (self.fft, self.hop);
        debug_assert_eq!(input.len(), hop);
        debug_assert_eq!(spec.len(), self.bins);
        let keep = fft - hop;
        for ((x, &m), &w) in self.time[..keep]
            .iter_mut()
            .zip(&self.analysis_mem)
            .zip(&self.analysis_window[..keep])
        {
            *x = m * w;
        }
        for ((x, &s), &w) in self.time[keep..]
            .iter_mut()
            .zip(input)
            .zip(&self.analysis_window[keep..])
        {
            *x = s * w;
        }
        let split = keep - hop;
        self.analysis_mem.rotate_left(hop);
        self.analysis_mem[split..].copy_from_slice(input);
        let _ = self
            .forward
            .process_with_scratch(&mut self.time, spec, &mut self.scratch);
        for x in spec.iter_mut() {
            *x *= self.wnorm;
        }
    }

    /// Inverse STFT of one spectrum into a hop of output via overlap-add on the
    /// last `synthesis` samples.
    pub(crate) fn synthesis(&mut self, spec: &mut [Complex32], output: &mut [f32]) {
        let (fft, synthesis, hop) = (self.fft, self.synthesis, self.hop);
        debug_assert_eq!(output.len(), hop);
        // Zero imaginary DC/Nyquist for realfft (matches libDF).
        spec[0].im = 0.;
        spec[self.bins - 1].im = 0.;
        let _ = self
            .inverse
            .process_with_scratch(spec, &mut self.time, &mut self.scratch);
        let frame = &mut self.time[fft - synthesis..];
        for (x, &w) in frame.iter_mut().zip(&self.synthesis_window) {
            *x *= w;
        }
        // Emit the first hop (adding the carried overlap tail) and save the remainder.
        output.copy_from_slice(&frame[..hop]);
        for (o, &m) in output.iter_mut().zip(&self.synthesis_mem) {
            *o += m;
        }
        self.synthesis_mem.copy_from_slice(&frame[hop..]);
    }

    /// Mean-normalized ERB band energies in dB, scaled by 1/40.
    pub(crate) fn feat_erb(&mut self, spec: &[Complex32], out: &mut [f32]) {
        debug_assert_eq!(out.len(), self.erb.len());
        let mut start = 0;
        for ((o, &width), s) in out.iter_mut().zip(&self.erb).zip(&mut self.mean_norm) {
            let k = 1. / width as f32;
            let energy: f32 = spec[start..start + width]
                .iter()
                .map(|x| (x.re * x.re + x.im * x.im) * k)
                .sum();
            start += width;
            let db = (energy + 1e-10).log10() * 10.;
            *s = db * (1. - self.alpha) + *s * self.alpha;
            *o = (db - *s) / 40.;
        }
    }

    /// Unit-normalized complex features for the lowest `nb_df` bins.
    pub(crate) fn feat_cplx(&mut self, spec: &[Complex32], re: &mut [f32], im: &mut [f32]) {
        debug_assert_eq!(spec.len(), self.unit_norm.len());
        for (((x, s), r), i) in spec.iter().zip(&mut self.unit_norm).zip(re).zip(im) {
            *s = x.norm() * (1. - self.alpha) + *s * self.alpha;
            let y = *x / s.sqrt();
            (*r, *i) = (y.re, y.im);
        }
    }

    pub(crate) fn apply_gains(&self, spec: &mut [Complex32], gains: &[f32]) {
        let mut start = 0;
        for (&width, &g) in self.erb.iter().zip(gains) {
            for x in &mut spec[start..start + width] {
                *x *= g;
            }
            start += width;
        }
    }
}

fn freq2erb(hz: f32) -> f32 {
    9.265 * (hz / (24.7 * 9.265)).ln_1p()
}

fn erb2freq(erb: f32) -> f32 {
    24.7 * 9.265 * ((erb / 9.265).exp() - 1.)
}

/// Computes STFT bin widths for `nb_erb` ERB bands over `bins` at 48 kHz
/// (matching libDF's `erb_fb`).
fn erb_bands(nb_erb: usize, bins: usize) -> Vec<usize> {
    let fft = (bins - 1) * 2;
    let width = SAMPLE_RATE as f32 / fft as f32;
    let (low, high) = (freq2erb(0.), freq2erb((SAMPLE_RATE / 2) as f32));
    let step = (high - low) / nb_erb as f32;
    let mut erb = vec![0; nb_erb];
    let (mut prev, mut over) = (0i32, 0i32);
    for (i, band) in erb.iter_mut().enumerate() {
        let bin = (erb2freq(low + (i + 1) as f32 * step) / width).round() as i32;
        let mut n = bin - prev - over;
        if n < MIN_BAND_BINS as i32 {
            over = MIN_BAND_BINS as i32 - n;
            n = MIN_BAND_BINS as i32;
        } else {
            over = 0;
        }
        *band = n as usize;
        prev = bin;
    }
    let last = nb_erb - 1;
    erb[last] += 1;
    let total: usize = erb.iter().sum();
    erb[last] -= total.saturating_sub(bins);
    erb
}

/// Computes `exp(-hop / (rate * tau))` rounded to the fewest decimals below 1
/// (matching libDF's `calc_norm_alpha`).
fn norm_alpha(hop: usize) -> f32 {
    let alpha = (-(hop as f32 / SAMPLE_RATE as f32) / NORM_TAU).exp();
    let mut precision = 3;
    loop {
        let scale = 10i32.pow(precision) as f32;
        let a = (alpha * scale).round() / scale;
        if a < 1. {
            return a;
        }
        precision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erb_bands_cover_every_bin() {
        let erb = erb_bands(32, 481);
        assert_eq!(erb.iter().sum::<usize>(), 481);
        assert!(erb.iter().all(|&n| n >= MIN_BAND_BINS));
        // libDF 0.5.6's bands at 48 kHz.
        assert_eq!(
            erb,
            [
                2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 5, 5, 7, 7, 8, 10, 12, 13, 15, 18, 20, 24,
                28, 31, 37, 42, 50, 56, 67
            ]
        );
    }

    #[test]
    fn erb_bands_cover_every_bin_of_a_480_point_fft() {
        let erb = erb_bands(32, 241);
        assert_eq!(erb.len(), 32);
        assert_eq!(erb.iter().sum::<usize>(), 241);
        assert!(erb.iter().all(|&n| n >= MIN_BAND_BINS), "{erb:?}");
    }

    #[test]
    fn norm_alpha_follows_the_hop() {
        assert_eq!(norm_alpha(480), 0.99);
        assert_eq!(norm_alpha(240), 0.995);
    }

    // Analysis followed by synthesis should reproduce the input delayed by
    // `synthesis - hop` samples once the first full FFT has filled.
    fn reconstructs(fft: usize, synthesis: usize, hop: usize) {
        let bins = fft / 2 + 1;
        let mut dsp = Dsp::new(fft, synthesis, hop, 32, bins / 5);
        let mut seed = 0x9e37_79b9u32;
        let input: Vec<f32> = (0..fft * 4)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (i as f32 * 0.013).sin() * 0.5 + (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect();
        let mut out = vec![0.; input.len()];
        let mut spec = vec![Complex32::default(); bins];
        for (x, y) in input.chunks_exact(hop).zip(out.chunks_exact_mut(hop)) {
            dsp.analysis(x, &mut spec);
            dsp.synthesis(&mut spec, y);
        }
        let delay = synthesis - hop;
        for i in fft..input.len() {
            assert!(
                (out[i] - input[i - delay]).abs() < 1e-4,
                "{fft}/{synthesis}/{hop}: sample {i}: {} vs {}",
                out[i],
                input[i - delay]
            );
        }
    }

    #[test]
    fn symmetric_framings_reconstruct_after_one_hop() {
        reconstructs(960, 960, 480);
        reconstructs(480, 480, 240);
    }

    #[test]
    fn asymmetric_framings_reconstruct_after_the_synthesis_overlap() {
        // 960-point analysis at 240-sample hop with 2x, 1.5x, 1.25x, and 1x synthesis windows.
        for synthesis in [480, 360, 300, 240] {
            reconstructs(960, synthesis, 240);
        }
        reconstructs(1440, 480, 240);
        reconstructs(960, 480, 480);
    }

    #[test]
    fn symmetric_windows_are_the_vorbis_window() {
        let (analysis, synthesis) = windows(960, 960, 480);
        let vorbis: Vec<f32> = (0..960)
            .map(|i| {
                let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / 480.).sin();
                (0.5 * std::f64::consts::PI * s * s).sin() as f32
            })
            .collect();
        for (i, (&a, &v)) in analysis.iter().zip(&vorbis).enumerate() {
            assert!((a - v).abs() < 1e-6, "analysis {i}: {a} vs {v}");
        }
        for (i, (&s, &v)) in synthesis.iter().zip(&vorbis).enumerate() {
            assert!((s - v).abs() < 1e-6, "synthesis {i}: {s} vs {v}");
        }
    }

    #[test]
    fn window_products_overlap_add_to_one() {
        for (fft, synthesis, hop) in [
            (960, 480, 240),
            (960, 360, 240),
            (960, 300, 240),
            (480, 480, 240),
        ] {
            let (analysis, synthesis_window) = windows(fft, synthesis, hop);
            assert_eq!(analysis.len(), fft);
            assert_eq!(synthesis_window.len(), synthesis);
            assert!(analysis.iter().all(|&w| w > 0. && w <= 1. + 1e-6));
            assert!(synthesis_window.iter().all(|&w| w > 0.));
            let product: Vec<f32> = analysis[fft - synthesis..]
                .iter()
                .zip(&synthesis_window)
                .map(|(a, s)| a * s)
                .collect();
            for j in 0..hop {
                let sum = product[j]
                    + if j + hop < synthesis {
                        product[j + hop]
                    } else {
                        0.
                    };
                assert!(
                    (sum - 1.).abs() < 1e-5,
                    "{fft}/{synthesis}/{hop} at {j}: {sum}"
                );
            }
            // Analysis window shouldn't have a discontinuity where the rise meets the taper.
            let steps = analysis.windows(2).map(|w| (w[1] - w[0]).abs());
            assert!(steps.fold(0f32, f32::max) < 8. / (synthesis - hop) as f32);
        }
    }

    // Dumps the window pair for `RC2FX_DFN_FRAMING=fft,synthesis,hop` to
    // `RC2FX_DFN_WINDOWS` (one float per line, analysis then synthesis) so the
    // training side's libDF port can diff against it.
    #[test]
    #[ignore]
    fn dump_windows() {
        let framing =
            std::env::var("RC2FX_DFN_FRAMING").expect("RC2FX_DFN_FRAMING=fft,synthesis,hop");
        let out = std::env::var("RC2FX_DFN_WINDOWS").expect("RC2FX_DFN_WINDOWS=path");
        let v: Vec<usize> = framing.split(',').map(|x| x.parse().unwrap()).collect();
        let (analysis, synthesis) = windows(v[0], v[1], v[2]);
        let text: String = analysis
            .iter()
            .chain(&synthesis)
            .map(|w| {
                format!(
                    "{w:.9e}
"
                )
            })
            .collect();
        std::fs::write(out, text).unwrap();
    }

    #[test]
    fn framing_validity() {
        assert!(framing_is_valid(960, 960, 480));
        assert!(framing_is_valid(480, 480, 240));
        assert!(framing_is_valid(960, 480, 240));
        assert!(framing_is_valid(960, 240, 240));
        assert!(!framing_is_valid(960, 480, 0));
        assert!(!framing_is_valid(960, 239, 240));
        assert!(!framing_is_valid(960, 481, 240));
        assert!(!framing_is_valid(480, 960, 480));
        assert!(!framing_is_valid(961, 480, 240));
    }
}
