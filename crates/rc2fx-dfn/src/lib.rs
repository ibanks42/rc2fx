//! DeepFilterNet3 speech enhancement in pure Rust.
//!
//! Streaming port of DeepFilterNet inference: libDF STFT framing and features
//! (`dsp`) feeding the three networks (`net`), with large weight matrices
//! quantized to per-row int8 and accelerated with NEON on AArch64. Every stage
//! runs on every hop (libDF's local-SNR shortcuts are omitted to match
//! training). Framing parameters and band counts are loaded from the weights
//! file, so a single build handles any DeepFilterNet3-shaped export. Embedded
//! weights live at `model/weights.bin` (LA0) and `model/la1/weights.bin` (LA1);
//! see `model/README.md`.
#[cfg(not(target_os = "linux"))]
compile_error!("rc2fx's mixer crates build for Linux only");
mod convp;
mod dsp;
mod glue;
pub mod hold;
mod kernels;
mod net;

use dsp::Dsp;
use net::{Dfn, Framing, Tensors};
use realfft::num_complex::Complex32;
use std::collections::VecDeque;

static WEIGHTS: &[u8] = include_bytes!("../model/weights.bin");

/// Embedded LA1 export (960 / 360 / 240, `df_lookahead = 1`); see `model/la1/README.md`.
static LA1_WEIGHTS: &[u8] = include_bytes!("../model/la1/weights.bin");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Malformed weights or incompatible DeepFilterNet3 layout.
    Weights,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Weights => f.write_str("invalid DeepFilterNet weights"),
        }
    }
}

impl std::error::Error for Error {}

/// Idle strategy for [`DeepFilter::set_layer_pause`].
///
/// `Sleep` yields the core (overshoots by ~0.1 ms on the mixer). `Spin` holds
/// the core for the exact duration and counts against the RT scheduler budget
/// (95% per second), which is shared with the USB gadget IRQ thread on CPU 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerPause {
    Sleep(std::time::Duration),
    Spin(std::time::Duration),
}

/// Single-channel DeepFilterNet3 enhancer. Allocates on construction; `process`
/// and `reset` are allocation-free.
pub struct DeepFilter {
    net: Dfn,
    dsp: Dsp,
    framing: Framing,
    order: usize,
    nb_df: usize,
    /// Recent spectra, oldest first: `masked` for ERB gains (`lookahead`
    /// frames back), `noisy` for the `order`-tap deep filter.
    masked: VecDeque<Vec<Complex32>>,
    noisy: VecDeque<Vec<Complex32>>,
    spec: Vec<Complex32>,
    enhanced: Vec<Complex32>,
    feat_erb: Vec<f32>,
    feat_re: Vec<f32>,
    feat_im: Vec<f32>,
    gru_prep: bool,
}

impl DeepFilter {
    /// Loads the default embedded model (LA0).
    pub fn new() -> Result<Self, Error> {
        Self::for_lookahead(0)
    }

    /// Loads an embedded model by lookahead (`0` = LA0, `1` = LA1).
    pub fn for_lookahead(lookahead: u8) -> Result<Self, Error> {
        match lookahead {
            0 => Self::from_weights(WEIGHTS),
            1 => Self::from_weights(LA1_WEIGHTS),
            _ => Err(Error::Weights),
        }
    }

    /// Loads a model from an exported weights blob (`model/README.md`).
    pub fn from_weights(bytes: &[u8]) -> Result<Self, Error> {
        let tensors = Tensors::parse(bytes)?;
        let framing = Framing::load(&tensors)?;
        let net = Dfn::new(&tensors)?;
        let order = net.df_order();
        let (nb_erb, nb_df) = (net.nb_erb(), net.nb_df());
        let bins = framing.fft / 2 + 1;
        if framing.lookahead >= order || nb_df > bins {
            return Err(Error::Weights);
        }
        let zero = vec![Complex32::default(); bins];
        Ok(Self {
            net,
            dsp: Dsp::new(framing.fft, framing.synthesis, framing.hop, nb_erb, nb_df),
            framing,
            order,
            nb_df,
            masked: (0..order + framing.lookahead)
                .map(|_| zero.clone())
                .collect(),
            // `lookahead < order` guarantees the DF taps stay within this ring.
            noisy: (0..order).map(|_| zero.clone()).collect(),
            spec: zero.clone(),
            enhanced: zero,
            feat_erb: vec![0.; nb_erb],
            feat_re: vec![0.; nb_df],
            feat_im: vec![0.; nb_df],
            gru_prep: false,
        })
    }

    /// Hop size in samples (input and output length per `process` call).
    pub fn frame(&self) -> usize {
        self.framing.hop
    }

    /// End-to-end latency in samples (`synthesis - hop + lookahead * hop`).
    pub fn delay(&self) -> usize {
        let Framing {
            synthesis,
            hop,
            lookahead,
            ..
        } = self.framing;
        synthesis - hop + lookahead * hop
    }

    /// Lookahead in frames.
    pub fn lookahead(&self) -> usize {
        self.framing.lookahead
    }

