//! DeepFilterNet3 speech enhancement for the mixer, in plain Rust.
//!
//! A streaming port of DeepFilterNet's inference: libDF's framing and features
//! (`dsp`) feeding the three networks (`net`), with the large weights stored
//! as int8 with one scale per row and NEON kernels on AArch64. Every stage
//! runs on every frame, as in training; libDF's local-SNR shortcuts are not
//! used. The STFT framing (FFT size, synthesis window, hop, lookahead) and the
//! band counts come from the weights file, so one build runs any
//! DeepFilterNet3-shaped model. Each `process` call takes one STFT hop. The
//! weights are embedded from `model/weights.bin` (LA0) and
//! `model/la1/weights.bin` (LA1); see `model/README.md`.
mod convp;
mod dsp;
mod glue;
pub mod hold;
mod kernels;
mod net;
#[cfg(test)]
mod traffic;

use dsp::Dsp;
use net::{Dfn, Framing, STEPS, Tensors};
use realfft::num_complex::Complex32;
use std::collections::VecDeque;

static WEIGHTS: &[u8] = include_bytes!("../model/weights.bin");

/// The preserved LA1 export (960 / 360 / 240, `df_lookahead` 1), embedded
/// alongside LA0 so one build can run either (`model/la1/README.md`).
static LA1_WEIGHTS: &[u8] = include_bytes!("../model/la1/weights.bin");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The weights are malformed or don't describe DeepFilterNet3.
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

/// One channel's enhancer. Construction allocates; `process` and `reset` don't.
/// How `DeepFilter::set_layer_pause` idles. A sleep gives the core away and
/// overshoots by about 0.1 ms on the mixer. A spin holds the core, costs
/// exactly its length and counts as run time: on a real-time thread it
/// draws on the kernel's real-time budget for the core (95% of each second),
/// which the mixer's USB gadget interrupt thread on CPU 0 depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerPause {
    Sleep(std::time::Duration),
    Spin(std::time::Duration),
}

pub struct DeepFilter {
    net: Dfn,
    dsp: Dsp,
    framing: Framing,
    order: usize,
    nb_df: usize,
    /// Recent spectra, oldest first: `masked` for the ERB gains (applied
    /// `lookahead` frames back), `noisy` for the deep filter's `order` taps.
    /// Each holds `STEPS - 1` more than one hop needs, as a call pushes all
    /// its hops before filtering the first.
    masked: VecDeque<Vec<Complex32>>,
    noisy: VecDeque<Vec<Complex32>>,
    spec: Vec<Complex32>,
    enhanced: Vec<Complex32>,
    /// [STEPS][..]: each hop's features.
    feat_erb: Vec<f32>,
    feat_re: Vec<f32>,
    feat_im: Vec<f32>,
    /// See `set_gru_prep`.
    gru_prep: bool,
}

impl DeepFilter {
    /// The embedded model, LA0.
    pub fn new() -> Result<Self, Error> {
        Self::for_lookahead(0)
    }

    /// An embedded model by lookahead: 0 is LA0, 1 is the preserved LA1
    /// export. Any other value is refused.
    pub fn for_lookahead(lookahead: u8) -> Result<Self, Error> {
        match lookahead {
            0 => Self::from_weights(WEIGHTS),
            1 => Self::from_weights(LA1_WEIGHTS),
            _ => Err(Error::Weights),
        }
    }

