//! The few dense kernels the networks need. Large weights are stored as int8
//! with one f32 scale per row and widened as they are read (NEON on AArch64);
//! all arithmetic is f32. Elsewhere the weights are kept as the dequantised
//! f32 values, so results match the mixer's.

/// A weight array: int8 with one f32 scale per `row` consecutive values
/// (symmetric, `|q| <= 127`).
pub(crate) struct Weights {
    #[cfg(target_arch = "aarch64")]
    q: Vec<i8>,
    #[cfg(target_arch = "aarch64")]
    scale: Vec<f32>,
    #[cfg(target_arch = "aarch64")]
    row: usize,
    #[cfg(not(target_arch = "aarch64"))]
    vals: Vec<f32>,
}

impl Weights {
    /// `row` is the span of one quantisation scale.
    pub(crate) fn rows(v: &[f32], row: usize) -> Self {
        debug_assert!(row > 0 && v.len().is_multiple_of(row));
        {
            let (q, scale) = quantize_rows(v, row);
            #[cfg(target_arch = "aarch64")]
            return Self { q, scale, row };
            #[cfg(not(target_arch = "aarch64"))]
            return Self {
                vals: dequantize_rows(&q, &scale, row),
            };
        }
    }
}

/// Symmetric int8 per `row` values: `w = q * scale`.
fn quantize_rows(w: &[f32], row: usize) -> (Vec<i8>, Vec<f32>) {
    let mut q = Vec::with_capacity(w.len());
    let mut scale = Vec::with_capacity(w.len() / row);
    for r in w.chunks_exact(row) {
        let max = r.iter().fold(0f32, |m, &x| m.max(x.abs()));
        let s = if max > 0. { max / 127. } else { 1. };
        scale.push(s);
        q.extend(r.iter().map(|&x| (x / s).round().clamp(-127., 127.) as i8));
    }
    (q, scale)
}

#[cfg(not(target_arch = "aarch64"))]
fn dequantize_rows(q: &[i8], scale: &[f32], row: usize) -> Vec<f32> {
    q.iter()
        .enumerate()
        .map(|(i, &v)| v as f32 * scale[i / row])
        .collect()
}

/// f32 to IEEE half precision, rounding to nearest even.
#[cfg(test)]
pub(crate) fn f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // Subnormal: the 24-bit significand shifted down to units of 2^-24.
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let (q, rem, halfway) = (m >> shift, m & ((1 << shift) - 1), 1 << (shift - 1));
        let q = if rem > halfway || (rem == halfway && q & 1 == 1) {
            q + 1
        } else {
            q
        };
        return sign | q as u16;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    // A carry out of the significand correctly bumps the exponent (to infinity at the top).
    if rem > 0x1000 || (rem == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    sign | h as u16
}

/// IEEE half precision to f32.
#[cfg(test)]
pub(crate) fn f16_value(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            // Subnormal: normalise the significand.
            let shift = man.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | ((man << shift) & 0x3ff) << 13
        }
        0x1f => sign | 0x7f80_0000 | man << 13,
        _ => sign | (exp + 112) << 23 | man << 13,
    };
    f32::from_bits(bits)
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    /// Four int8 weights times one scale, as an f32 vector. `p` must point
    /// to four readable i8.
    #[inline(always)]
    pub(super) unsafe fn w4_q8(p: *const i8, scale: f32) -> float32x4_t {
        // SAFETY: the caller guarantees 4 readable bytes at `p`.
        unsafe {
            let raw = (p as *const i32).read_unaligned();
            let q = vreinterpret_s8_s32(vdup_n_s32(raw));
            let wide = vmovl_s16(vget_low_s16(vmovl_s8(q)));
            vmulq_n_f32(vcvtq_f32_s32(wide), scale)
        }
    }

    /// `pointwise_block` for int8 weights with one scale per [inp] row of
    /// four outputs (`sp[i]`).
    #[inline(always)]
    pub(super) unsafe fn pointwise_block_q8<const NV: usize, const N: usize>(
        wp: *const i8,
        sp: *const f32,
        inp: usize,
        xp: *const f32,
        f: usize,
        yp: *mut f32,
        ys: usize,
    ) {
        // SAFETY: as `pointwise_block`.
        unsafe {
            let mut acc = [[[vdupq_n_f32(0.); NV]; 4]; N];
            for i in 0..inp {
                let wv = w4_q8(wp.add(i * 4), *sp.add(i));
                for (t, acc) in acc.iter_mut().enumerate() {
                    let xr = xp.add(t * inp * f + i * f);
                    let [a, b, c, d] = acc;
                    for (k, (((a, b), c), d)) in a.iter_mut().zip(b).zip(c).zip(d).enumerate() {
                        let xv = vld1q_f32(xr.add(k * 4));
                        *a = vfmaq_laneq_f32::<0>(*a, xv, wv);
                        *b = vfmaq_laneq_f32::<1>(*b, xv, wv);
                        *c = vfmaq_laneq_f32::<2>(*c, xv, wv);
                        *d = vfmaq_laneq_f32::<3>(*d, xv, wv);
                    }
                }
            }
            for (t, acc) in acc.iter().enumerate() {
                for (r, row) in acc.iter().enumerate() {
                    let yr = yp.add(t * ys + r * f);
                    for (k, &v) in row.iter().enumerate() {
                        vst1q_f32(yr.add(k * 4), v);
                    }
                }
            }
        }
    }

    /// acc + x * w[lane]: the fused multiply-add of `pointwise_block_q8`,
    /// with the lane chosen by a loop index that is constant once unrolled.
    #[inline(always)]
    unsafe fn fma_lane(
        acc: float32x4_t,
        x: float32x4_t,
        w: float32x4_t,
        lane: usize,
    ) -> float32x4_t {
        // SAFETY: register-only NEON arithmetic.
        unsafe {
            match lane {
                0 => vfmaq_laneq_f32::<0>(acc, x, w),
                1 => vfmaq_laneq_f32::<1>(acc, x, w),
                2 => vfmaq_laneq_f32::<2>(acc, x, w),
                _ => vfmaq_laneq_f32::<3>(acc, x, w),
            }
        }
    }

    /// `pointwise_block_q8` for the first `R` (1 to 4) outputs of one block
    /// over `NV` vectors of bins, with the sums starting from what `yp`
    /// holds (`fresh` false) or from zero. Per output and bin the
    /// multiply-adds are those of `pointwise_block_q8`, in the same order;
    /// outputs past `R` are not computed.
    ///
    /// # Safety
    /// `wp` and `sp` cover `inp` inputs of the block; `xp` rows are `f`
    /// apart with `NV * 4` readable bins each; `yp` rows are `f` apart with
    /// `NV * 4` readable and writable bins each, `R` rows.
    #[inline(always)]
    pub(super) unsafe fn pointwise_onto_block<const NV: usize, const R: usize>(
        wp: *const i8,
        sp: *const f32,
        inp: usize,
        xp: *const f32,
        f: usize,
        yp: *mut f32,
        fresh: bool,
    ) {
        // SAFETY: as documented above.
        unsafe {
            let mut acc = [[vdupq_n_f32(0.); NV]; R];
            if !fresh {
                for (r, row) in acc.iter_mut().enumerate() {
                    for (k, v) in row.iter_mut().enumerate() {
                        *v = vld1q_f32(yp.add(r * f + k * 4));
                    }
                }
            }
            for i in 0..inp {
                let wv = w4_q8(wp.add(i * 4), *sp.add(i));
                let xr = xp.add(i * f);
                for k in 0..NV {
                    let xv = vld1q_f32(xr.add(k * 4));
                    for (r, row) in acc.iter_mut().enumerate() {
                        row[k] = fma_lane(row[k], xv, wv, r);
                    }
                }
            }
            for (r, row) in acc.iter().enumerate() {
                for (k, &v) in row.iter().enumerate() {
                    vst1q_f32(yp.add(r * f + k * 4), v);
                }
            }
        }
    }

    /// `pointwise_onto_block` for a block with `rows` (1 to 4) outputs.
    ///
    /// # Safety
    /// As `pointwise_onto_block::<NV, rows>`.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn pointwise_onto_rows<const NV: usize>(
        rows: usize,
        wp: *const i8,
        sp: *const f32,
        inp: usize,
        xp: *const f32,
        f: usize,
        yp: *mut f32,
        fresh: bool,
    ) {
        // SAFETY: the caller's guarantees for `rows` outputs.
        unsafe {
            match rows {
                1 => pointwise_onto_block::<NV, 1>(wp, sp, inp, xp, f, yp, fresh),
                2 => pointwise_onto_block::<NV, 2>(wp, sp, inp, xp, f, yp, fresh),
                3 => pointwise_onto_block::<NV, 3>(wp, sp, inp, xp, f, yp, fresh),
                _ => pointwise_onto_block::<NV, 4>(wp, sp, inp, xp, f, yp, fresh),
            }
        }
    }
}

