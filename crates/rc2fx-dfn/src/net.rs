//! DeepFilterNet3's three networks (encoder, ERB decoder, DF decoder), one
//! frame at a time (the GRUs carry their hidden state from frame to frame).
//! Layer sizes, band counts and the STFT framing come from the weights, so
//! slimmed variants (narrower GRUs, fewer layers, fewer deep-filter bins, a
//! shorter hop) load unchanged.
use crate::Error;
use crate::convp::Convp;
use crate::glue::{add_transposed, conv3x3, depthwise_t3, depthwise3, shift3_add, transpose};
use crate::kernels::{QWeights, matvec_q8};
use crate::kernels::{Weights, grouped, pack_pointwise, pointwise, sigmoid_slice, tanh_slice};
use crate::profile;
use std::collections::HashMap;

/// Named f32 tensors in the exporter's format: a u32 count, then per tensor a
/// u32 name length, the name, a u32 rank, u32 dims and little-endian f32 data.
/// Besides the model's state dict, the file carries the STFT framing as
/// `rc2fx.fft_size`, `rc2fx.hop_size` and `rc2fx.lookahead`, each a rank-1
/// tensor holding one integral f32.
pub(crate) struct Tensors(pub(crate) HashMap<String, (Vec<usize>, Vec<f32>)>);

/// The STFT framing a model was trained for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Framing {
    /// FFT size and analysis window length.
    pub(crate) fft: usize,
    /// Synthesis window length: the last `synthesis` samples of each frame
    /// are resynthesized (see `dsp::windows`).
    pub(crate) synthesis: usize,
    pub(crate) hop: usize,
    /// Frames of input past the one being enhanced.
    pub(crate) lookahead: usize,
}

impl Framing {
    pub(crate) fn load(t: &Tensors) -> Result<Self, Error> {
        let framing = Self {
            fft: t.scalar("rc2fx.fft_size")?,
            synthesis: t.scalar("rc2fx.synthesis_size")?,
            hop: t.scalar("rc2fx.hop_size")?,
            lookahead: t.scalar("rc2fx.lookahead")?,
        };
        if !crate::dsp::framing_is_valid(framing.fft, framing.synthesis, framing.hop) {
            return Err(Error::Weights);
        }
        Ok(framing)
    }
}

impl Tensors {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Cursor(bytes);
        let n = r.word()?;
        let mut map = HashMap::new();
        for _ in 0..n {
            let l = r.word()?;
            let name = String::from_utf8(r.take(l)?.to_vec()).map_err(|_| Error::Weights)?;
            let rank = r.word()?;
            let dims = (0..rank).map(|_| r.word()).collect::<Result<Vec<_>, _>>()?;
            let count = dims
                .iter()
                .try_fold(1usize, |a, &d| a.checked_mul(d))
                .ok_or(Error::Weights)?;
            let data = r.take(count.checked_mul(4).ok_or(Error::Weights)?)?;
            let data = data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            map.insert(name, (dims, data));
        }
        Ok(Self(map))
    }

    fn get(&self, name: &str) -> Result<&(Vec<usize>, Vec<f32>), Error> {
        self.0.get(name).ok_or(Error::Weights)
    }

    /// A tensor with the given rank.
    fn shaped(&self, name: &str, rank: usize) -> Result<(&[usize], &[f32]), Error> {
        let (d, v) = self.get(name)?;
        if d.len() != rank || v.is_empty() {
            return Err(Error::Weights);
        }
        Ok((d, v))
    }

    fn vector(&self, name: &str, len: usize) -> Result<Vec<f32>, Error> {
        let (_, v) = self.get(name)?;
        if v.len() != len {
            return Err(Error::Weights);
        }
        Ok(v.clone())
    }

    /// A rank-1 tensor holding one non-negative integer as f32.
    fn scalar(&self, name: &str) -> Result<usize, Error> {
        let (d, v) = self.shaped(name, 1)?;
        let x = v[0];
        if d != [1] || !(0. ..=u32::MAX as f32).contains(&x) || x.fract() != 0. {
            return Err(Error::Weights);
        }
        Ok(x as usize)
    }
}

/// Reads the weights file front to back.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.0.len() {
            return Err(Error::Weights);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn word(&mut self) -> Result<usize, Error> {
        let w = self.take(4)?;
        Ok(u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as usize)
    }
}

/// Batch norm (inference) folded into a per-channel scale and shift, followed
/// by an activation.
struct Norm {
    scale: Vec<f32>,
    shift: Vec<f32>,
}

#[derive(Clone, Copy)]
enum Act {
    Relu,
    Sigmoid,
}