    /// A model from a weights file in the exporter's format (`model/README.md`).
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
            masked: (0..order + framing.lookahead + STEPS - 1)
                .map(|_| zero.clone())
                .collect(),
            // The lookahead is under the order, so the taps span the ring.
            noisy: (0..order + STEPS - 1).map(|_| zero.clone()).collect(),
            spec: zero.clone(),
            enhanced: zero,
            feat_erb: vec![0.; STEPS * nb_erb],
            feat_re: vec![0.; STEPS * nb_df],
            feat_im: vec![0.; STEPS * nb_df],
            gru_prep: false,
        })
    }

    /// Samples in and out per `process` call: one STFT hop.
    pub fn frame(&self) -> usize {
        self.framing.hop
    }

    /// Samples the output lags the input: the synthesis window's overlap
    /// plus the model's lookahead frames.
    pub fn delay(&self) -> usize {
        let Framing {
            synthesis,
            hop,
            lookahead,
            ..
        } = self.framing;
        synthesis - hop + lookahead * hop
    }

    /// Frames of input the model sees past the one it enhances.
    pub fn lookahead(&self) -> usize {
        self.framing.lookahead
    }

    /// Enhances one frame (`frame()` samples) in place; the result belongs to
    /// the input `delay()` samples earlier. Returns the model's local SNR
    /// estimate in dB (-15 to 35) for the frame's newest hop, its measure of
    /// speech presence.
    pub fn process(&mut self, frame: &mut [f32]) -> f32 {
        assert_eq!(frame.len(), self.frame());
        self.process_hops(frame)
    }

    /// `process` for one to `STEPS` hops.
    fn process_hops(&mut self, frame: &mut [f32]) -> f32 {
        let (hop, nb_df) = (self.framing.hop, self.nb_df);
        let nb_erb = self.feat_erb.len() / STEPS;
        let steps = frame.len() / hop;
        assert!(frame.len() == steps * hop && (1..=STEPS).contains(&steps));
        profile::start();
        for (t, input) in frame.chunks_exact(hop).enumerate() {
            // The rings' oldest spectra are overwritten; the STFT's own
            // buffers are counted in dsp.
            #[cfg(test)]
            {
                use traffic::*;
                slice(Class::Act, &self.spec);
                traffic::store(
                    Class::State,
                    self.masked[0].as_ptr(),
                    self.masked[0].len() * 8,
                );
                traffic::store(
                    Class::State,
                    self.noisy[0].as_ptr(),
                    self.noisy[0].len() * 8,
                );
            }
            self.dsp.analysis(input, &mut self.spec);
            push(&mut self.masked, &self.spec);
            push(&mut self.noisy, &self.spec);
            self.dsp
                .feat_erb(&self.spec, &mut self.feat_erb[t * nb_erb..(t + 1) * nb_erb]);
            self.dsp.feat_cplx(
                &self.spec[..nb_df],
                &mut self.feat_re[t * nb_df..(t + 1) * nb_df],
                &mut self.feat_im[t * nb_df..(t + 1) * nb_df],
            );
        }
        profile::mark(0);
        let lsnr = self.net.encode(
            &self.feat_erb[..steps * nb_erb],
            &self.feat_re[..steps * nb_df],
            &self.feat_im[..steps * nb_df],
        );
        self.net.erb_decode(steps);
        self.net.df_decode(steps);

        for (t, output) in frame.chunks_exact_mut(hop).enumerate() {
            // Hop t's spectra sit `later` places before the rings' ends.
            let later = steps - 1 - t;
            // Stage 1: ERB gains on the spectrum `lookahead` frames back.
            let at = self.masked.len() - 1 - later - self.framing.lookahead;
            let target = &mut self.masked[at];
            #[cfg(test)]
            {
                use traffic::*;
                touch_n(Class::State, target.as_ptr(), target.len() * 8, 1);
                traffic::store(Class::State, target.as_ptr(), target.len() * 8);
                slice(Class::Act, &self.enhanced);
                for spec in self
                    .noisy
                    .range(self.noisy.len() - later - self.order..self.noisy.len() - later)
                {
                    slice(Class::State, &spec[..nb_df]);
                }
                slice(Class::Act, self.net.coefs(t));
            }
            self.dsp.apply_gains(target, self.net.mask(t));
            self.enhanced.copy_from_slice(target);
            // Stage 2: the deep filter replaces the lowest bins with a complex
            // filter over the last `order` noisy spectra, oldest tap first;
            // the newest is `lookahead` frames past the target.
            let coefs = self.net.coefs(t);
            let end = self.noisy.len() - later;
            for (f, out) in self.enhanced[..nb_df].iter_mut().enumerate() {
                let taps = &coefs[f * self.order * 2..(f + 1) * self.order * 2];
                *out = self
                    .noisy
                    .range(end - self.order..end)
                    .zip(taps.as_chunks::<2>().0)
                    .map(|(spec, c)| spec[f] * Complex32::new(c[0], c[1]))
                    .sum();
            }
            profile::mark(13);
            self.dsp.synthesis(&mut self.enhanced, output);
            profile::mark(14);
        }
        lsnr
    }

    /// Idles six times per `process` call, after each weight-heavy layer (the encoder
    /// convolutions, the three GRUs, the ERB decoder's pathways), so that no
    /// stretch of weight streaming from DRAM lasts longer than about 0.4 ms.
    /// On the mixer the four cores and the audio DMA share one DRAM; a core
    /// streaming for over a millisecond at a time made JACK drop buffers
    /// while the same bytes in half-millisecond bursts did not
    /// (2026-10-29, one hop per call). A call's wall
    /// time grows by six idles and must still fit the frame period; the
    /// result is unchanged.
    /// `None` (the default) never pauses.
    pub fn set_layer_pause(&mut self, pause: Option<LayerPause>) {
        self.net.set_pause(pause);
    }

    /// Experimental staged preparation, off by default. When on,
    /// `prepare_step` computes one GRU layer's recurrent products (W_hh h,
    /// which depend only on that layer's hidden state) ahead of the next
    /// `process` call, which then uses them instead of computing them
    /// itself. Kernel, input and arithmetic are unchanged, so the output is
    /// bit for bit the same, prepared or not; the work moves rather than
    /// shrinks. Only a call's first hop can use prepared products. A reset
    /// or the next call discards them; turning this off discards them too.
    pub fn set_gru_prep(&mut self, on: bool) {
        self.gru_prep = on;
        if !on {
            self.net.discard_gru();
        }
    }

    pub fn gru_prep(&self) -> bool {
        self.gru_prep
    }

    /// GRU layers that `prepare_step` could still prepare for the next call
    /// (0 when staged preparation is off).
    pub fn prep_pending(&self) -> usize {
        if self.gru_prep {
            self.net.gru_unprepared()
        } else {
            0
        }
    }

    /// Prepares one more GRU layer for the next call (see `set_gru_prep`),
    /// in the order the call needs them. Returns false, doing nothing, when
    /// staged preparation is off or every layer is prepared. One step is
    /// one matrix product that runs to completion: it allocates nothing and
    /// can't be cut short.
    pub fn prepare_step(&mut self) -> bool {
        self.gru_prep && self.net.prepare_gru()
    }

    /// Prepares layers one at a time while `go()` returns true, asking it
    /// before each; returns how many were prepared. A caller that stops
    /// once input is waiting still finishes the layer it had started when
    /// that input arrived, so the input can wait up to one step (the layer
    /// is then used by the call, not discarded).
    pub fn prepare_while(&mut self, mut go: impl FnMut() -> bool) -> usize {
        let mut done = 0;
        while self.prep_pending() > 0 && go() && self.prepare_step() {
            done += 1;
        }
        done
    }

    /// Forgets all history, as if newly constructed.
    pub fn reset(&mut self) {
        self.net.reset();
        self.dsp.reset();
        for spec in self.masked.iter_mut().chain(self.noisy.iter_mut()) {
            spec.fill(Complex32::default());
        }
    }
}