/// Pointwise (1x1) convolution over `f` bins: y[o][f] = sum_i W[o][i] x[i][f].
/// `w` comes from `pack_pointwise` (`Weights::rows(.., 4)`); `out` is a
/// multiple of 4, `f` of 8.
pub(crate) fn pointwise(w: &Weights, out: usize, inp: usize, x: &[f32], f: usize, y: &mut [f32]) {
    debug_assert!(out.is_multiple_of(4) && f.is_multiple_of(8));
    debug_assert!(x.len() >= inp * f && y.len() >= out * f);
    // Every 16-bin block (and the 8-bin tail) re-reads its four outputs'
    // weights; every block of four outputs re-reads the whole input.
    #[cfg(test)]
    weights_traffic(w, out * inp, out / 4 * inp, f.div_ceil(16));
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch_n(Class::Act, x.as_ptr(), inp * f * 4, out / 4);
        store(Class::Act, y.as_ptr(), out * f * 4);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        debug_assert_eq!(w.row, 4);
        for ob in 0..out / 4 {
            let wp = w.q.as_ptr().add(ob * inp * 4);
            let sp = w.scale.as_ptr().add(ob * inp);
            let yp = y.as_mut_ptr().add(ob * 4 * f);
            let mut f0 = 0;
            while f0 + 16 <= f {
                neon::pointwise_block_q8::<4, 1>(
                    wp,
                    sp,
                    inp,
                    x.as_ptr().add(f0),
                    f,
                    yp.add(f0),
                    out * f,
                );
                f0 += 16;
            }
            if f0 < f {
                neon::pointwise_block_q8::<2, 1>(
                    wp,
                    sp,
                    inp,
                    x.as_ptr().add(f0),
                    f,
                    yp.add(f0),
                    out * f,
                );
            }
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        y[..out * f].fill(0.);
        for ob in 0..out / 4 {
            for i in 0..inp {
                for r in 0..4 {
                    let wv = w.vals[(ob * inp + i) * 4 + r];
                    let o = ob * 4 + r;
                    for k in 0..f {
                        y[o * f + k] += wv * x[i * f + k];
                    }
                }
            }
        }
    }
}