impl Norm {
    fn load(t: &Tensors, prefix: &str, channels: usize) -> Result<Self, Error> {
        let g = t.vector(&format!("{prefix}.weight"), channels)?;
        let b = t.vector(&format!("{prefix}.bias"), channels)?;
        let m = t.vector(&format!("{prefix}.running_mean"), channels)?;
        let v = t.vector(&format!("{prefix}.running_var"), channels)?;
        let scale: Vec<f32> = g
            .iter()
            .zip(&v)
            .map(|(g, v)| g / (v + 1e-5).sqrt())
            .collect();
        let shift = b
            .iter()
            .zip(&m)
            .zip(&scale)
            .map(|((b, m), s)| b - m * s)
            .collect();
        Ok(Self { scale, shift })
    }

    /// x is [channels][f].
    fn apply(&self, x: &mut [f32], f: usize, act: Act) {
        #[cfg(test)]
        {
            use crate::traffic::*;
            let ch = x.len() / f;
            crate::traffic::touch(Class::Param, self.scale.as_ptr(), ch * 4);
            crate::traffic::touch(Class::Param, self.shift.as_ptr(), ch * 4);
            touch(Class::Act, x.as_ptr(), x.len() * 4);
            crate::traffic::store(Class::Act, x.as_ptr(), x.len() * 4);
        }
        for (c, row) in x.chunks_exact_mut(f).enumerate() {
            let (s, h) = (self.scale[c], self.shift[c]);
            for v in row {
                let y = *v * s + h;
                *v = match act {
                    Act::Relu => y.max(0.),
                    Act::Sigmoid => sigmoid(y),
                };
            }
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1. / (1. + (-x).exp())
}

/// Grouped linear layer, sizes from its [groups][in/groups][out/groups] weight.
struct Linear {
    w: Weights,
    groups: usize,
    ws: usize,
    hs: usize,
}

impl Linear {
    fn load(t: &Tensors, name: &str) -> Result<Self, Error> {
        let (d, v) = t.shaped(name, 3)?;
        Ok(Self {
            w: Weights::rows(v, d[2]),
            groups: d[0],
            ws: d[1],
            hs: d[2],
        })
    }
    fn inp(&self) -> usize {
        self.groups * self.ws
    }
    fn out(&self) -> usize {
        self.groups * self.hs
    }
    fn run(&self, x: &[f32], y: &mut [f32]) {
        grouped(&self.w, self.groups, self.ws, self.hs, x, y)
    }
}

/// One GRU layer (PyTorch gate order r, z, n) with its hidden state.
struct Gru {
    wih: QWeights,
    whh: QWeights,
    /// Scratch for the quantized input of `matvec_q8`.
    xq: Vec<i16>,
    bih: Vec<f32>,
    bhh: Vec<f32>,
    hidden: usize,
    h: Vec<f32>,
    /// Advances whenever `h` changes (a step or a reset).
    h_gen: u64,
    /// [3 * hidden]: the step's input products.
    gi: Vec<f32>,
    /// The recurrent products W_hh h (biases not added).
    gh: Vec<f32>,
    /// `Some(h_gen)` when `gh` was prepared ahead (`prepare`) from the
    /// current `h`; the next step then uses it instead of recomputing it.
    gh_for: Option<u64>,
}

impl Gru {
    fn load(t: &Tensors, prefix: &str, layer: usize, inp: usize) -> Result<Self, Error> {
        let (d, wih) = t.shaped(&format!("{prefix}.weight_ih_l{layer}"), 2)?;
        let hidden = d[0] / 3;
        if d[0] != 3 * hidden || d[1] != inp || hidden % 16 != 0 || !inp.is_multiple_of(16) {
            return Err(Error::Weights);
        }
        let (dh, whh) = t.shaped(&format!("{prefix}.weight_hh_l{layer}"), 2)?;
        if dh != [3 * hidden, hidden] {
            return Err(Error::Weights);
        }
        if inp > 1024 {
            return Err(Error::Weights);
        }
        Ok(Self {
            wih: QWeights::new(wih, 3 * hidden, inp),
            whh: QWeights::new(whh, 3 * hidden, hidden),
            xq: vec![0; inp.max(hidden)],
            bih: t.vector(&format!("{prefix}.bias_ih_l{layer}"), 3 * hidden)?,
            bhh: t.vector(&format!("{prefix}.bias_hh_l{layer}"), 3 * hidden)?,
            hidden,
            h: vec![0.; hidden],
            h_gen: 0,
            gi: vec![0.; 3 * hidden],
            gh: vec![0.; 3 * hidden],
            gh_for: None,
        })
    }

    /// Whether `gh` holds the recurrent products of the current `h`.
    fn prepared(&self) -> bool {
        self.gh_for == Some(self.h_gen)
    }

    /// W_hh h for the current hidden state, by the same kernel and from the
    /// same input as the next step would compute it, so the step's result
    /// is unchanged. Returns false if it was already prepared.
    fn prepare(&mut self) -> bool {
        if self.prepared() {
            return false;
        }
        let n = self.hidden;
        matvec_q8(&self.whh, 3 * n, &self.h, &mut self.xq, &mut self.gh);
        self.gh_for = Some(self.h_gen);
        true
    }

    fn discard(&mut self) {
        self.gh_for = None;
    }

    /// Advances one step, replacing `x` with the new hidden state (`inp`
    /// equals `hidden` in a SqueezedGRU). A step whose W_hh h was prepared
    /// (`prepare`) uses it.
    fn step(&mut self, x: &mut [f32]) {
        let n = self.hidden;
        let gi = &mut self.gi[..3 * n];
        matvec_q8(&self.wih, 3 * n, x, &mut self.xq, gi);
        // Biases, gate products and the hidden state the update reads and
        // writes (the products' own inputs are counted by the kernels).
        #[cfg(test)]
        {
            use crate::traffic::*;
            slice(Class::Param, &self.bih);
            slice(Class::Param, &self.bhh);
            touch_n(Class::Act, gi.as_ptr(), 3 * n * 4, 3);
            store_n(Class::Act, gi.as_ptr(), 3 * n * 4, 2);
            touch_n(Class::Act, self.gh.as_ptr(), 3 * n * 4, 2);
            touch_n(Class::State, self.h.as_ptr(), n * 4, 2);
            store(Class::State, self.h.as_ptr(), n * 4);
            store(Class::Act, x.as_ptr(), n * 4);
        }
        if self.gh_for != Some(self.h_gen) {
            self.gh_for = None;
            matvec_q8(&self.whh, 3 * n, &self.h, &mut self.xq, &mut self.gh);
        }
        // r and z gates
        for (((g, bi), h), bh) in gi[..2 * n]
            .iter_mut()
            .zip(&self.bih)
            .zip(&self.gh)
            .zip(&self.bhh)
        {
            *g += bi + h + bh;
        }
        sigmoid_slice(&mut gi[..2 * n]);
        // candidate n = tanh(W_in x + b_in + r (W_hn h + b_hn))
        for j in 0..n {
            let k = 2 * n + j;
            gi[k] += self.bih[k] + gi[j] * (self.gh[k] + self.bhh[k]);
        }
        tanh_slice(&mut gi[2 * n..3 * n]);
        for j in 0..n {
            let z = gi[n + j];
            self.h[j] = (1. - z) * gi[2 * n + j] + z * self.h[j];
        }
        self.h_gen = self.h_gen.wrapping_add(1);
        x.copy_from_slice(&self.h);
    }

    fn reset(&mut self) {
        self.h.fill(0.);
        self.h_gen = self.h_gen.wrapping_add(1);
        self.gh_for = None;
    }
}

/// SqueezedGRU_S: grouped linear in (ReLU), stacked GRUs, optional grouped
/// linear out (ReLU).
struct SqueezedGru {
    lin_in: Linear,
    layers: Vec<Gru>,
    lin_out: Option<Linear>,
    /// [hidden]
    x: Vec<f32>,
}

impl SqueezedGru {
    fn load(t: &Tensors, prefix: &str, inp: usize) -> Result<Self, Error> {
        let lin_in = Linear::load(t, &format!("{prefix}.linear_in.0.weight"))?;
        let hidden = lin_in.out();
        if lin_in.inp() != inp {
            return Err(Error::Weights);
        }
        let layers = (0..)
            .take_while(|l| t.0.contains_key(&format!("{prefix}.gru.weight_ih_l{l}")))
            .map(|l| Gru::load(t, &format!("{prefix}.gru"), l, hidden))
            .collect::<Result<Vec<_>, _>>()?;
        if layers.is_empty() || layers.iter().any(|l| l.hidden != hidden) {
            return Err(Error::Weights);
        }
        let name = format!("{prefix}.linear_out.0.weight");
        let lin_out = match t.0.contains_key(&name) {
            true => Some(Linear::load(t, &name)?),
            false => None,
        };
        if lin_out.as_ref().is_some_and(|o| o.inp() != hidden) {
            return Err(Error::Weights);
        }
        Ok(Self {
            lin_in,
            layers,
            lin_out,
            x: vec![0.; hidden],
        })
    }

    fn out(&self) -> usize {
        self.lin_out
            .as_ref()
            .map(|l| l.out())
            .unwrap_or(self.lin_in.out())
    }

    /// One step: `inp` values of `input` into `out()` values of `y`, layer by
    /// layer.
    fn step(&mut self, input: &[f32], y: &mut [f32]) {
        let x = &mut self.x[..self.lin_in.out()];
        self.lin_in.run(input, x);
        relu(x);
        for l in &mut self.layers {
            l.step(x);
        }
        match &self.lin_out {
            Some(o) => {
                o.run(x, y);
                relu(y);
            }
            None => y[..x.len()].copy_from_slice(x),
        }
    }

    fn reset(&mut self) {
        self.layers.iter_mut().for_each(Gru::reset);
    }

    /// Prepares the first unprepared layer, in step order; false if none.
    fn prepare_next(&mut self) -> bool {
        self.layers
            .iter_mut()
            .find(|l| !l.prepared())
            .is_some_and(Gru::prepare)
    }

    fn unprepared(&self) -> usize {
        self.layers.iter().filter(|l| !l.prepared()).count()
    }

    fn discard(&mut self) {
        self.layers.iter_mut().for_each(Gru::discard);
    }
}

fn relu(x: &mut [f32]) {
    #[cfg(test)]
    crate::traffic::touch(crate::traffic::Class::Act, x.as_ptr(), x.len() * 4);
    #[cfg(test)]
    crate::traffic::store(crate::traffic::Class::Act, x.as_ptr(), x.len() * 4);
    x.iter_mut().for_each(|v| *v = v.max(0.));
}

/// Separable conv block: depthwise 1x3 (stride along frequency), pointwise,
/// batch norm, ReLU. `transposed` swaps the depthwise step for a stride-2
/// transposed one.
struct SepConv {
    dw: Vec<f32>,
    pw: Weights,
    norm: Norm,
    ch: usize,
    stride: usize,
    transposed: bool,
}

impl SepConv {
    fn load(
        t: &Tensors,
        prefix: &str,
        ch: usize,
        stride: usize,
        transposed: bool,
    ) -> Result<Self, Error> {
        let (d, dw) = t.shaped(&format!("{prefix}.0.weight"), 4)?;
        let (dp, pw) = t.shaped(&format!("{prefix}.1.weight"), 4)?;
        if d != [ch, 1, 1, 3] || dp != [ch, ch, 1, 1] {
            return Err(Error::Weights);
        }
        Ok(Self {
            dw: dw.to_vec(),
            pw: Weights::rows(&pack_pointwise(pw, ch, ch), 4),
            norm: Norm::load(t, &format!("{prefix}.2"), ch)?,
            ch,
            stride,
            transposed,
        })
    }

    /// x [ch][f] -> y [ch][fo], the first ch*fo values of `y`. Scratch holds
    /// [ch][fo].
    fn run(&self, x: &[f32], f: usize, tmp: &mut [f32], y: &mut [f32]) {
        let fo = if self.transposed {
            2 * f
        } else {
            f / self.stride
        };
        let len = self.ch * fo;
        let x = &x[..self.ch * f];
        let scratch = &mut tmp[..len];
        if self.transposed {
            depthwise_t3(&self.dw, x, self.ch, f, scratch);
        } else {
            depthwise3(&self.dw, x, self.ch, f, self.stride, scratch);
        }
        pointwise(&self.pw, self.ch, self.ch, tmp, fo, y);
        self.norm.apply(&mut y[..len], fo, Act::Relu);
    }
}

/// 1x1 depthwise "pathway" conv: per-channel scale, batch norm, ReLU.
struct Pathway {
    w: Vec<f32>,
    norm: Norm,
}

impl Pathway {
    fn load(t: &Tensors, prefix: &str, ch: usize) -> Result<Self, Error> {
        Ok(Self {
            w: t.vector(&format!("{prefix}.0.weight"), ch)?,
            norm: Norm::load(t, &format!("{prefix}.1"), ch)?,
        })
    }
    fn run(&self, x: &[f32], f: usize, y: &mut [f32]) {
        #[cfg(test)]
        {
            use crate::traffic::*;
            slice(Class::Param, &self.w);
            touch(Class::Act, x.as_ptr(), x.len() * 4);
            crate::traffic::store(Class::Act, y.as_ptr(), x.len() * 4);
        }
        for (c, (xr, yr)) in x.chunks_exact(f).zip(y.chunks_exact_mut(f)).enumerate() {
            for (a, b) in xr.iter().zip(yr.iter_mut()) {
                *b = a * self.w[c];
            }
        }
        self.norm.apply(&mut y[..x.len()], f, Act::Relu);
    }
}

/// The three networks with all streaming state.
pub(crate) struct Dfn {
    /// Idle inserted after each weight-heavy layer; see `DeepFilter::set_layer_pause`.
    pause: Option<crate::LayerPause>,
    ch: usize,
    nb_erb: usize,
    nb_df: usize,
    // encoder
    erb_conv0: Vec<f32>, // [ch][3][3]
    erb_norm0: Norm,
    erb_conv: [SepConv; 3],
    df_conv0: Vec<f32>, // [ch][3][3], input channel ch / (ch/2)
    df_pw0: Weights,
    df_norm0: Norm,
    df_conv1: SepConv,
    df_fc_emb: Linear,
    enc_gru: SqueezedGru,
    lsnr_w: Vec<f32>,
    lsnr_b: f32,
    /// 3 frames of [1][nb_erb] ERB features, oldest first: the last two of
    /// the previous call, then this call's.
    erb_hist: Vec<f32>,
    /// The same for [2][nb_df] complex features (re, im).
    spec_hist: Vec<f32>,
    // erb decoder
    erb_gru: SqueezedGru,
    pathway: [Pathway; 4], // conv3p, conv2p, conv1p, conv0p
    convt: [SepConv; 3],   // convt3, convt2, convt1
    out_w: Vec<f32>,       // [ch][3]
    out_norm: Norm,
    // df decoder
    df_gru: SqueezedGru,
    df_skip: Linear,
    df_out: Linear,
    /// df_convp's temporal conv over the last frames of c0.
    convp: Convp,
    convp_pw: Vec<f32>,
    convp_norm: Norm,
    convp_out: usize,
    // Activations: what one layer hands the next.
    e0: Vec<f32>,
    e1: Vec<f32>,
    e2: Vec<f32>,
    e3: Vec<f32>,
    c0: Vec<f32>,
    c1: Vec<f32>,
    /// df_fc_emb's output, then the encoder GRU's input.
    cemb: Vec<f32>,
    emb: Vec<f32>,
    /// The ERB decoder GRU's output, then the DF decoder's hidden state.
    erb_g: Vec<f32>,
    df_g: Vec<f32>,
    dbuf: Vec<f32>,
    mask: Vec<f32>,
    coefs: Vec<f32>, // [nb_df][convp_out]
    // Scratch within one layer.
    tmp: Vec<f32>,
    tmp2: Vec<f32>,
    xbuf: Vec<f32>,
}

impl Dfn {
    pub(crate) fn new(t: &Tensors) -> Result<Self, Error> {
        let (d0, erb_conv0) = t.shaped("enc.erb_conv0.1.weight", 4)?;
        let ch = d0[0];
        if d0 != [ch, 1, 3, 3] || ch % 8 != 0 {
            return Err(Error::Weights);
        }
        let (dd, df_conv0) = t.shaped("enc.df_conv0.1.weight", 4)?;
        let (dpw, df_pw0) = t.shaped("enc.df_conv0.2.weight", 4)?;
        if dd != [ch, 1, 3, 3] || dpw != [ch, ch, 1, 1] {
            return Err(Error::Weights);
        }
        // The band counts follow from the first linear layers: the encoder
        // GRU takes e3 ([ch][nb_erb / 4]) and df_fc_emb takes c1
        // ([ch][nb_df / 2]).
        let emb_len = Linear::load(t, "enc.emb_gru.linear_in.0.weight")?.inp();
        let df_fc_emb = Linear::load(t, "enc.df_fc_emb.0.weight")?;
        if emb_len % ch != 0 || df_fc_emb.inp() % ch != 0 {
            return Err(Error::Weights);
        }
        let nb_erb = 4 * emb_len / ch;
        let nb_df = 2 * df_fc_emb.inp() / ch;
        // The pointwise kernels work on whole groups of eight bins, so every
        // width they see (nb_erb, nb_erb / 2, nb_erb / 4, nb_df, nb_df / 2)
        // must be a multiple of eight. Their buffers have no slack past the
        // last bin to absorb a partial group.
        if nb_erb == 0 || nb_erb % 32 != 0 || nb_df == 0 || nb_df % 16 != 0 {
            return Err(Error::Weights);
        }
        let (pd, convp_w) = t.shaped("df_dec.df_convp.1.weight", 4)?;
        let (convp_out, convp_kt) = (pd[0], pd[2]);
        if pd != [convp_out, ch / 2, convp_kt, 1] || convp_out % 2 != 0 || convp_kt == 0 {
            return Err(Error::Weights);
        }
        let (dout, out_w) = t.shaped("erb_dec.conv0_out.0.weight", 4)?;
        if dout != [1, ch, 1, 3] {
            return Err(Error::Weights);
        }
        let enc_gru = SqueezedGru::load(t, "enc.emb_gru", emb_len)?;
        let erb_gru = SqueezedGru::load(t, "erb_dec.emb_gru", emb_len)?;
        let df_gru = SqueezedGru::load(t, "df_dec.df_gru", emb_len)?;
        let df_skip = Linear::load(t, "df_dec.df_skip.weight")?;
        let df_out = Linear::load(t, "df_dec.df_out.0.weight")?;
        let hidden = df_gru.out();
        if df_fc_emb.out() != emb_len
            || enc_gru.out() != emb_len
            || erb_gru.out() != emb_len
            || df_skip.inp() != emb_len
            || df_skip.out() != hidden
            || df_out.inp() != hidden
            || df_out.out() != nb_df * convp_out
        {
            return Err(Error::Weights);
        }
        let big = ch * nb_df;
        // Every use of the two scratch buffers: the encoder's and decoder's
        // [ch][f] layers up to f = nb_erb or nb_df, the DF decoder's hidden
        // state and df_convp's [out][nb_df].
        let scratch = (ch * nb_erb.max(nb_df))
            .max(emb_len)
            .max(hidden)
            .max(convp_out * nb_df);
        Ok(Self {
            ch,
            nb_erb,
            nb_df,
            erb_conv0: erb_conv0.to_vec(),
            erb_norm0: Norm::load(t, "enc.erb_conv0.2", ch)?,
            erb_conv: [
                SepConv::load(t, "enc.erb_conv1", ch, 2, false)?,
                SepConv::load(t, "enc.erb_conv2", ch, 2, false)?,
                SepConv::load(t, "enc.erb_conv3", ch, 1, false)?,
            ],
            df_conv0: df_conv0.to_vec(),
            df_pw0: Weights::rows(&pack_pointwise(df_pw0, ch, ch), 4),
            df_norm0: Norm::load(t, "enc.df_conv0.3", ch)?,
            df_conv1: SepConv::load(t, "enc.df_conv1", ch, 2, false)?,
            df_fc_emb,
            enc_gru,
            lsnr_w: t.vector("enc.lsnr_fc.0.weight", emb_len)?,
            lsnr_b: t.vector("enc.lsnr_fc.0.bias", 1)?[0],
            erb_hist: vec![0.; 3 * nb_erb],
            spec_hist: vec![0.; 3 * 2 * nb_df],
            erb_gru,
            pathway: [
                Pathway::load(t, "erb_dec.conv3p", ch)?,
                Pathway::load(t, "erb_dec.conv2p", ch)?,
                Pathway::load(t, "erb_dec.conv1p", ch)?,
                Pathway::load(t, "erb_dec.conv0p", ch)?,
            ],
            convt: [
                SepConv::load(t, "erb_dec.convt3", ch, 1, false)?,
                SepConv::load(t, "erb_dec.convt2", ch, 2, true)?,
                SepConv::load(t, "erb_dec.convt1", ch, 2, true)?,
            ],
            out_w: out_w.to_vec(),
            out_norm: Norm::load(t, "erb_dec.conv0_out.1", 1)?,
            df_gru,
            df_skip,
            df_out,
            convp: Convp::new(convp_w, convp_out / 2, ch / 2, convp_kt, nb_df),
            convp_pw: t.vector("df_dec.df_convp.2.weight", convp_out * convp_out)?,
            convp_norm: Norm::load(t, "df_dec.df_convp.3", convp_out)?,
            convp_out,
            pause: None,
            e0: vec![0.; ch * nb_erb],
            e1: vec![0.; ch * nb_erb / 2],
            e2: vec![0.; ch * nb_erb / 4],
            e3: vec![0.; ch * nb_erb / 4],
            c0: vec![0.; big],
            c1: vec![0.; big / 2],
            cemb: vec![0.; emb_len],
            emb: vec![0.; emb_len],
            erb_g: vec![0.; emb_len],
            df_g: vec![0.; hidden],
            dbuf: vec![0.; ch * nb_erb],
            mask: vec![0.; nb_erb],
            coefs: vec![0.; nb_df * convp_out],
            tmp: vec![0.; scratch],
            tmp2: vec![0.; scratch],
            xbuf: vec![0.; emb_len],
        })
    }

    /// Raises df_convp's batch-norm shift so its ReLU passes every value.
    /// With the embedded weights the pathway's output is almost always
    /// clipped to zero; opened, whatever its convolution computes reaches
    /// the coefficients and the output samples.
    #[cfg(test)]
    pub(crate) fn open_convp(&mut self) {
        self.convp_norm.shift.iter_mut().for_each(|s| *s += 4.);
    }

    pub(crate) fn set_pause(&mut self, pause: Option<crate::LayerPause>) {
        self.pause = pause;
    }

    /// Deep-filter order (taps per bin); the coefficients hold re/im pairs.
    pub(crate) fn df_order(&self) -> usize {
        self.convp_out / 2
    }

    /// ERB bands in the features and gain mask.
    pub(crate) fn nb_erb(&self) -> usize {
        self.nb_erb
    }

    /// Low bins the deep filter covers.
    pub(crate) fn nb_df(&self) -> usize {
        self.nb_df
    }

    /// Prepares one GRU layer's recurrent products for the next call, in
    /// the order the call reaches them (encoder, ERB decoder, DF decoder);
    /// false if every layer is prepared.
    pub(crate) fn prepare_gru(&mut self) -> bool {
        self.enc_gru.prepare_next() || self.erb_gru.prepare_next() || self.df_gru.prepare_next()
    }

    /// GRU layers not yet prepared for the next call.
    pub(crate) fn gru_unprepared(&self) -> usize {
        self.enc_gru.unprepared() + self.erb_gru.unprepared() + self.df_gru.unprepared()
    }

    pub(crate) fn discard_gru(&mut self) {
        self.enc_gru.discard();
        self.erb_gru.discard();
        self.df_gru.discard();
    }

    pub(crate) fn reset(&mut self) {
        self.enc_gru.reset();
        self.erb_gru.reset();
        self.df_gru.reset();
        self.erb_hist.fill(0.);
        self.spec_hist.fill(0.);
        self.convp.reset();
    }

    /// Encoder for one frame of features; returns its local SNR estimate in
    /// dB.
    pub(crate) fn encode(&mut self, feat_erb: &[f32], feat_re: &[f32], feat_im: &[f32]) -> f32 {
        let (ch, nb_erb, nb_df) = (self.ch, self.nb_erb, self.nb_df);
        #[cfg(test)]
        {
            // The histories are read by the 3x3 convs and shifted at the end.
            use crate::traffic::*;
            touch(
                Class::State,
                self.erb_hist.as_ptr(),
                self.erb_hist.len() * 4,
            );
            touch(
                Class::State,
                self.spec_hist.as_ptr(),
                self.spec_hist.len() * 4,
            );
            store_n(
                Class::State,
                self.erb_hist.as_ptr(),
                self.erb_hist.len() * 4,
                2,
            );
            store_n(
                Class::State,
                self.spec_hist.as_ptr(),
                self.spec_hist.len() * 4,
                2,
            );
            slice(Class::Act, feat_erb);
            slice(Class::Act, feat_re);
            slice(Class::Act, feat_im);
            slice(Class::Param, &self.lsnr_w);
        }
        // The new frame follows the last two of the previous call.
        self.erb_hist[2 * nb_erb..3 * nb_erb].copy_from_slice(feat_erb);
        let newest = &mut self.spec_hist[2 * 2 * nb_df..3 * 2 * nb_df];
        newest[..nb_df].copy_from_slice(&feat_re[..nb_df]);
        newest[nb_df..].copy_from_slice(&feat_im[..nb_df]);
        let (l0, l3) = (ch * nb_erb, ch * nb_erb / 4);

        // erb_conv0: 1 -> ch, 3x3 (3 frames, 3 bins), BN, ReLU.
        let e0 = &mut self.e0[..l0];
        conv3x3(&self.erb_conv0, ch, &self.erb_hist, 1, nb_erb, e0);
        self.erb_norm0.apply(e0, nb_erb, Act::Relu);
        profile::mark(1);
        self.erb_conv[0].run(&self.e0, nb_erb, &mut self.tmp, &mut self.e1);
        self.erb_conv[1].run(&self.e1, nb_erb / 2, &mut self.tmp, &mut self.e2);
        self.erb_conv[2].run(&self.e2, nb_erb / 4, &mut self.tmp, &mut self.e3);
        profile::mark(2);
        pause_for(self.pause);

        // df_conv0: 2 -> ch in two groups, 3x3, then pointwise, BN, ReLU.
        let big = ch * nb_df;
        conv3x3(
            &self.df_conv0,
            ch,
            &self.spec_hist,
            2,
            nb_df,
            &mut self.tmp[..big],
        );
        pointwise(&self.df_pw0, ch, ch, &self.tmp, nb_df, &mut self.c0);
        self.df_norm0.apply(&mut self.c0[..big], nb_df, Act::Relu);
        profile::mark(3);
        self.df_conv1
            .run(&self.c0, nb_df, &mut self.tmp, &mut self.c1);
        profile::mark(4);
        pause_for(self.pause);
        // cemb: c1 flattened bin-major ([f][ch]), grouped linear, ReLU.
        let n = self.df_fc_emb.out();
        let ci = self.df_fc_emb.inp();
        transpose(&self.c1[..big / 2], ch, nb_df / 2, &mut self.tmp[..ci]);
        self.df_fc_emb.run(&self.tmp[..ci], &mut self.cemb[..n]);
        relu(&mut self.cemb[..n]);
        profile::mark(5);
        // emb = GRU(e3 flattened bin-major + cemb).
        let f3 = nb_erb / 4;
        transpose(&self.e3[..l3], ch, f3, &mut self.tmp);
        for (a, b) in self.cemb[..n].iter_mut().zip(&self.tmp[..n]) {
            *a += b;
        }
        self.enc_gru.step(&self.cemb[..n], &mut self.emb[..n]);
        let z: f32 = self
            .lsnr_w
            .iter()
            .zip(&self.emb[..n])
            .map(|(a, b)| a * b)
            .sum::<f32>()
            + self.lsnr_b;
        // Keep the last two frames for the next call.
        self.erb_hist.copy_within(nb_erb..3 * nb_erb, 0);
        self.spec_hist.copy_within(2 * nb_df..3 * 2 * nb_df, 0);
        profile::mark(6);
        pause_for(self.pause);
        sigmoid(z) * 50. - 15.
    }

    /// ERB decoder: the per-band gain mask for the last `encode`'s frame;
    /// see `mask`.
    pub(crate) fn erb_decode(&mut self) {
        let (ch, nb_erb) = (self.ch, self.nb_erb);
        let f3 = nb_erb / 4;
        let n = self.emb.len();
        self.erb_gru.step(&self.emb[..n], &mut self.erb_g[..n]);
        profile::mark(7);
        pause_for(self.pause);
        let ld = ch * nb_erb;
        // d3 = convt3(conv3p(e3) + emb)
        // [f][ch] -> [ch][f]
        transpose(&self.erb_g[..n], f3, ch, &mut self.xbuf);
        self.pathway[0].run(&self.e3[..n], f3, &mut self.tmp2[..n]);
        add(&mut self.tmp2[..n], &self.xbuf);
        self.convt[0].run(&self.tmp2[..n], f3, &mut self.tmp, &mut self.dbuf);
        // d2 = convt2(conv2p(e2) + d3), d1 = convt1(conv1p(e1) + d2)
        let mut f = f3;
        for (k, e) in [(1, &self.e2), (2, &self.e1)] {
            let len = ch * f;
            self.pathway[k].run(&e[..len], f, &mut self.tmp2[..len]);
            add(&mut self.tmp2[..len], &self.dbuf[..len]);
            self.convt[k].run(&self.tmp2[..len], f, &mut self.tmp, &mut self.dbuf);
            f *= 2;
        }
        profile::mark(8);
        pause_for(self.pause);
        // m = conv0_out(conv0p(e0) + d1): ch -> 1, 1x3, BN, sigmoid.
        self.pathway[3].run(&self.e0[..ld], nb_erb, &mut self.tmp2);
        add(&mut self.tmp2[..ld], &self.dbuf[..ld]);
        let mask = &mut self.mask[..nb_erb];
        #[cfg(test)]
        {
            use crate::traffic::*;
            slice(Class::Param, &self.out_w);
            touch(Class::Act, self.tmp2.as_ptr(), ld * 4);
            touch_n(Class::Act, mask.as_ptr(), nb_erb * 4, ch);
            crate::traffic::store_n(Class::Act, mask.as_ptr(), nb_erb * 4, 1 + ch);
        }
        mask.fill(0.);
        for (c, row) in self.tmp2[..ld].chunks_exact(nb_erb).enumerate() {
            let k = &self.out_w[c * 3..c * 3 + 3];
            shift3_add(k[0], k[1], k[2], row, mask);
        }
        self.out_norm.apply(mask, nb_erb, Act::Sigmoid);
        profile::mark(9);
    }

    /// The gain mask from `erb_decode`.
    pub(crate) fn mask(&self) -> &[f32] {
        &self.mask[..self.nb_erb]
    }

    /// DF decoder: deep-filter coefficients for the last `encode`'s frame;
    /// see `coefs`.
    pub(crate) fn df_decode(&mut self) {
        let (ch, nb_df) = (self.ch, self.nb_df);
        let hidden = self.df_gru.out();
        let n = self.emb.len();
        self.df_gru.step(&self.emb[..n], &mut self.df_g[..hidden]);
        self.df_skip.run(&self.emb[..n], &mut self.tmp2[..hidden]);
        add(&mut self.df_g[..hidden], &self.tmp2[..hidden]);
        profile::mark(10);
        pause_for(self.pause);
        // df_out: grouped linear + tanh -> [nb_df][out]
        let out = self.convp_out;
        self.df_out
            .run(&self.df_g[..hidden], &mut self.coefs[..nb_df * out]);
        tanh_slice(&mut self.coefs[..nb_df * out]);
        profile::mark(11);
        // df_convp over the c0 history: ch -> out in two groups, kt x 1
        // (`Convp`), then pointwise, BN, ReLU, added channels-last. The
        // frame's c0 joins the history just before the conv.
        let coefs = &mut self.coefs[..nb_df * out];
        self.convp.step(&self.c0[..ch * nb_df]);
        // The 1x1 mix reads each of its weights over every bin.
        #[cfg(test)]
        {
            use crate::traffic::*;
            slice(Class::Param, &self.convp_pw);
            touch_n(Class::Act, self.tmp2.as_ptr(), out * nb_df * 4, out);
            crate::traffic::store_n(Class::Act, self.tmp2.as_ptr(), out * nb_df * 4, 1 + out);
            touch(Class::Act, coefs.as_ptr(), nb_df * out * 4);
            crate::traffic::store(Class::Act, coefs.as_ptr(), nb_df * out * 4);
        }
        let z = &mut self.tmp2[..out * nb_df];
        self.convp.mix(&self.convp_pw, z);
        self.convp_norm.apply(z, nb_df, Act::Relu);
        add_transposed(z, out, nb_df, coefs);
        profile::mark(12);
    }

    /// The coefficients from `df_decode`, [nb_df][order * 2].
    pub(crate) fn coefs(&self) -> &[f32] {
        &self.coefs[..self.nb_df * self.convp_out]
    }
}

/// Lets the memory bus go between weight-heavy layers.
#[inline(always)]
fn pause_for(pause: Option<crate::LayerPause>) {
    match pause {
        None => {}
        Some(crate::LayerPause::Sleep(d)) => std::thread::sleep(d),
        Some(crate::LayerPause::Spin(d)) => {
            let end = std::time::Instant::now() + d;
            while std::time::Instant::now() < end {
                std::hint::spin_loop();
            }
        }
    }
}

fn add(a: &mut [f32], b: &[f32]) {
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch(Class::Act, a.as_ptr(), a.len().min(b.len()) * 4);
        crate::traffic::store(Class::Act, a.as_ptr(), a.len().min(b.len()) * 4);
        touch(Class::Act, b.as_ptr(), a.len().min(b.len()) * 4);
    }
    a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
}