/// A switch for the tests: while it is on for the calling thread, the model
/// runs the reference forms of the kernels that also have a faster one (the
/// scalar loops of `glue`, the full-history `convp::reference`), so one
/// binary can compare the two bit for bit. Production builds have no switch;
/// `on` is the constant false there.
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

/// The bench's choice of int8 matrix kernel, in builds with the
/// `bench-kernels` feature (`rc2fx-bench`). Other builds have only the tiled
/// kernel and no switch.
#[cfg(feature = "bench-kernels")]
pub mod bench_kernels {
    use std::sync::atomic::{AtomicBool, Ordering};

    static ROW_MAJOR: AtomicBool = AtomicBool::new(false);

    /// Runs every model's GRU products with the row-major kernel (true), the
    /// production kernel until 2026-11-10, or with the tiled kernel that
    /// replaced it (false, the default). The two give the same bits. Set it
    /// between `process` calls.
    pub fn set_row_major_q8(on: bool) {
        ROW_MAJOR.store(on, Ordering::Relaxed);
    }

    #[inline(always)]
    pub(crate) fn row_major_q8() -> bool {
        ROW_MAJOR.load(Ordering::Relaxed)
    }

    /// Mean nanoseconds per 384 x 128 product (the GRU's shape) over
    /// `iterations` products of one cache-resident matrix: the row-major
    /// kernel, then the tiled one. Also checks that the two agree in every
    /// bit. This is the arithmetic alone; a live frame also waits on memory.
    pub fn resident_product_ns(iterations: usize) -> [f64; 2] {
        crate::kernels::bench_resident_products(iterations)
    }
}