    /// Enhances one hop (`frame()` samples) in place, corresponding to input
    /// from `delay()` samples earlier. Returns the estimated local SNR in dB
    /// (`[-15, 35]`).
    pub fn process(&mut self, frame: &mut [f32]) -> f32 {
        assert_eq!(frame.len(), self.frame());
        let nb_df = self.nb_df;
        self.dsp.analysis(frame, &mut self.spec);
        push(&mut self.masked, &self.spec);
        push(&mut self.noisy, &self.spec);
        self.dsp.feat_erb(&self.spec, &mut self.feat_erb);
        self.dsp
            .feat_cplx(&self.spec[..nb_df], &mut self.feat_re, &mut self.feat_im);
        let lsnr = self
            .net
            .encode(&self.feat_erb, &self.feat_re, &self.feat_im);
        self.net.erb_decode();
        self.net.df_decode();

        // Stage 1: apply ERB gains to the spectrum `lookahead` frames back.
        let at = self.masked.len() - 1 - self.framing.lookahead;
        let target = &mut self.masked[at];
        self.dsp.apply_gains(target, self.net.mask());
        self.enhanced.copy_from_slice(target);
        // Stage 2: replace the lowest `nb_df` bins with the `order`-tap complex
        // deep filter over recent noisy spectra (oldest tap first; newest tap
        // is `lookahead` frames ahead of the target).
        let coefs = self.net.coefs();
        let end = self.noisy.len();
        for (f, out) in self.enhanced[..nb_df].iter_mut().enumerate() {
            let taps = &coefs[f * self.order * 2..(f + 1) * self.order * 2];
            *out = self
                .noisy
                .range(end - self.order..end)
                .zip(taps.as_chunks::<2>().0)
                .map(|(spec, c)| spec[f] * Complex32::new(c[0], c[1]))
                .sum();
        }
        self.dsp.synthesis(&mut self.enhanced, frame);
        lsnr
    }

    /// Inserts six short pauses per `process` call after weight-heavy layers
    /// (encoder convs, the three GRUs, ERB decoder paths) so DRAM streaming
    /// bursts stay under ~0.4 ms. On the mixer, >1 ms continuous streaming on
    /// the shared DRAM bus caused JACK xruns whereas 0.5 ms bursts didn't
    /// (2026-10-29). Output is unchanged; `None` disables pausing.
    pub fn set_layer_pause(&mut self, pause: Option<LayerPause>) {
        self.net.set_pause(pause);
    }

    /// Enables experimental staged GRU preparation (off by default).
    ///
    /// When enabled, [`prepare_step`](Self::prepare_step) precomputes one GRU
    /// layer's recurrent matvec (`W_hh * h`) ahead of the next `process` call.
    /// The math and kernels are identical, so output is bit-for-bit unchanged;
    /// work is shifted in time, not reduced. Resetting, processing a frame, or
    /// disabling this discards any prepared products.
    pub fn set_gru_prep(&mut self, on: bool) {
        self.gru_prep = on;
        if !on {
            self.net.discard_gru();
        }
    }

    /// Number of GRU layers not yet prepared for the next hop (`0` when disabled).
    pub fn prep_pending(&self) -> usize {
        if self.gru_prep {
            self.net.gru_unprepared()
        } else {
            0
        }
    }

    /// Prepares the next unprepared GRU layer in execution order. Returns
    /// `false` if staged prep is off or all layers are ready. Runs one matvec
    /// to completion without allocating.
    pub fn prepare_step(&mut self) -> bool {
        self.gru_prep && self.net.prepare_gru()
    }

    /// Prepares GRU layers while `go()` returns `true`, returning the count
    /// completed. Each step runs to completion once started, so arriving input
    /// may wait up to one matvec (and then reuses the result).
    pub fn prepare_while(&mut self, mut go: impl FnMut() -> bool) -> usize {
        let mut done = 0;
        while self.prep_pending() > 0 && go() && self.prepare_step() {
            done += 1;
        }
        done
    }

    /// Clears all internal state and history.
    pub fn reset(&mut self) {
        self.net.reset();
        self.dsp.reset();
        for spec in self.masked.iter_mut().chain(self.noisy.iter_mut()) {
            spec.fill(Complex32::default());
        }
    }
}

/// Test-only thread-local switch to force scalar/reference kernel paths
/// (`glue`, `convp::reference`) for bit-exact differential checks. Compiled
/// away in non-test builds.
pub(crate) mod reference {
    #[cfg(test)]
    thread_local! {
        static ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[inline(always)]
    pub(crate) fn on() -> bool {
        #[cfg(test)]
        return ON.get();
        #[cfg(not(test))]
        false
    }

    #[cfg(test)]
    pub(crate) fn set(on: bool) {
        ON.set(on);
    }
}

/// Runtime switch between tiled and legacy row-major int8 matvec kernels for
/// `rc2fx-bench` (`bench-kernels` feature).
#[cfg(feature = "bench-kernels")]
pub mod bench_kernels {
    use std::sync::atomic::{AtomicBool, Ordering};

    static ROW_MAJOR: AtomicBool = AtomicBool::new(false);

    /// Switch GRU matvecs between the legacy row-major kernel (`true`, used
    /// until 2026-11-10) and the tiled kernel (`false`, default). Outputs match
    /// bit-for-bit. Call between `process` invocations.
    pub fn set_row_major_q8(on: bool) {
        ROW_MAJOR.store(on, Ordering::Relaxed);
    }

    #[inline(always)]
    pub(crate) fn row_major_q8() -> bool {
        ROW_MAJOR.load(Ordering::Relaxed)
    }

    /// Returns mean ns per 384x128 matvec for `[row_major, tiled]` on a
    /// cache-resident matrix after verifying bit-exact equality. Measures
    /// compute only (no DRAM streaming).
    pub fn resident_product_ns(iterations: usize) -> [f64; 2] {
        crate::kernels::bench_resident_products(iterations)
    }
}

/// Rotates `spec` into the ring, reusing the dropped buffer's allocation.
fn push(ring: &mut VecDeque<Vec<Complex32>>, spec: &[Complex32]) {
    if let Some(mut oldest) = ring.pop_front() {
        oldest.copy_from_slice(spec);
        ring.push_back(oldest);
    }
}

#[cfg(test)]
mod tests;