/// One run of a pointwise convolution's inputs, added to running sums:
/// y[o][f] += sum_i W[o][at + i] x[i][f] for `rows` outputs and inputs
/// `at .. at + inp` of the `total` that `w` was packed for (`pack_pointwise`
/// with the outputs padded to a multiple of four, `Weights::rows(.., 4)`).
/// `fresh` starts the sums from zero instead of from `y`. `f` is a multiple
/// of 8; x is [inp][f], y is [rows][f].
///
/// Per output and bin this performs the multiply-adds `pointwise` performs
/// for the same inputs, in the same order. Running `pointwise`'s inputs
/// through it in order, in any number of runs, the first of them fresh,
/// gives `pointwise`'s result bit for bit: the sums only pass through `y`
/// between runs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pointwise_onto(
    w: &Weights,
    rows: usize,
    total: usize,
    at: usize,
    inp: usize,
    x: &[f32],
    f: usize,
    y: &mut [f32],
    fresh: bool,
) {
    let blocks = rows.div_ceil(4);
    assert!(f.is_multiple_of(8) && at + inp <= total);
    assert!(x.len() >= inp * f && y.len() >= rows * f);
    // Every 16-bin block (and the 8-bin tail) re-reads the run's weights;
    // every block of four outputs re-reads the run's input and updates its
    // sums in place.
    #[cfg(test)]
    {
        use crate::traffic::*;
        let times = f.div_ceil(16);
        // Each run reads its own part of the weights and scales.
        let part = (w as *const Weights as *const u8).wrapping_add(at);
        touch_n(Class::Q8, part, blocks * inp * 4, times);
        touch_n(Class::Scale, part, blocks * inp * 4, times);
        touch_n(Class::Act, x.as_ptr(), inp * f * 4, blocks);
        if !fresh {
            touch(Class::State, y.as_ptr(), rows * f * 4);
        }
        store(Class::State, y.as_ptr(), rows * f * 4);
    }
    #[cfg(target_arch = "aarch64")]
    {
        debug_assert_eq!(w.row, 4);
        assert!(w.q.len() >= blocks * total * 4 && w.scale.len() >= blocks * total);
        // SAFETY: the asserts cover the weights and scales of every block's
        // inputs `at .. at + inp`, `inp` rows of `f` bins in x and `rows`
        // rows of `f` bins in y; bins advance in groups of 16, then 8.
        unsafe {
            for ob in 0..blocks {
                let wp = w.q.as_ptr().add((ob * total + at) * 4);
                let sp = w.scale.as_ptr().add(ob * total + at);
                let yp = y.as_mut_ptr().add(ob * 4 * f);
                let r = (rows - ob * 4).min(4);
                let mut f0 = 0;
                while f0 + 16 <= f {
                    let (xp, yp) = (x.as_ptr().add(f0), yp.add(f0));
                    neon::pointwise_onto_rows::<4>(r, wp, sp, inp, xp, f, yp, fresh);
                    f0 += 16;
                }
                if f0 < f {
                    let (xp, yp) = (x.as_ptr().add(f0), yp.add(f0));
                    neon::pointwise_onto_rows::<2>(r, wp, sp, inp, xp, f, yp, fresh);
                }
            }
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        assert!(w.vals.len() >= blocks * total * 4);
        if fresh {
            y[..rows * f].fill(0.);
        }
        for ob in 0..blocks {
            for i in 0..inp {
                for r in 0..(rows - ob * 4).min(4) {
                    let wv = w.vals[(ob * total + at + i) * 4 + r];
                    let o = ob * 4 + r;
                    for k in 0..f {
                        y[o * f + k] += wv * x[i * f + k];
                    }
                }
            }
        }
    }
}

/// Two independent pointwise inputs, each [inp][f], sharing weight loads.
/// Output steps are `ys` floats apart (at least out*f).
pub(crate) fn pointwise_pair(
    w: &Weights,
    out: usize,
    inp: usize,
    x: &[f32],
    f: usize,
    y: &mut [f32],
    ys: usize,
) {
    debug_assert!(out.is_multiple_of(4) && f.is_multiple_of(8));
    debug_assert!(x.len() >= 2 * inp * f && ys >= out * f && y.len() >= ys + out * f);
    #[cfg(test)]
    weights_traffic(w, out * inp, out / 4 * inp, f / 8);
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch_n(Class::Act, x.as_ptr(), 2 * inp * f * 4, out / 4);
        store(Class::Act, y.as_ptr(), 2 * out * f * 4);
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the slice lengths and the packed-weight layout cover every
    // block, both inputs, and both outputs; bins advance in groups of eight.
    unsafe {
        for ob in 0..out / 4 {
            let wp = w.q.as_ptr().add(ob * inp * 4);
            let yp = y.as_mut_ptr().add(ob * 4 * f);
            // 2 steps * 4 outputs * 2 vectors = 16 accumulator registers.
            for f0 in (0..f).step_by(8) {
                neon::pointwise_block_q8::<2, 2>(
                    wp,
                    w.scale.as_ptr().add(ob * inp),
                    inp,
                    x.as_ptr().add(f0),
                    f,
                    yp.add(f0),
                    ys,
                );
            }
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        y[..out * f].fill(0.);
        y[ys..ys + out * f].fill(0.);
        for ob in 0..out / 4 {
            for i in 0..inp {
                for r in 0..4 {
                    let wv = w.vals[(ob * inp + i) * 4 + r];
                    let o = ob * 4 + r;
                    for k in 0..f {
                        y[o * f + k] += wv * x[i * f + k];
                        y[ys + o * f + k] += wv * x[inp * f + i * f + k];
                    }
                }
            }
        }
    }
}

/// `Weights` traffic: `values` weights and, as int8, `scales` f32 scales,
/// each read `times` over.
#[cfg(test)]
fn weights_traffic(w: &Weights, values: usize, scales: usize, times: usize) {
    use crate::traffic::*;
    touch_n(Class::Q8, w, values, times);
    touch_n(Class::Scale, w, scales * 4, times);
}

/// Reorders a row-major [out][inp] matrix into blocks of four outputs per input.
pub(crate) fn pack_pointwise(w: &[f32], out: usize, inp: usize) -> Vec<f32> {
    let mut p = vec![0f32; out * inp];
    for ob in 0..out / 4 {
        for i in 0..inp {
            for r in 0..4 {
                p[(ob * inp + i) * 4 + r] = w[(ob * 4 + r) * inp + i];
            }
        }
    }
    p
}

/// Grouped linear: y[g*hs + h] = sum_i x[g*ws + i] W[g][i][h]. Any `hs`; the
/// NEON path takes four outputs at a time and finishes the rest one by one.
pub(crate) fn grouped(w: &Weights, groups: usize, ws: usize, hs: usize, x: &[f32], y: &mut [f32]) {
    grouped_steps::<1>(w, groups, ws, hs, x, y);
}

/// Two step-major grouped-linear inputs, sharing every weight load.
pub(crate) fn grouped_pair(
    w: &Weights,
    groups: usize,
    ws: usize,
    hs: usize,
    x: &[f32],
    y: &mut [f32],
) {
    grouped_steps::<2>(w, groups, ws, hs, x, y);
}

#[cfg(any(test, target_arch = "aarch64"))]
const fn grouped_registers_enabled<const N: usize>(hs: usize) -> bool {
    cfg!(target_arch = "aarch64") && N == 1 && matches!(hs, 8 | 16 | 32 | 40)
}

// N is monomorphised to one or two, so the two inputs use the same loaded
// and widened weight, without changing either input's accumulation order.
fn grouped_steps<const N: usize>(
    w: &Weights,
    groups: usize,
    ws: usize,
    hs: usize,
    x: &[f32],
    y: &mut [f32],
) {
    grouped_impl::<N, true>(w, groups, ws, hs, x, y);
}

// REGISTERS=false retains the actual original NEON path for bitwise tests.
fn grouped_impl<const N: usize, const REGISTERS: bool>(
    w: &Weights,
    groups: usize,
    ws: usize,
    hs: usize,
    x: &[f32],
    y: &mut [f32],
) {
    debug_assert!(x.len() >= N * groups * ws && y.len() >= N * groups * hs);
    // y is cleared, then every input row reads and writes its group's hs
    // outputs back.
    #[cfg(test)]
    weights_traffic(w, groups * ws * hs, groups * ws, 1);
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch(Class::Act, x.as_ptr(), N * groups * ws * 4);
        if REGISTERS && grouped_registers_enabled::<N>(hs) {
            store(Class::Act, y.as_ptr(), N * groups * hs * 4);
        } else {
            touch_n(Class::Act, y.as_ptr(), N * groups * hs * 4, ws);
            store_n(Class::Act, y.as_ptr(), N * groups * hs * 4, 1 + ws);
        }
    }
    #[cfg(target_arch = "aarch64")]
    if REGISTERS && grouped_registers_enabled::<N>(hs) {
        // SAFETY: dimensions are checked above; each supported width covers
        // complete eight-weight blocks and each group writes its own outputs.
        unsafe {
            match hs {
                8 => grouped_registers::<2>(w, groups, ws, x, y),
                16 => grouped_registers::<4>(w, groups, ws, x, y),
                32 => grouped_registers::<8>(w, groups, ws, x, y),
                40 => grouped_registers::<10>(w, groups, ws, x, y),
                _ => unreachable!(),
            }
        }
        return;
    }
    y[..N * groups * hs].fill(0.);
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        debug_assert_eq!(w.row, hs);
        let wide = hs - hs % 8;
        for g in 0..groups {
            let yp = y.as_mut_ptr().add(g * hs);
            for i in 0..ws {
                let r = g * ws + i;
                // The row's scale folds into the broadcast input.
                let xs: [f32; N] = std::array::from_fn(|t| {
                    *x.get_unchecked(t * groups * ws + r) * *w.scale.get_unchecked(r)
                });
                let wp = w.q.as_ptr().add(r * hs);
                let mut h = 0;
                while h < wide {
                    let q = vmovl_s8(vld1_s8(wp.add(h)));
                    let lo = vcvtq_f32_s32(vmovl_s16(vget_low_s16(q)));
                    let hi = vcvtq_f32_s32(vmovl_high_s16(q));
                    for (t, &xs) in xs.iter().enumerate() {
                        let p = yp.add(t * groups * hs + h);
                        let xv = vdupq_n_f32(xs);
                        vst1q_f32(p, vfmaq_f32(vld1q_f32(p), xv, lo));
                        vst1q_f32(p.add(4), vfmaq_f32(vld1q_f32(p.add(4)), xv, hi));
                    }
                    h += 8;
                }
                while h < hs {
                    let wv = *wp.add(h) as f32;
                    for (t, &xs) in xs.iter().enumerate() {
                        *yp.add(t * groups * hs + h) += xs * wv;
                    }
                    h += 1;
                }
            }
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    for g in 0..groups {
        for i in 0..ws {
            for h in 0..hs {
                let wv = w.vals[(g * ws + i) * hs + h];
                for t in 0..N {
                    y[t * groups * hs + g * hs + h] += x[t * groups * ws + g * ws + i] * wv;
                }
            }
        }
    }
}

/// Single-step grouped int8: keep all output vectors in registers while
/// visiting input rows in the same order as the original NEON implementation.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn grouped_registers<const V: usize>(
    w: &Weights,
    groups: usize,
    ws: usize,
    x: &[f32],
    y: &mut [f32],
) {
    use std::arch::aarch64::*;
    let hs = V * 4;
    debug_assert_eq!(w.row, hs);
    unsafe {
        for g in 0..groups {
            let mut acc = [vdupq_n_f32(0.); V];
            for i in 0..ws {
                let r = g * ws + i;
                // The multiplication must round before the same FMA as the
                // baseline; folding the scale into weights would change it.
                let xs = *x.get_unchecked(r) * *w.scale.get_unchecked(r);
                let xv = vdupq_n_f32(xs);
                let wp = w.q.as_ptr().add(r * hs);
                for b in 0..V / 2 {
                    let q = vmovl_s8(vld1_s8(wp.add(b * 8)));
                    let lo = vcvtq_f32_s32(vmovl_s16(vget_low_s16(q)));
                    let hi = vcvtq_f32_s32(vmovl_high_s16(q));
                    acc[2 * b] = vfmaq_f32(acc[2 * b], xv, lo);
                    acc[2 * b + 1] = vfmaq_f32(acc[2 * b + 1], xv, hi);
                }
            }
            for (v, value) in acc.into_iter().enumerate() {
                vst1q_f32(y.as_mut_ptr().add(g * hs + v * 4), value);
            }
        }
    }
}

/// A matrix stored as int8 with one f32 scale per row (`w = q * scale`,
/// symmetric, `|q| <= 127`): half the bytes of f16, for the GRU matrices that
/// make up most of the weights streamed per frame.
///
/// The bytes are tiled for `matvec_q8`. Rows are taken four at a time; the
/// four rows' bytes for columns 0 to 15 come first (row by row, 64 bytes, one
/// cache line), then their bytes for columns 16 to 31, and so on. Rows past
/// the last complete group of four follow row-major. A matrix takes exactly
/// the bytes it takes row-major, and a product still reads them front to
/// back as one stream.
pub(crate) struct QWeights {
    q: Vec<i8>,
    scale: Vec<f32>,
    cols: usize,
    /// The same bytes row-major, for `matvec_q8_rows`. Tests and bench
    /// builds only.
    #[cfg(any(test, feature = "bench-kernels"))]
    rows: Vec<i8>,
}

/// Where the tiled layout keeps row-major element (`r`, `c`); `grouped` is
/// the number of rows in complete groups of four.
#[cfg(test)]
fn tiled(r: usize, c: usize, cols: usize, grouped: usize) -> usize {
    if r < grouped {
        (r & !3) * cols + (c & !15) * 4 + (r & 3) * 16 + (c & 15)
    } else {
        r * cols + c
    }
}

impl QWeights {
    /// Quantises a row-major `rows` x `cols` matrix (`cols` a multiple of
    /// 16) straight into the tiled layout: one pass and one allocation.
    pub(crate) fn new(w: &[f32], rows: usize, cols: usize) -> Self {
        assert!(cols.is_multiple_of(16) && w.len() == rows * cols);
        let grouped = rows / 4 * 4;
        let mut q = vec![0i8; rows * cols];
        #[cfg(any(test, feature = "bench-kernels"))]
        let mut row_major = Vec::with_capacity(rows * cols);
        let mut scale = Vec::with_capacity(rows);
        // One quantised row at a time, then its 16-column pieces into their
        // tiles.
        let mut line = vec![0i8; cols];
        for (r, row) in w.chunks_exact(cols).enumerate() {
            let max = row.iter().fold(0f32, |m, &x| m.max(x.abs()));
            let s = if max > 0. { max / 127. } else { 1. };
            scale.push(s);
            for (o, &x) in line.iter_mut().zip(row) {
                *o = (x / s).round().clamp(-127., 127.) as i8;
            }
            if r < grouped {
                let group = &mut q[(r & !3) * cols..][..4 * cols];
                let tiles = group.as_chunks_mut::<64>().0;
                for (tile, piece) in tiles.iter_mut().zip(line.as_chunks::<16>().0) {
                    tile[(r & 3) * 16..][..16].copy_from_slice(piece);
                }
            } else {
                q[r * cols..(r + 1) * cols].copy_from_slice(&line);
            }
            #[cfg(any(test, feature = "bench-kernels"))]
            row_major.extend_from_slice(&line);
        }
        Self {
            q,
            scale,
            cols,
            #[cfg(any(test, feature = "bench-kernels"))]
            rows: row_major,
        }
    }
}

#[cfg(test)]
impl QWeights {
    /// The int8 values, row-major.
    pub(crate) fn int8(&self) -> &[i8] {
        &self.rows
    }

    /// A matrix from int8 values and scales as given, so a test can hold
    /// values the quantiser never produces (-128).
    fn from_int8(q: &[i8], scale: &[f32], cols: usize) -> Self {
        let rows = scale.len();
        assert!(cols.is_multiple_of(16) && q.len() == rows * cols);
        let grouped = rows / 4 * 4;
        let mut tiles = vec![0i8; rows * cols];
        for (i, &v) in q.iter().enumerate() {
            tiles[tiled(i / cols, i % cols, cols, grouped)] = v;
        }
        Self {
            q: tiles,
            scale: scale.to_vec(),
            cols,
            rows: q.to_vec(),
        }
    }

    /// Weight and scale bytes one product reads.
    fn stream_bytes(&self, rows: usize) -> (usize, usize) {
        (rows * self.cols, rows * 4)
    }
}

/// `x` quantised to 12-bit integers with one scale: the largest magnitude maps
/// to 2047, so a 1024-column int8 dot product stays inside i32.
fn quantize_x(x: &[f32], out: &mut [i16]) -> f32 {
    let max = x.iter().fold(0f32, |m, &v| m.max(v.abs()));
    let s = if max > 0. { max / 2047. } else { 1. };
    let inv = 1. / s;
    for (o, &v) in out.iter_mut().zip(x) {
        *o = (v * inv).round() as i16;
    }
    s
}

/// y = W x for an int8 matrix of `rows` rows (all of `w`'s; `cols` a
/// multiple of 16, at most 1024). The input is quantised to 12 bits once; the
/// products accumulate in i32 and are scaled back at the end of each row.
pub(crate) fn matvec_q8(w: &QWeights, rows: usize, x: &[f32], xq: &mut [i16], y: &mut [f32]) {
    matvec_q8_steps::<1>(w, rows, x, xq, y);
}

/// Two independent inputs, quantised separately, sharing each int8 load.
pub(crate) fn matvec_q8_pair(w: &QWeights, rows: usize, x: &[f32], xq: &mut [i16], y: &mut [f32]) {
    matvec_q8_steps::<2>(w, rows, x, xq, y);
}

/// Whether `matvec_q8` runs the retained row-major kernel instead of the
/// tiled one: under the tests' `reference` switch, and in a bench build when
/// the bench has selected it. Production builds have neither.
#[cfg(any(test, feature = "bench-kernels"))]
#[inline(always)]
fn row_major_selected() -> bool {
    #[cfg(test)]
    let test = crate::reference::on();
    #[cfg(not(test))]
    let test = false;
    #[cfg(feature = "bench-kernels")]
    let bench = crate::bench_kernels::row_major_q8();
    #[cfg(not(feature = "bench-kernels"))]
    let bench = false;
    test || bench
}

/// The tiled kernel. Each row's sum is the sum, modulo 2^32, of the same
/// `cols` exact i8 x i16 products the row-major kernel adds, and it is scaled
/// by the same two f32 multiplications, so every result has the same bits
/// (`tiled_matvec_matches_the_row_major_kernel_bit_for_bit`).
fn matvec_q8_steps<const N: usize>(
    w: &QWeights,
    rows: usize,
    x: &[f32],
    xq: &mut [i16],
    y: &mut [f32],
) {
    crate::hold::gate();
    #[cfg(any(test, feature = "bench-kernels"))]
    if row_major_selected() {
        return matvec_q8_rows::<N>(w, rows, x, xq, y);
    }
    let cols = w.cols;
    // The tiles are laid out for all of the matrix's rows.
    assert!(rows == w.scale.len() && w.q.len() == rows * cols);
    assert!(cols.is_multiple_of(16) && cols <= 1024);
    assert!(x.len() >= N * cols && xq.len() >= N * cols && y.len() >= N * rows);
    // x is quantised once into xq, which every group of rows re-reads.
    #[cfg(test)]
    {
        use crate::traffic::*;
        let (weights, scales) = w.stream_bytes(rows);
        touch(Class::Q8, w, weights);
        touch(Class::Scale, w, scales);
        touch(Class::Act, x.as_ptr(), N * cols * 4);
        store(Class::Act, xq.as_ptr(), N * cols * 2);
        touch_n(Class::Act, xq.as_ptr(), N * cols * 2, rows.div_ceil(4));
        store(Class::Act, y.as_ptr(), N * rows * 4);
    }
    let xs: [f32; N] = std::array::from_fn(|t| {
        quantize_x(
            &x[t * cols..(t + 1) * cols],
            &mut xq[t * cols..(t + 1) * cols],
        )
    });
    let grouped = rows / 4 * 4;
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the asserts above cover every load and store. The tile pointer
    // advances 64 bytes per 16 columns of a group, `grouped * cols` bytes in
    // all, and the rows after that are read row-major up to `rows * cols`;
    // xq holds `cols` values per step and y `rows` per step.
    unsafe {
        use std::arch::aarch64::*;
        let mut tile = w.q.as_ptr();
        for g in (0..grouped).step_by(4) {
            // [step][row][2]: two accumulators per row, as the row-major
            // kernel keeps.
            let mut acc = [[[vdupq_n_s32(0); 2]; 4]; N];
            let mut i = 0;
            while i < cols {
                let [w0, w1, w2, w3] = [0, 16, 32, 48].map(|o| vld1q_s8(tile.add(o)));
                tile = tile.add(64);
                // Columns 0 to 7 of the four rows, then columns 8 to 15, so
                // that the two additions to one accumulator are eight
                // multiply-accumulates apart.
                let lo = [w0, w1, w2, w3].map(|v| vmovl_s8(vget_low_s8(v)));
                let hi = [w0, w1, w2, w3].map(|v| vmovl_high_s8(v));
                for (t, acc) in acc.iter_mut().enumerate() {
                    let xp = xq.as_ptr().add(t * cols + i);
                    let x01 = vld1q_s16(xp);
                    let x23 = vld1q_s16(xp.add(8));
                    for (acc, w01) in acc.iter_mut().zip(lo) {
                        acc[0] = vmlal_s16(acc[0], vget_low_s16(w01), vget_low_s16(x01));
                        acc[1] = vmlal_high_s16(acc[1], w01, x01);
                    }
                    for (acc, w23) in acc.iter_mut().zip(hi) {
                        acc[0] = vmlal_s16(acc[0], vget_low_s16(w23), vget_low_s16(x23));
                        acc[1] = vmlal_high_s16(acc[1], w23, x23);
                    }
                }
                i += 16;
            }
            // The four rows' sums in one vector (pairwise adds wrap as the
            // row-major kernel's across-vector add does), converted and
            // scaled lane by lane with the scalar expression's operations:
            // sum as f32 * (scale * xs).
            let scale = vld1q_f32(w.scale.as_ptr().add(g));
            for (t, [a, b, c, d]) in acc.into_iter().enumerate() {
                let ab = vpaddq_s32(vaddq_s32(a[0], a[1]), vaddq_s32(b[0], b[1]));
                let cd = vpaddq_s32(vaddq_s32(c[0], c[1]), vaddq_s32(d[0], d[1]));
                let sums = vcvtq_f32_s32(vpaddq_s32(ab, cd));
                let by = vmulq_f32(scale, vdupq_n_f32(xs[t]));
                vst1q_f32(y.as_mut_ptr().add(t * rows + g), vmulq_f32(sums, by));
            }
        }
        for r in grouped..rows {
            let row = w.q.as_ptr().add(r * cols);
            let mut acc = [[vdupq_n_s32(0); 2]; N];
            let mut i = 0;
            while i < cols {
                let wv = vld1q_s8(row.add(i));
                let w01 = vmovl_s8(vget_low_s8(wv));
                let w23 = vmovl_high_s8(wv);
                for (t, acc) in acc.iter_mut().enumerate() {
                    let xp = xq.as_ptr().add(t * cols + i);
                    let x01 = vld1q_s16(xp);
                    let x23 = vld1q_s16(xp.add(8));
                    acc[0] = vmlal_s16(acc[0], vget_low_s16(w01), vget_low_s16(x01));
                    acc[1] = vmlal_high_s16(acc[1], w01, x01);
                    acc[0] = vmlal_s16(acc[0], vget_low_s16(w23), vget_low_s16(x23));
                    acc[1] = vmlal_high_s16(acc[1], w23, x23);
                }
                i += 16;
            }
            let scale = *w.scale.get_unchecked(r);
            for (t, [a, b]) in acc.into_iter().enumerate() {
                let sum = vaddvq_s32(vaddq_s32(a, b));
                *y.get_unchecked_mut(t * rows + r) = sum as f32 * (scale * xs[t]);
            }
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // The NEON sums wrap; these wrap too. Sixteen running sums per row,
        // one per column of a tile, so the compiler can keep them in vector
        // registers; any grouping of a wrapping sum gives the same total.
        for g in (0..grouped).step_by(4) {
            let mut acc = [[[0i32; 16]; 4]; N];
            let tiles = w.q[g * cols..(g + 4) * cols].as_chunks::<64>().0;
            for (b, tile) in tiles.iter().enumerate() {
                for (t, acc) in acc.iter_mut().enumerate() {
                    let xq = xq[t * cols + b * 16..].first_chunk::<16>().unwrap();
                    for (acc, wq) in acc.iter_mut().zip(tile.as_chunks::<16>().0) {
                        for ((a, &wv), &xv) in acc.iter_mut().zip(wq).zip(xq) {
                            *a = a.wrapping_add(wv as i32 * xv as i32);
                        }
                    }
                }
            }
            for (t, acc) in acc.into_iter().enumerate() {
                for (k, acc) in acc.into_iter().enumerate() {
                    let sum = acc.into_iter().fold(0i32, i32::wrapping_add);
                    y[t * rows + g + k] = sum as f32 * (w.scale[g + k] * xs[t]);
                }
            }
        }
        for r in grouped..rows {
            for (t, &xs) in xs.iter().enumerate() {
                let sum = w.q[r * cols..(r + 1) * cols]
                    .iter()
                    .zip(&xq[t * cols..][..cols])
                    .fold(0i32, |a, (&wv, &xv)| a.wrapping_add(wv as i32 * xv as i32));
                y[t * rows + r] = sum as f32 * (w.scale[r] * xs);
            }
        }
    }
}

/// The row-major kernel that `matvec_q8` was until 2026-11-10, unchanged
/// apart from reading the row-major copy of the weights: one row at a time,
/// 16 columns per iteration. Tests compare the tiled kernel with it, and a
/// bench build can run it instead, to time the two on the mixer.
#[cfg(any(test, feature = "bench-kernels"))]
fn matvec_q8_rows<const N: usize>(
    w: &QWeights,
    rows: usize,
    x: &[f32],
    xq: &mut [i16],
    y: &mut [f32],
) {
    let cols = w.cols;
    debug_assert!(cols.is_multiple_of(16) && cols <= 1024);
    debug_assert!(x.len() >= N * cols && xq.len() >= N * cols && y.len() >= N * rows);
    // x is quantised once into xq, which every row re-reads.
    #[cfg(test)]
    {
        use crate::traffic::*;
        let (weights, scales) = w.stream_bytes(rows);
        touch(Class::Q8, w, weights);
        touch(Class::Scale, w, scales);
        touch(Class::Act, x.as_ptr(), N * cols * 4);
        store(Class::Act, xq.as_ptr(), N * cols * 2);
        touch_n(Class::Act, xq.as_ptr(), N * cols * 2, rows);
        store(Class::Act, y.as_ptr(), N * rows * 4);
    }
    let xs: [f32; N] = std::array::from_fn(|t| {
        quantize_x(
            &x[t * cols..(t + 1) * cols],
            &mut xq[t * cols..(t + 1) * cols],
        )
    });
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        for r in 0..rows {
            let row = w.rows.as_ptr().add(r * cols);
            let mut acc = [[vdupq_n_s32(0); 2]; N];
            let mut i = 0;
            while i < cols {
                let wv = vld1q_s8(row.add(i));
                let w01 = vmovl_s8(vget_low_s8(wv));
                let w23 = vmovl_high_s8(wv);
                for (t, acc) in acc.iter_mut().enumerate() {
                    let xp = xq.as_ptr().add(t * cols + i);
                    let x01 = vld1q_s16(xp);
                    let x23 = vld1q_s16(xp.add(8));
                    acc[0] = vmlal_s16(acc[0], vget_low_s16(w01), vget_low_s16(x01));
                    acc[1] = vmlal_high_s16(acc[1], w01, x01);
                    acc[0] = vmlal_s16(acc[0], vget_low_s16(w23), vget_low_s16(x23));
                    acc[1] = vmlal_high_s16(acc[1], w23, x23);
                }
                i += 16;
            }
            let scale = *w.scale.get_unchecked(r);
            for (t, [a, b]) in acc.into_iter().enumerate() {
                let sum = vaddvq_s32(vaddq_s32(a, b));
                *y.get_unchecked_mut(t * rows + r) = sum as f32 * (scale * xs[t]);
            }
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    for (r, row) in w.rows.chunks_exact(cols).take(rows).enumerate() {
        let mut acc = [0i32; N];
        for (i, &wv) in row.iter().enumerate() {
            for (t, acc) in acc.iter_mut().enumerate() {
                *acc += wv as i32 * xq[t * cols + i] as i32;
            }
        }
        for (t, acc) in acc.into_iter().enumerate() {
            y[t * rows + r] = acc as f32 * (w.scale[r] * xs[t]);
        }
    }
}

/// See `bench_kernels::resident_product_ns`.
#[cfg(feature = "bench-kernels")]
pub(crate) fn bench_resident_products(iterations: usize) -> [f64; 2] {
    let (rows, cols) = (384, 128);
    let values = |n: usize, k: usize| -> Vec<f32> {
        (0..n).map(|i| ((i * k % 101) as f32 - 50.) / 67.).collect()
    };
    let w = QWeights::new(&values(rows * cols, 17), rows, cols);
    let x = values(cols, 29);
    let mut xq = vec![0i16; cols];
    let mut out = [vec![0f32; rows], vec![0f32; rows]];
    let mut ns = [0f64; 2];
    for (k, y) in out.iter_mut().enumerate() {
        let run = |y: &mut [f32], xq: &mut [i16]| {
            let (w, x) = (std::hint::black_box(&w), std::hint::black_box(&x));
            if k == 0 {
                matvec_q8_rows::<1>(w, rows, x, xq, y);
            } else {
                matvec_q8(w, rows, x, xq, y);
            }
        };
        // Once to bring the weights into the cache, then the timed runs.
        run(y, &mut xq);
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            run(y, &mut xq);
        }
        ns[k] = start.elapsed().as_nanos() as f64 / iterations.max(1) as f64;
    }
    assert!(
        out[0]
            .iter()
            .zip(&out[1])
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "the tiled and row-major kernels disagree"
    );
    ns
}

// Cephes-style expf: x = n ln2 + r, exp(r) by a degree-5 polynomial, scaled by
// 2^n through the exponent bits. About 1-2 ulp over the clamped range. The
// scalar version is the same algorithm, so the PC check covers it.
const EXP_HI: f32 = 88.37626;
const EXP_LO: f32 = -88.37626;
const LOG2E: f32 = std::f32::consts::LOG2_E;
const LN2_HI: f32 = 0.693_359_4;
const LN2_LO: f32 = -2.121_944_4e-4;
const P: [f32; 6] = [
    1.987_569_1e-4,
    1.398_199_9e-3,
    8.333_452e-3,
    4.166_579_6e-2,
    1.666_666_6e-1,
    5e-1,
];

fn exp_scalar(x: f32) -> f32 {
    let x = x.clamp(EXP_LO, EXP_HI);
    let n = (x * LOG2E).round();
    let r = x - n * LN2_HI - n * LN2_LO;
    let mut p = P[0];
    for c in &P[1..] {
        p = p * r + c;
    }
    let y = p * r * r + r + 1.;
    f32::from_bits((y.to_bits() as i32 + ((n as i32) << 23)) as u32)
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn exp4(x: std::arch::aarch64::float32x4_t) -> std::arch::aarch64::float32x4_t {
    use std::arch::aarch64::*;
    // SAFETY: register-only NEON arithmetic; NEON is part of the aarch64
    // baseline.
    unsafe {
        let x = vminq_f32(vmaxq_f32(x, vdupq_n_f32(EXP_LO)), vdupq_n_f32(EXP_HI));
        let n = vrndnq_f32(vmulq_f32(x, vdupq_n_f32(LOG2E)));
        let r = vfmsq_f32(vfmsq_f32(x, n, vdupq_n_f32(LN2_HI)), n, vdupq_n_f32(LN2_LO));
        let mut p = vdupq_n_f32(P[0]);
        for c in &P[1..] {
            p = vfmaq_f32(vdupq_n_f32(*c), p, r);
        }
        let y = vaddq_f32(vfmaq_f32(r, p, vmulq_f32(r, r)), vdupq_n_f32(1.));
        vreinterpretq_f32_s32(vaddq_s32(
            vreinterpretq_s32_f32(y),
            vshlq_n_s32::<23>(vcvtq_s32_f32(n)),
        ))
    }
}

/// In place: x = 1 / (1 + exp(-x)).
pub(crate) fn sigmoid_slice(x: &mut [f32]) {
    #[cfg(target_arch = "aarch64")]
    let i = unsafe {
        use std::arch::aarch64::*;
        let one = vdupq_n_f32(1.);
        let mut i = 0;
        while i + 4 <= x.len() {
            let p = x.as_mut_ptr().add(i);
            let e = exp4(vnegq_f32(vld1q_f32(p)));
            vst1q_f32(p, vdivq_f32(one, vaddq_f32(one, e)));
            i += 4;
        }
        i
    };
    #[cfg(not(target_arch = "aarch64"))]
    let i = 0;
    for v in &mut x[i..] {
        *v = 1. / (1. + exp_scalar(-*v));
    }
}

/// In place: x = tanh(x) = 1 - 2 / (exp(2x) + 1).
pub(crate) fn tanh_slice(x: &mut [f32]) {
    #[cfg(target_arch = "aarch64")]
    let i = unsafe {
        use std::arch::aarch64::*;
        let (one, two) = (vdupq_n_f32(1.), vdupq_n_f32(2.));
        let mut i = 0;
        while i + 4 <= x.len() {
            let p = x.as_mut_ptr().add(i);
            let e = exp4(vmulq_f32(vld1q_f32(p), two));
            vst1q_f32(p, vsubq_f32(one, vdivq_f32(two, vaddq_f32(e, one))));
            i += 4;
        }
        i
    };
    #[cfg(not(target_arch = "aarch64"))]
    let i = 0;
    for v in &mut x[i..] {
        *v = 1. - 2. / (exp_scalar(2. * *v) + 1.);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| ((i * 17 % 101) as f32 - 50.) / 67.)
            .collect()
    }

    fn same_bits(a: &[f32], b: &[f32]) {
        assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn paired_matvec_matches_separate_inputs() {
        for (rows, cols) in [(12, 16), (48, 128), (24, 1024)] {
            let weights = values(rows * cols);
            let x = values(2 * cols);
            let mut expected = vec![0.; 2 * rows];
            let mut actual = expected.clone();
            {
                let w = QWeights::new(&weights, rows, cols);
                let mut scratch = vec![0; 2 * cols];
                matvec_q8(&w, rows, &x[..cols], &mut scratch, &mut expected[..rows]);
                matvec_q8(&w, rows, &x[cols..], &mut scratch, &mut expected[rows..]);
                matvec_q8_pair(&w, rows, &x, &mut scratch, &mut actual);
            }
            same_bits(&actual, &expected);
        }
    }

    /// The int8 kernel against
    /// a scalar reference, exactly: 32-byte loads, the 16-column tail (cols an
    /// odd multiple of 16) and the paired inputs.
    #[test]
    fn int8_matvec_matches_a_scalar_reference_bit_for_bit() {
        for (rows, cols) in [(5, 16), (7, 48), (12, 96), (9, 272), (24, 1024)] {
            let weights = values(rows * cols);
            let w = QWeights::new(&weights, rows, cols);
            let (q, scale) = (w.int8(), &w.scale);
            let x: Vec<f32> = values(2 * cols + 3)[3..].to_vec();
            let mut xq = vec![0; 2 * cols];
            let mut pair = vec![0.; 2 * rows];
            matvec_q8_pair(&w, rows, &x, &mut xq, &mut pair);
            for t in 0..2 {
                let input = &x[t * cols..][..cols];
                let mut single = vec![0.; rows];
                matvec_q8(&w, rows, input, &mut xq, &mut single);
                let mut xi = vec![0i16; cols];
                let xs = quantize_x(input, &mut xi);
                for r in 0..rows {
                    let sum: i32 = (0..cols)
                        .map(|j| q[r * cols + j] as i32 * xi[j] as i32)
                        .sum();
                    let expected = sum as f32 * (scale[r] * xs);
                    assert_eq!(
                        single[r].to_bits(),
                        expected.to_bits(),
                        "{rows}x{cols} row {r}"
                    );
                    assert_eq!(
                        pair[t * rows + r].to_bits(),
                        expected.to_bits(),
                        "pair {t} row {r}"
                    );
                }
            }
        }
    }

    #[test]
    fn q8_integer_bound_is_explicit() {
        assert!(127_i64 * 2047 * 1024 < i32::MAX as i64);
        // This is an intended-range bound, not a universal quantizer claim:
        // subnormal scaling can underflow and saturate the i16 cast.
        let mut q = [0i16; 1];
        quantize_x(&[f32::from_bits(1)], &mut q);
        assert_eq!(q[0], i16::MAX);
        quantize_x(&[-f32::from_bits(1)], &mut q);
        assert_eq!(q[0], i16::MIN);
    }

    #[test]
    fn q8_matches_scalar_bits_on_shapes_and_inputs() {
        for rows in [1, 3, 4, 5, 7, 12, 384] {
            for cols in [16, 32, 48, 128, 272, 1024] {
                let w = QWeights::new(&values(rows * cols), rows, cols);
                for mode in 0..4 {
                    let x: Vec<f32> = (0..2 * cols)
                        .map(|i| match mode {
                            0 => 0.0,
                            1 => {
                                if i % 2 == 0 {
                                    1e20
                                } else {
                                    -1e20
                                }
                            }
                            2 => {
                                if i % 3 == 0 {
                                    1e-20
                                } else {
                                    -1e-20
                                }
                            }
                            _ => ((i * 17 % 101) as f32 - 50.) / 67.,
                        })
                        .collect();
                    let mut actual = vec![0.; 2 * rows];
                    let mut scratch = vec![0i16; 2 * cols];
                    matvec_q8_pair(&w, rows, &x, &mut scratch, &mut actual);
                    for t in 0..2 {
                        let mut quantized = vec![0i16; cols];
                        let xs = quantize_x(&x[t * cols..(t + 1) * cols], &mut quantized);
                        assert!(quantized.iter().all(|q| q.unsigned_abs() <= 2047));
                        let mut single = vec![0.; rows];
                        matvec_q8(
                            &w,
                            rows,
                            &x[t * cols..(t + 1) * cols],
                            &mut scratch,
                            &mut single,
                        );
                        for r in 0..rows {
                            let sum: i32 = (0..cols)
                                .map(|i| w.rows[r * cols + i] as i32 * quantized[i] as i32)
                                .sum();
                            let expected = (sum as f32 * (w.scale[r] * xs)).to_bits();
                            assert_eq!(
                                single[r].to_bits(),
                                expected,
                                "single {rows}x{cols} mode={mode} row={r}"
                            );
                            assert_eq!(
                                actual[t * rows + r].to_bits(),
                                expected,
                                "pair {rows}x{cols} mode={mode} step={t} row={r}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The NEON baseline accumulates modulo 2^32, even when subnormal input
    /// scaling saturates i16 and invalidates the normal 12-bit sum bound.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn q8_subnormal_saturation_matches_wrapping_reference() {
        let (rows, cols) = (5, 1024);
        let weights: Vec<f32> = (0..rows * cols)
            .map(|i| if i / cols % 2 == 0 { 1. } else { -1. })
            .collect();
        let w = QWeights::new(&weights, rows, cols);
        for value in [f32::from_bits(1), -f32::from_bits(1)] {
            let x = vec![value; 2 * cols];
            let mut scratch = vec![0; 2 * cols];
            let mut single = vec![0.; rows];
            let mut pair = vec![0.; 2 * rows];
            matvec_q8(&w, rows, &x[..cols], &mut scratch, &mut single);
            matvec_q8_pair(&w, rows, &x, &mut scratch, &mut pair);
            let mut q = vec![0; cols];
            let scale = quantize_x(&x[..cols], &mut q);
            assert_eq!(scale, 0.0);
            assert_eq!(
                q[0],
                if value.is_sign_positive() {
                    i16::MAX
                } else {
                    i16::MIN
                }
            );
            for r in 0..rows {
                let sum = (0..cols).fold(0i32, |acc, i| {
                    acc.wrapping_add(w.rows[r * cols + i] as i32 * q[i] as i32)
                });
                let expected = (sum as f32 * (w.scale[r] * scale)).to_bits();
                assert_eq!(single[r].to_bits(), expected, "single row={r}");
                assert_eq!(pair[r].to_bits(), expected, "pair 0 row={r}");
                assert_eq!(pair[rows + r].to_bits(), expected, "pair 1 row={r}");
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn q8_nonfinite_scaling_matches_baseline_bits() {
        let (rows, cols) = (5, 16);
        let w = QWeights::new(&values(rows * cols), rows, cols);
        for value in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            let x = vec![value; 2 * cols];
            let mut q = vec![0; cols];
            let scale = quantize_x(&x[..cols], &mut q);
            let mut scratch = vec![0; 2 * cols];
            let mut single = vec![0.; rows];
            let mut pair = vec![0.; 2 * rows];
            matvec_q8(&w, rows, &x[..cols], &mut scratch, &mut single);
            matvec_q8_pair(&w, rows, &x, &mut scratch, &mut pair);
            for r in 0..rows {
                let sum = (0..cols).fold(0i32, |acc, i| {
                    acc.wrapping_add(w.rows[r * cols + i] as i32 * q[i] as i32)
                });
                let expected = (sum as f32 * (w.scale[r] * scale)).to_bits();
                assert_eq!(single[r].to_bits(), expected, "single row={r}");
                assert_eq!(pair[r].to_bits(), expected, "pair 0 row={r}");
                assert_eq!(pair[rows + r].to_bits(), expected, "pair 1 row={r}");
            }
        }
    }

    /// Runs the GRU's 384 x 128 product `RC2FX_Q8_ITERATIONS` times (10,000
    /// by default) with the tiled kernel, or with the row-major one when
    /// `RC2FX_Q8_KERNEL=rows`. Under QEMU the difference between two
    /// iteration counts gives the instructions per product; the time it
    /// prints is a host or QEMU figure, not the mixer's.
    #[test]
    #[ignore = "explicit local kernel timing observation"]
    fn q8_kernel_benchmark() {
        let (rows, cols) = (384, 128);
        let iterations = std::env::var("RC2FX_Q8_ITERATIONS")
            .map_or(10_000, |n| n.parse().expect("RC2FX_Q8_ITERATIONS"));
        let row_major = std::env::var("RC2FX_Q8_KERNEL").is_ok_and(|k| k == "rows");
        let w = QWeights::new(&values(rows * cols), rows, cols);
        let x = values(cols);
        let mut scratch = vec![0; cols];
        let mut y = vec![0.; rows];
        crate::reference::set(row_major);
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            matvec_q8(
                std::hint::black_box(&w),
                rows,
                std::hint::black_box(&x),
                &mut scratch,
                &mut y,
            );
            std::hint::black_box(&y);
        }
        let elapsed = start.elapsed();
        crate::reference::set(false);
        eprintln!(
            "q8 matvec {rows}x{cols} ({}), {iterations} products, ns/call={:.1}",
            if row_major { "row-major" } else { "tiled" },
            elapsed.as_nanos() as f64 / iterations.max(1) as f64
        );
    }

    /// One class of int8 weights: 0 every value including -128, 1 the
    /// extremes only, 2 mostly zero with stray extremes, 3 what the quantiser
    /// makes of ordinary floats (handled by the caller).
    fn int8_weights(rng: &mut crate::glue::tests::Rng, class: usize, n: usize) -> Vec<i8> {
        (0..n)
            .map(|_| match (class, rng.next() % 16) {
                (0, _) => rng.next() as i8,
                (1, k) => [-128i8, -127, 127, 127][k as usize % 4],
                (_, 0) => [-128i8, 127][(rng.next() & 1) as usize],
                (_, _) => 0,
            })
            .collect()
    }

    /// One class of input for one step: 0 ordinary, 1 every element at the
    /// largest magnitude (quantises to +-2047), 2 subnormals (the scaling
    /// overflows and the i16 cast saturates, so the sums wrap), 3 zero, 4
    /// huge and tiny magnitudes, 5 infinities and NaNs among finite values.
    fn step_input(rng: &mut crate::glue::tests::Rng, class: usize, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| {
                let sign = if rng.next() & 1 == 0 { 1. } else { -1. };
                match class {
                    0 => rng.unit(),
                    1 => 0.75 * sign,
                    2 => f32::from_bits(1 + rng.next() % 3) * sign,
                    3 => 0. * sign,
                    4 => rng.value(3),
                    _ => rng.value(4),
                }
            })
            .collect()
    }

    /// The tiled kernel against the row-major kernel it replaced
    /// (`matvec_q8_rows`, kept unchanged), every output bit, NaN payloads
    /// included: the model's 384 x 128 matrices and every other shape class
    /// (whole groups of four rows, one to three rows left over, fewer than
    /// four rows, 16 to 1024 columns), with random, extreme and sparse int8
    /// weights and quantised float weights, ordinary to subnormal and huge
    /// row scales, six classes of input, one step and two, under each float
    /// mode. Nothing outside the output is written.
    #[test]
    fn tiled_matvec_matches_the_row_major_kernel_bit_for_bit() {
        use crate::glue::tests::{Rng, each_float_mode};
        const GUARD: usize = 7;
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        // The row-major kernel's host form adds with overflow checks in a
        // debug build, so the inputs that make the sums wrap are compared
        // where both kernels wrap: on AArch64, and in release builds.
        let wraps = cfg!(any(target_arch = "aarch64", not(debug_assertions)));
        each_float_mode(|mode| {
            let mut rng = Rng(0x51ed_270b_a5a5_0001);
            let shapes = [
                (384, 128),
                (1, 16),
                (2, 48),
                (3, 128),
                (4, 16),
                (5, 32),
                (6, 272),
                (7, 128),
                (8, 1024),
                (9, 64),
                (12, 128),
                (63, 1024),
                (387, 128),
            ];
            for (rows, cols) in shapes {
                for weight_class in 0..4 {
                    let scale: Vec<f32> = (0..rows)
                        .map(|_| match rng.next() % 8 {
                            0 => f32::from_bits(1 + rng.next() % 0x7f_ffff),
                            1 => 1e30,
                            2 => 1e-30,
                            _ => (rng.next() % 1000 + 1) as f32 * 1e-5,
                        })
                        .collect();
                    let w = if weight_class == 3 {
                        QWeights::new(&rng.values(0, rows * cols), rows, cols)
                    } else {
                        let q = int8_weights(&mut rng, weight_class, rows * cols);
                        QWeights::from_int8(&q, &scale, cols)
                    };
                    for input_class in 0..6 {
                        // Extreme weights over 1024 saturated columns.
                        if input_class == 2 && !wraps {
                            continue;
                        }
                        let what = format!(
                            "{mode}: {rows}x{cols} weights {weight_class} input {input_class}"
                        );
                        // The two steps hold different classes half the time.
                        let other = if rng.next() & 1 == 0 { input_class } else { 0 };
                        let mut x = step_input(&mut rng, input_class, cols);
                        x.extend(step_input(&mut rng, other, cols));
                        let run = |row_major: bool, steps: usize, x: &[f32]| {
                            let mut xq = vec![0i16; steps * cols];
                            let mut y = vec![f32::from_bits(0x7fc1_2345); steps * rows + 2 * GUARD];
                            let out = &mut y[GUARD..GUARD + steps * rows];
                            match (row_major, steps) {
                                (true, 1) => matvec_q8_rows::<1>(&w, rows, x, &mut xq, out),
                                (true, _) => matvec_q8_rows::<2>(&w, rows, x, &mut xq, out),
                                (false, 1) => matvec_q8(&w, rows, x, &mut xq, out),
                                (false, _) => matvec_q8_pair(&w, rows, x, &mut xq, out),
                            }
                            (bits(&y), xq)
                        };
                        for (steps, x) in [(1, &x[..cols]), (1, &x[cols..]), (2, &x[..])] {
                            let (want, want_xq) = run(true, steps, x);
                            let (got, got_xq) = run(false, steps, x);
                            assert_eq!(got, want, "{what} steps {steps}");
                            assert_eq!(got_xq, want_xq, "{what} steps {steps}");
                            let guard = f32::from_bits(0x7fc1_2345).to_bits();
                            assert!(
                                got[..GUARD]
                                    .iter()
                                    .chain(&got[GUARD + steps * rows..])
                                    .all(|&b| b == guard),
                                "{what}: wrote outside the output"
                            );
                        }
                    }
                }
            }
        });
    }

    /// The tiled layout holds each int8 value once, at the place the kernel
    /// reads it: against a plain scalar sum over the row-major values, with
    /// wrapping i32 sums, for whole groups and row tails.
    #[test]
    fn tiled_matvec_matches_a_wrapping_scalar_sum() {
        let mut rng = crate::glue::tests::Rng(0x7117_ed00_0000_0003);
        for (rows, cols) in [(384, 128), (4, 16), (7, 48), (10, 1024), (3, 32)] {
            let q = int8_weights(&mut rng, 0, rows * cols);
            let scale: Vec<f32> = (0..rows).map(|r| (r + 1) as f32 * 1e-3).collect();
            let w = QWeights::from_int8(&q, &scale, cols);
            // Every value is in the tiles exactly once.
            let grouped = rows / 4 * 4;
            let mut seen = vec![false; rows * cols];
            for (i, &v) in q.iter().enumerate() {
                let at = tiled(i / cols, i % cols, cols, grouped);
                assert!(!seen[at] && w.q[at] == v);
                seen[at] = true;
            }
            for class in [0, 1, 2] {
                let x = step_input(&mut rng, class, cols);
                let mut xq = vec![0i16; cols];
                let mut y = vec![0.; rows];
                matvec_q8(&w, rows, &x, &mut xq, &mut y);
                let mut xi = vec![0i16; cols];
                let xs = quantize_x(&x, &mut xi);
                for r in 0..rows {
                    let sum = (0..cols).fold(0i32, |a, c| {
                        a.wrapping_add(q[r * cols + c] as i32 * xi[c] as i32)
                    });
                    let want = sum as f32 * (scale[r] * xs);
                    assert_eq!(y[r].to_bits(), want.to_bits(), "{rows}x{cols} row {r}");
                }
            }
        }
    }

    /// The tiled kernel with its input and its output each ending at an
    /// inaccessible page, and again each beginning at one: no load or store
    /// leaves them, for whole groups and row tails, one step and two.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[test]
    fn tiled_matvec_stays_inside_its_slices() {
        use crate::glue::tests::{Fenced, Rng};
        let mut rng = Rng(0xfe1c_ed00_70c0_de04);
        for at_end in [true, false] {
            for (rows, cols) in [(384, 128), (4, 16), (7, 48), (3, 32), (9, 1024)] {
                let w = QWeights::new(&rng.values(0, rows * cols), rows, cols);
                for steps in [1, 2] {
                    let plain = rng.values(0, steps * cols);
                    let x = Fenced::new(&plain, at_end);
                    let mut y = Fenced::new(&vec![0.; steps * rows], at_end);
                    let mut xq = vec![0i16; steps * cols];
                    let mut want = vec![0.; steps * rows];
                    if steps == 1 {
                        matvec_q8(&w, rows, &x, &mut xq, &mut y);
                        matvec_q8_rows::<1>(&w, rows, &plain, &mut xq, &mut want);
                    } else {
                        matvec_q8_pair(&w, rows, &x, &mut xq, &mut y);
                        matvec_q8_rows::<2>(&w, rows, &plain, &mut xq, &mut want);
                    }
                    same_bits(&y, &want);
                }
            }
        }
    }

    /// The tests' `reference` switch selects the row-major kernel, so the
    /// whole-model comparisons in `crate::tests` cover this kernel too.
    #[test]
    fn the_reference_switch_selects_the_row_major_kernel() {
        assert!(!row_major_selected());
        crate::reference::set(true);
        let selected = row_major_selected();
        crate::reference::set(false);
        assert!(selected);
    }

    #[test]
    fn grouped_registers_dispatch_covers_its_widths() {
        let enabled = cfg!(target_arch = "aarch64");
        for hs in [8, 16, 32, 40] {
            assert_eq!(grouped_registers_enabled::<1>(hs), enabled, "width {hs}");
            assert!(!grouped_registers_enabled::<2>(hs));
        }
        for hs in [0, 7, 9, 13, 24, 41] {
            assert!(!grouped_registers_enabled::<1>(hs));
        }
    }

    /// Compare with the original AArch64 implementation itself, rather than
    /// the host fallback (which has different f32 weight/scale arithmetic).
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn grouped_registers_match_original_aarch64_bit_for_bit() {
        for (groups, ws) in [
            (1, 0),
            (1, 1),
            (3, 7),
            (5, 17),
            (16, 8),
            (16, 16),
            (16, 20),
            (16, 32),
            (32, 8),
            (32, 20),
            (32, 64),
        ] {
            for hs in [7, 8, 13, 16, 32, 40] {
                let weights = Weights::rows(&values(groups * ws * hs), hs);
                for mode in 0..5 {
                    let x: Vec<f32> = (0..2 * groups * ws)
                        .map(|i| match mode {
                            0 => 0.,
                            1 => {
                                if i % 2 == 0 {
                                    1e20
                                } else {
                                    -1e20
                                }
                            }
                            2 => {
                                if i % 2 == 0 {
                                    f32::from_bits(1)
                                } else {
                                    -f32::from_bits(1)
                                }
                            }
                            3 => [f32::INFINITY, f32::NEG_INFINITY, f32::NAN][i % 3],
                            _ => ((i * 17 % 101) as f32 - 50.) / 67.,
                        })
                        .collect();
                    let mut old = vec![123.; 2 * groups * hs];
                    let mut new = old.clone();
                    grouped_impl::<1, false>(&weights, groups, ws, hs, &x, &mut old);
                    grouped_impl::<1, true>(&weights, groups, ws, hs, &x, &mut new);
                    same_bits(&old[..groups * hs], &new[..groups * hs]);
                    grouped_impl::<2, false>(&weights, groups, ws, hs, &x, &mut old);
                    grouped_impl::<2, true>(&weights, groups, ws, hs, &x, &mut new);
                    same_bits(&old, &new);
                }
            }
        }
    }

    #[test]
    fn paired_grouped_matches_separate_inputs_including_tails() {
        for (groups, ws, hs) in [(2, 7, 3), (4, 16, 8), (3, 32, 13), (32, 64, 16)] {
            let w = Weights::rows(&values(groups * ws * hs), hs);
            let x = values(2 * groups * ws);
            let mut expected = vec![0.; 2 * groups * hs];
            let mut actual = expected.clone();
            grouped(
                &w,
                groups,
                ws,
                hs,
                &x[..groups * ws],
                &mut expected[..groups * hs],
            );
            grouped(
                &w,
                groups,
                ws,
                hs,
                &x[groups * ws..],
                &mut expected[groups * hs..],
            );
            grouped_pair(&w, groups, ws, hs, &x, &mut actual);
            same_bits(&actual, &expected);
        }
    }

    #[test]
    fn paired_pointwise_matches_separate_inputs_with_output_stride() {
        let sizes = [(4, 3, 8), (12, 8, 24), (64, 64, 64)];
        for (out, inp, f) in sizes {
            let w = Weights::rows(&pack_pointwise(&values(out * inp), out, inp), 4);
            let x = values(2 * inp * f);
            let ys = out * f + 13;
            let mut expected = vec![123.; 2 * ys];
            let mut actual = expected.clone();
            pointwise(&w, out, inp, &x[..inp * f], f, &mut expected[..ys]);
            pointwise(&w, out, inp, &x[inp * f..], f, &mut expected[ys..]);
            pointwise_pair(&w, out, inp, &x, f, &mut actual, ys);
            same_bits(&actual, &expected);
        }
    }

    /// `pointwise_onto` over a pointwise conv's inputs, split into runs at
    /// arbitrary places, against `pointwise` over all of them at once: every
    /// row count from one to twelve (whole blocks of four and blocks left
    /// short), 16-bin blocks with and without the 8-bin tail, every input
    /// class and float mode, with the sums resting in memory between runs.
    /// Rows past `rows` and the bins around the output stay untouched.
    #[test]
    fn pointwise_onto_in_any_runs_matches_pointwise_bit_for_bit() {
        use crate::glue::tests::{CLASSES, Rng, assert_same, each_float_mode};
        const GUARD: usize = 11;
        each_float_mode(|mode| {
            let mut rng = Rng(0x7e57_0f0e_70c0_de01);
            for rows in 1..=12usize {
                let out = rows.next_multiple_of(4);
                for (inp, f) in [(1, 8), (3, 16), (7, 24), (32, 64), (160, 40)] {
                    let mut dense = rng.values(0, out * inp);
                    // The outputs past `rows` are zero rows, as `Convp` packs.
                    dense[rows * inp..].fill(0.);
                    let w = Weights::rows(&pack_pointwise(&dense, out, inp), 4);
                    for class in 0..CLASSES {
                        let what = format!("{mode}: rows {rows} inp {inp} f {f} class {class}");
                        let x = rng.values(class, inp * f);
                        let mut want = vec![7.; out * f];
                        pointwise(&w, out, inp, &x, f, &mut want);
                        let junk = rng.values(class, rows * f + 2 * GUARD);
                        let mut got = junk.clone();
                        let mut at = 0;
                        while at < inp {
                            let run = 1 + rng.next() as usize % (inp - at);
                            pointwise_onto(
                                &w,
                                rows,
                                inp,
                                at,
                                run,
                                &x[at * f..(at + run) * f],
                                f,
                                &mut got[GUARD..GUARD + rows * f],
                                at == 0,
                            );
                            at += run;
                        }
                        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
                        assert_eq!(bits(&got[..GUARD]), bits(&junk[..GUARD]), "{what}");
                        assert_eq!(
                            bits(&got[GUARD + rows * f..]),
                            bits(&junk[GUARD + rows * f..]),
                            "{what}"
                        );
                        let got = &got[GUARD..GUARD + rows * f];
                        // Without infinities or NaNs every bit agrees; with
                        // them a NaN's payload is left open (see `glue`).
                        if class < CLASSES - 1 {
                            assert_eq!(bits(got), bits(&want[..rows * f]), "{what}");
                        } else {
                            assert_same(got, &want[..rows * f], &what);
                        }
                    }
                }
            }
        });
    }

    /// `pointwise_onto` with its input and its sums each ending at an
    /// inaccessible page, and again each beginning at one (`Fenced`): no
    /// load or store leaves them, for whole and short blocks of rows and
    /// with and without the 8-bin tail.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[test]
    fn pointwise_onto_stays_inside_its_slices() {
        use crate::glue::tests::{Fenced, Rng};
        let mut rng = Rng(0xfe1c_ed00_70c0_de02);
        for at_end in [true, false] {
            for rows in 1..=9usize {
                let out = rows.next_multiple_of(4);
                for (inp, f) in [(1, 8), (3, 16), (7, 24), (32, 64)] {
                    let mut dense = rng.values(0, out * inp);
                    dense[rows * inp..].fill(0.);
                    let w = Weights::rows(&pack_pointwise(&dense, out, inp), 4);
                    let x = rng.values(1, inp * f);
                    let mut want = vec![0.; out * f];
                    pointwise(&w, out, inp, &x, f, &mut want);
                    let mut got = Fenced::new(&rng.values(1, rows * f), at_end);
                    // In two runs when there are two inputs to split.
                    let first = inp.div_ceil(2);
                    for (at, run) in [(0, first), (first, inp - first)] {
                        let x = Fenced::new(&x[at * f..(at + run) * f], at_end);
                        pointwise_onto(&w, rows, inp, at, run, &x, f, &mut got, at == 0);
                    }
                    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
                    assert_eq!(
                        bits(&got),
                        bits(&want[..rows * f]),
                        "rows {rows} inp {inp} f {f}"
                    );
                }
            }
        }
    }

    #[test]
    fn f16_round_trips_every_finite_value() {
        for h in 0..=u16::MAX {
            if h & 0x7c00 == 0x7c00 {
                continue;
            }
            assert_eq!(f16_bits(f16_value(h)), h, "{h:#06x}");
        }
    }

    #[test]
    fn f16_rounds_to_nearest_even() {
        // 1 + 2^-11 is halfway between 1 and the next f16; ties go to even (1).
        assert_eq!(f16_bits(1. + 2f32.powi(-11)), 0x3c00);
        assert_eq!(f16_bits(1. + 3. * 2f32.powi(-11)), 0x3c02);
        assert_eq!(f16_bits(65520.), 0x7c00);
        assert_eq!(f16_bits(2f32.powi(-25)), 0);
        assert_eq!(f16_bits(-2f32.powi(-24)), 0x8001);
    }

    #[test]
    fn polynomial_exp_matches_std() {
        for i in -8000..8000 {
            let x = i as f32 * 0.01;
            let (a, b) = (exp_scalar(x), x.exp());
            assert!((a - b).abs() <= b * 3e-7, "{x}: {a} vs {b}");
        }
    }
}