/// Drops the oldest spectrum and appends `spec`, reusing the dropped buffer.
fn push(ring: &mut VecDeque<Vec<Complex32>>, spec: &[Complex32]) {
    if let Some(mut oldest) = ring.pop_front() {
        oldest.copy_from_slice(spec);
        ring.push_back(oldest);
    }
}

/// Per-stage timing of `process`, printed to stderr every 2,000 hops when
/// built with `--cfg dfn_profile` (RUSTFLAGS); compiled out otherwise.
/// `start` opens a call; each `mark(stage)` charges the time since the
/// previous mark to that stage. Stages are the model's layers in order; the
/// layers run once per call for all its hops, the last two stages once per
/// hop, and the means are per hop.
pub(crate) mod profile {
    #[cfg(dfn_profile)]
    use std::{
        cell::Cell,
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };
    #[cfg(dfn_profile)]
    const STAGES: usize = 15;
    #[cfg(dfn_profile)]
    const NAMES: [&str; STAGES] = [
        "features",
        "erb_conv0",
        "erb_conv1-3",
        "df_conv0+pw",
        "df_conv1",
        "df_fc_emb",
        "enc_gru+lsnr",
        "erb_gru",
        "erb_pathways+convt",
        "erb_out",
        "df_gru+skip",
        "df_out",
        "df_convp",
        "df_filter",
        "apply+synthesis",
    ];
    #[cfg(dfn_profile)]
    static TOTALS: [AtomicU64; STAGES] = [const { AtomicU64::new(0) }; STAGES];
    #[cfg(dfn_profile)]
    static FRAMES: AtomicU64 = AtomicU64::new(0);
    #[cfg(dfn_profile)]
    thread_local! { static LAST: Cell<Option<Instant>> = const { Cell::new(None) }; }

    #[inline(always)]
    pub(crate) fn start() {
        #[cfg(test)]
        crate::traffic::start();
        #[cfg(dfn_profile)]
        LAST.with(|l| l.set(Some(Instant::now())));
    }

    #[inline(always)]
    pub(crate) fn mark(stage: usize) {
        #[cfg(test)]
        crate::traffic::mark(stage);
        #[cfg(not(dfn_profile))]
        let _ = stage;
        #[cfg(dfn_profile)]
        {
            let now = Instant::now();
            if let Some(last) = LAST.with(|l| l.replace(Some(now))) {
                TOTALS[stage].fetch_add((now - last).as_nanos() as u64, Ordering::Relaxed);
            }
            if stage == STAGES - 1 {
                let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(2000) {
                    let total: u64 = TOTALS.iter().map(|t| t.load(Ordering::Relaxed)).sum();
                    let line: Vec<String> = NAMES
                        .iter()
                        .zip(&TOTALS)
                        .map(|(name, t)| {
                            let t = t.load(Ordering::Relaxed);
                            format!(
                                "{name} {:.3} ms ({:.0}%)",
                                t as f64 / n as f64 / 1e6,
                                100. * t as f64 / total as f64
                            )
                        })
                        .collect();
                    eprintln!(
                        "dfn profile after {n} hops, mean per hop {:.3} ms: {}",
                        total as f64 / n as f64 / 1e6,
                        line.join(", ")
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
