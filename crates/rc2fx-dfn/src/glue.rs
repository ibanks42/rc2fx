//! The small f32 loops between the dense kernels: the 3-tap convolutions
//! along frequency, the transposes and one transposed add.
//!
//! Each loop has a scalar form in `scalar`, which is the reference. It runs on
//! other architectures, for rows too short for a vector, and when
//! `crate::reference` is on. The NEON forms compute every output with the
//! same operations in the same order as the scalar form: a separate multiply
//! and add for each term, never a fused multiply-add, and the same grouping
//! of the sums. Their results are therefore bit for bit the scalar ones for
//! all inputs, with one exception: when two different NaNs meet in an add,
//! which NaN's payload survives depends on the operand order the compiler
//! chose, so NaN results are only guaranteed to be NaN in both.
use crate::reference;

/// The scalar reference forms. `conv3x3` here writes each output row three
/// times (one overwrite, two accumulations); the NEON form writes it once
/// with the same sum order.
pub(crate) mod scalar {
    /// Depthwise convolution along frequency with a 3-tap kernel, padding 1.
    /// x [c][f] -> y [c][f / stride].
    pub(crate) fn depthwise3(
        w: &[f32],
        x: &[f32],
        c: usize,
        f: usize,
        stride: usize,
        y: &mut [f32],
    ) {
        let fo = f / stride;
        for ch in 0..c {
            let (k0, k1, k2) = (w[ch * 3], w[ch * 3 + 1], w[ch * 3 + 2]);
            let xr = &x[ch * f..(ch + 1) * f];
            let yr = &mut y[ch * fo..(ch + 1) * fo];
            if stride == 1 {
                shift3(k0, k1, k2, xr, yr);
            } else {
                // y[o] = k0 x[2o-1] + k1 x[2o] + k2 x[2o+1]
                yr[0] = k1 * xr[0] + k2 * xr[1];
                for o in 1..fo {
                    yr[o] = k0 * xr[2 * o - 1] + k1 * xr[2 * o] + k2 * xr[2 * o + 1];
                }
            }
        }
    }

    /// y[b] = k0 x[b-1] + k1 x[b] + k2 x[b+1] with zero padding (y overwritten).
    pub(crate) fn shift3(k0: f32, k1: f32, k2: f32, x: &[f32], y: &mut [f32]) {
        y[..x.len()].fill(0.);
        shift3_add(k0, k1, k2, x, y);
    }

    /// y[b] += k0 x[b-1] + k1 x[b] + k2 x[b+1] with zero padding (x at least
    /// two bins).
    pub(crate) fn shift3_add(k0: f32, k1: f32, k2: f32, x: &[f32], y: &mut [f32]) {
        let n = x.len();
        let y = &mut y[..n];
        y[0] += k1 * x[0] + k2 * x[1];
        // One fused pass over the interior, three unit-stride reads per bin.
        for (((o, &a), &b), &c) in y[1..n - 1]
            .iter_mut()
            .zip(&x[..n - 2])
            .zip(&x[1..n - 1])
            .zip(&x[2..])
        {
            *o += k0 * a + k1 * b + k2 * c;
        }
        y[n - 1] += k0 * x[n - 2] + k1 * x[n - 1];
    }

    /// Depthwise transposed convolution along frequency: kernel 3, stride 2,
    /// padding 1, output padding 1. x [c][f] -> y [c][2f]:
    /// y[2i] = k1 x[i], y[2i+1] = k2 x[i] + k0 x[i+1].
    pub(crate) fn depthwise_t3(w: &[f32], x: &[f32], c: usize, f: usize, y: &mut [f32]) {
        for ch in 0..c {
            let (k0, k1, k2) = (w[ch * 3], w[ch * 3 + 1], w[ch * 3 + 2]);
            let xr = &x[ch * f..(ch + 1) * f];
            let yr = &mut y[ch * 2 * f..(ch + 1) * 2 * f];
            for i in 0..f {
                let next = if i + 1 < f { xr[i + 1] } else { 0. };
                yr[2 * i] = k1 * xr[i];
                yr[2 * i + 1] = k2 * xr[i] + k0 * next;
            }
        }
    }

    /// 3x3 conv from `groups` input channels (each feeding ch/groups outputs)
    /// over 3 frames of history: `x` is [3 frames][groups][f], oldest frame
    /// first. y is [ch][f].
    pub(crate) fn conv3x3(w: &[f32], ch: usize, x: &[f32], groups: usize, f: usize, y: &mut [f32]) {
        let per = ch / groups;
        for (o, yr) in y[..ch * f].chunks_exact_mut(f).enumerate() {
            let g = o / per;
            let k = &w[o * 9..o * 9 + 9];
            // Oldest frame first, straight into the output: the first frame
            // overwrites, the other two accumulate.
            let xr = &x[g * f..(g + 1) * f];
            shift3(k[0], k[1], k[2], xr, yr);
            for kt in 1..3 {
                let xr = &x[(kt * groups + g) * f..(kt * groups + g + 1) * f];
                shift3_add(k[kt * 3], k[kt * 3 + 1], k[kt * 3 + 2], xr, yr);
            }
        }
    }

    /// [rows][cols] -> [cols][rows]
    pub(crate) fn transpose(x: &[f32], rows: usize, cols: usize, y: &mut [f32]) {
        for r in 0..rows {
            for c in 0..cols {
                y[c * rows + r] = x[r * cols + c];
            }
        }
    }

    /// y [cols][rows] += z [rows][cols] transposed.
    pub(crate) fn add_transposed(z: &[f32], rows: usize, cols: usize, y: &mut [f32]) {
        for c in 0..cols {
            for r in 0..rows {
                y[c * rows + r] += z[r * cols + c];
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    /// Fewest outputs per row `stride2_row` takes.
    pub(super) const STRIDE2_MIN_OUT: usize = 5;
    /// Shortest input row `t3_row` takes.
    pub(super) const T3_MIN: usize = 5;
    /// Shortest row `taps_fill` and `taps_add` take.
    pub(super) const TAPS_MIN: usize = 6;

    /// One row of the stride-2 depthwise conv: `fo` outputs from `2 * fo`
    /// inputs, y[o] = (k0 x[2o-1] + k1 x[2o]) + k2 x[2o+1], the first without
    /// its k0 term.
    ///
    /// # Safety
    /// `fo >= STRIDE2_MIN_OUT`, `2 * fo` readable at `x`, `fo` writable at
    /// `y`, not overlapping.
    #[inline(always)]
    pub(super) unsafe fn stride2_row(k: [f32; 3], x: *const f32, fo: usize, y: *mut f32) {
        // SAFETY: the vector at output o reads x[2o-1 .. 2o+8) and writes
        // y[o .. o+4), and o stays within 1..=fo-4.
        unsafe {
            *y = k[1] * *x + k[2] * *x.add(1);
            let (v0, v1, v2) = (vdupq_n_f32(k[0]), vdupq_n_f32(k[1]), vdupq_n_f32(k[2]));
            let mut o = 1;
            loop {
                // a.0 = x[2o-1], x[2o+1], ..; a.1 = x[2o], ..; b.1 = x[2o+1], ..
                let a = vld2q_f32(x.add(2 * o - 1));
                let b = vld2q_f32(x.add(2 * o));
                let r = vaddq_f32(
                    vaddq_f32(vmulq_f32(v0, a.0), vmulq_f32(v1, a.1)),
                    vmulq_f32(v2, b.1),
                );
                vst1q_f32(y.add(o), r);
                if o + 4 >= fo {
                    break;
                }
                // The last vector overlaps the one before it: every output
                // depends on x alone, so writing one twice is harmless.
                o = (o + 4).min(fo - 4);
            }
        }
    }

    /// One row of the transposed depthwise conv: y[2i] = k1 x[i],
    /// y[2i+1] = k2 x[i] + k0 x[i+1], with zero past the row's end.
    ///
    /// # Safety
    /// `f >= T3_MIN`, `f` readable at `x`, `2 * f` writable at `y`, not
    /// overlapping.
    #[inline(always)]
    pub(super) unsafe fn t3_row(k: [f32; 3], x: *const f32, f: usize, y: *mut f32) {
        // SAFETY: the vector at input i reads x[i .. i+5) and writes
        // y[2i .. 2i+8), and i stays within 0..=f-5.
        unsafe {
            let (v0, v1, v2) = (vdupq_n_f32(k[0]), vdupq_n_f32(k[1]), vdupq_n_f32(k[2]));
            let mut i = 0;
            loop {
                let xv = vld1q_f32(x.add(i));
                let next = vld1q_f32(x.add(i + 1));
                let even = vmulq_f32(v1, xv);
                let odd = vaddq_f32(vmulq_f32(v2, xv), vmulq_f32(v0, next));
                vst2q_f32(y.add(2 * i), float32x4x2_t(even, odd));
                if i + 5 >= f {
                    break;
                }
                i = (i + 4).min(f - 5);
            }
            // The last input has no successor; its k0 term multiplies the
            // zero padding, as in the scalar form.
            let last = *x.add(f - 1);
            *y.add(2 * f - 2) = k[1] * last;
            *y.add(2 * f - 1) = k[2] * last + k[0] * 0.;
        }
    }

    /// The 3-tap term of one vector of bins starting at `p`:
    /// (k0 x[b-1] + k1 x[b]) + k2 x[b+1].
    #[inline(always)]
    unsafe fn taps(kv: &[float32x4_t; 3], p: *const f32) -> float32x4_t {
        // SAFETY: the caller keeps p - 1 .. p + 5 readable.
        unsafe {
            vaddq_f32(
                vaddq_f32(
                    vmulq_f32(kv[0], vld1q_f32(p.sub(1))),
                    vmulq_f32(kv[1], vld1q_f32(p)),
                ),
                vmulq_f32(kv[2], vld1q_f32(p.add(1))),
            )
        }
    }

    /// `T` zero-padded 3-tap rows summed into one output row, written once:
    /// y[b] = ((0 + r_0[b]) + r_1[b]) + ..., with
    /// r_t[b] = (k_t0 x_t[b-1] + k_t1 x_t[b]) + k_t2 x_t[b+1]. The leading
    /// zero is kept: the scalar form clears the row and then accumulates, and
    /// 0 + (-0) is +0.
    ///
    /// # Safety
    /// `n >= TAPS_MIN`, `n` readable at every `x[t]`, `n` writable at `y`,
    /// not overlapping.
    #[inline(always)]
    pub(super) unsafe fn taps_fill<const T: usize>(
        k: &[[f32; 3]; T],
        x: &[*const f32; T],
        n: usize,
        y: *mut f32,
    ) {
        // SAFETY: the vector at bin b reads x[b-1 .. b+5) and writes
        // y[b .. b+4), and b stays within 1..=n-5.
        unsafe {
            let (mut first, mut last) = (0f32, 0f32);
            for (k, &x) in k.iter().zip(x) {
                first += k[1] * *x + k[2] * *x.add(1);
                last += k[0] * *x.add(n - 2) + k[1] * *x.add(n - 1);
            }
            let kv: [[float32x4_t; 3]; T] = k.map(|k| k.map(|v| vdupq_n_f32(v)));
            let mut b = 1;
            loop {
                let mut acc = vdupq_n_f32(0.);
                for (kv, &x) in kv.iter().zip(x) {
                    acc = vaddq_f32(acc, taps(kv, x.add(b)));
                }
                vst1q_f32(y.add(b), acc);
                if b + 5 >= n {
                    break;
                }
                // The last vector overlaps the one before it; the row is
                // only written, never read, so that is harmless.
                b = (b + 4).min(n - 5);
            }
            *y = first;
            *y.add(n - 1) = last;
        }
    }

    /// One zero-padded 3-tap row added to `y`:
    /// y[b] += (k0 x[b-1] + k1 x[b]) + k2 x[b+1].
    ///
    /// # Safety
    /// `n >= TAPS_MIN`, `n` readable at `x`, `n` readable and writable at
    /// `y`, not overlapping.
    #[inline(always)]
    pub(super) unsafe fn taps_add(k: [f32; 3], x: *const f32, n: usize, y: *mut f32) {
        // SAFETY: the vector at bin b reads x[b-1 .. b+5) and updates
        // y[b .. b+4) with b + 4 <= n - 1; the scalar loop finishes the
        // interior. No bin is visited twice, as each update reads y.
        unsafe {
            *y += k[1] * *x + k[2] * *x.add(1);
            let kv = k.map(|v| vdupq_n_f32(v));
            let mut b = 1;
            while b + 4 < n {
                let p = y.add(b);
                vst1q_f32(p, vaddq_f32(vld1q_f32(p), taps(&kv, x.add(b))));
                b += 4;
            }
            while b < n - 1 {
                *y.add(b) += k[0] * *x.add(b - 1) + k[1] * *x.add(b) + k[2] * *x.add(b + 1);
                b += 1;
            }
            *y.add(n - 1) += k[0] * *x.add(n - 2) + k[1] * *x.add(n - 1);
        }
    }
}

/// Depthwise convolution along frequency with a 3-tap kernel, padding 1.
/// x [c][f] -> y [c][f / stride], `stride` 1 or 2.
pub(crate) fn depthwise3(w: &[f32], x: &[f32], c: usize, f: usize, stride: usize, y: &mut [f32]) {
    #[cfg(any(test, target_arch = "aarch64"))]
    let fo = f / stride;
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch(Class::Param, w.as_ptr(), c * 3 * 4);
        touch(Class::Act, x.as_ptr(), c * f * 4);
        crate::traffic::store(Class::Act, y.as_ptr(), c * fo * 4);
    }
    #[cfg(target_arch = "aarch64")]
    {
        let wide = if stride == 1 {
            f >= neon::TAPS_MIN
        } else {
            fo >= neon::STRIDE2_MIN_OUT
        };
        if wide && !reference::on() {
            assert!(w.len() >= c * 3 && x.len() >= c * f && y.len() >= c * fo);
            for ch in 0..c {
                let k = [w[ch * 3], w[ch * 3 + 1], w[ch * 3 + 2]];
                // SAFETY: the assert covers row `ch` of x ([c][f]) and of y
                // ([c][fo]); the row lengths meet the kernels' minimums, and
                // a stride-2 row reads 2 * fo <= f inputs.
                unsafe {
                    let (xr, yr) = (x.as_ptr().add(ch * f), y.as_mut_ptr().add(ch * fo));
                    if stride == 1 {
                        neon::taps_fill(&[k], &[xr], f, yr);
                    } else {
                        neon::stride2_row(k, xr, fo, yr);
                    }
                }
            }
            return;
        }
    }
    scalar::depthwise3(w, x, c, f, stride, y);
}

/// y[b] += k0 x[b-1] + k1 x[b] + k2 x[b+1] with zero padding (x at least
/// two bins).
pub(crate) fn shift3_add(k0: f32, k1: f32, k2: f32, x: &[f32], y: &mut [f32]) {
    #[cfg(target_arch = "aarch64")]
    if x.len() >= neon::TAPS_MIN && !reference::on() {
        assert!(y.len() >= x.len());
        // SAFETY: both slices hold x.len() values, at least the minimum.
        unsafe { neon::taps_add([k0, k1, k2], x.as_ptr(), x.len(), y.as_mut_ptr()) };
        return;
    }
    scalar::shift3_add(k0, k1, k2, x, y);
}

/// Depthwise transposed convolution along frequency: kernel 3, stride 2,
/// padding 1, output padding 1. x [c][f] -> y [c][2f]:
/// y[2i] = k1 x[i], y[2i+1] = k2 x[i] + k0 x[i+1].
pub(crate) fn depthwise_t3(w: &[f32], x: &[f32], c: usize, f: usize, y: &mut [f32]) {
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch(Class::Param, w.as_ptr(), c * 3 * 4);
        touch(Class::Act, x.as_ptr(), c * f * 4);
        crate::traffic::store(Class::Act, y.as_ptr(), c * 2 * f * 4);
    }
    #[cfg(target_arch = "aarch64")]
    if f >= neon::T3_MIN && !reference::on() {
        assert!(w.len() >= c * 3 && x.len() >= c * f && y.len() >= c * 2 * f);
        for ch in 0..c {
            let k = [w[ch * 3], w[ch * 3 + 1], w[ch * 3 + 2]];
            // SAFETY: the assert covers row `ch` of x ([c][f]) and of y
            // ([c][2f]), and f meets the kernel's minimum.
            unsafe { neon::t3_row(k, x.as_ptr().add(ch * f), f, y.as_mut_ptr().add(ch * 2 * f)) };
        }
        return;
    }
    scalar::depthwise_t3(w, x, c, f, y);
}

/// 3x3 conv from `groups` input channels (each feeding ch/groups outputs) over
/// 3 frames of history: `x` is [3 frames][groups][f], oldest frame first.
/// y is [ch][f].
pub(crate) fn conv3x3(w: &[f32], ch: usize, x: &[f32], groups: usize, f: usize, y: &mut [f32]) {
    #[cfg(any(test, target_arch = "aarch64"))]
    let per = ch / groups;
    // `per > 0` leaves a conv without outputs, or with fewer outputs than
    // groups, to the scalar form, which does nothing or panics as it always
    // has.
    #[cfg(target_arch = "aarch64")]
    if f >= neon::TAPS_MIN && per > 0 && !reference::on() {
        // Each output reads its group's three frames and writes itself once.
        #[cfg(test)]
        {
            use crate::traffic::*;
            touch(Class::Param, w.as_ptr(), ch * 9 * 4);
            touch_n(Class::Act, x.as_ptr(), 3 * groups * f * 4, per);
            crate::traffic::store(Class::Act, y.as_ptr(), ch * f * 4);
        }
        assert!(w.len() >= ch * 9 && x.len() >= 3 * groups * f && y.len() >= ch * f);
        for o in 0..ch {
            let g = o / per;
            assert!(g < groups);
            let k: [[f32; 3]; 3] = std::array::from_fn(|t| {
                [w[o * 9 + t * 3], w[o * 9 + t * 3 + 1], w[o * 9 + t * 3 + 2]]
            });
            // SAFETY: g < groups, so each frame's row (t * groups + g) lies
            // in x ([3][groups][f]); row o lies in y ([ch][f]); f meets the
            // kernel's minimum.
            unsafe {
                let xs: [*const f32; 3] =
                    std::array::from_fn(|t| x.as_ptr().add((t * groups + g) * f));
                neon::taps_fill(&k, &xs, f, y.as_mut_ptr().add(o * f));
            }
        }
        return;
    }
    // Each output reads its group's three frames and writes itself three times.
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch(Class::Param, w.as_ptr(), ch * 9 * 4);
        touch_n(Class::Act, x.as_ptr(), 3 * groups * f * 4, per);
        touch_n(Class::Act, y.as_ptr(), ch * f * 4, 2);
        crate::traffic::store_n(Class::Act, y.as_ptr(), ch * f * 4, 3);
    }
    scalar::conv3x3(w, ch, x, groups, f, y);
}

/// [rows][cols] -> [cols][rows]
pub(crate) fn transpose(x: &[f32], rows: usize, cols: usize, y: &mut [f32]) {
    #[cfg(test)]
    {
        use crate::traffic::*;
        touch(Class::Act, x.as_ptr(), rows * cols * 4);
        crate::traffic::store(Class::Act, y.as_ptr(), rows * cols * 4);
    }
    if reference::on() || rows == 0 || cols == 0 {
        return scalar::transpose(x, rows, cols, y);
    }
    let y = &mut y[..rows * cols];
    for (r, xr) in x[..rows * cols].chunks_exact(cols).enumerate() {
        for (c, &v) in xr.iter().enumerate() {
            // SAFETY: c < cols and r < rows, so c * rows + r < rows * cols,
            // the length of y.
            unsafe { *y.get_unchecked_mut(c * rows + r) = v };
        }
    }
}

/// y [cols][rows] += z [rows][cols] transposed.
pub(crate) fn add_transposed(z: &[f32], rows: usize, cols: usize, y: &mut [f32]) {
    if reference::on() || rows == 0 || cols == 0 {
        return scalar::add_transposed(z, rows, cols, y);
    }
    let z = &z[..rows * cols];
    for (c, yc) in y[..rows * cols].chunks_exact_mut(rows).enumerate() {
        for (r, v) in yc.iter_mut().enumerate() {
            // SAFETY: r < rows and c < cols, so r * cols + c < rows * cols,
            // the length of z.
            *v += unsafe { *z.get_unchecked(r * cols + c) };
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Rng(pub(crate) u64);

    impl Rng {
        pub(crate) fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 20) as u32
        }

        /// Uniform in -1..1.
        pub(crate) fn unit(&mut self) -> f32 {
            (self.next() & 0xff_ffff) as f32 / (1 << 23) as f32 - 1.
        }

        /// One value of an input class: 0 ordinary audio-sized values, 1
        /// signed zeros among them, 2 subnormals, 3 magnitudes whose products
        /// overflow or vanish, 4 all of those with infinities and NaNs.
        pub(crate) fn value(&mut self, class: usize) -> f32 {
            let sign = if self.next() & 1 == 0 { 1. } else { -1. };
            match (class, self.next() % 8) {
                (0, _) => self.unit(),
                (1, 0..=3) => 0. * sign,
                (1, _) => self.unit(),
                (2, 0..=4) => f32::from_bits(1 + self.next() % 0x7f_ffff) * sign,
                (2, 5) => f32::MIN_POSITIVE * sign,
                (2, _) => self.unit() * 1e-30,
                (3, 0) => f32::MAX * sign,
                (3, 1) => 1e30 * self.unit(),
                (3, 2) => 1e-30 * self.unit(),
                (3, 3) => 3e19 * sign,
                (3, _) => self.unit(),
                (_, 0) => f32::INFINITY * sign,
                (_, 1) => f32::NAN * sign,
                (_, 2) => f32::from_bits(0x7f80_0001 + self.next() % 0x7f_fffe) * sign,
                (_, 3) => 0. * sign,
                (_, 4) => f32::MAX * sign,
                (_, 5) => f32::from_bits(1 + self.next() % 0x7f_ffff) * sign,
                (_, _) => self.unit(),
            }
        }

        pub(crate) fn values(&mut self, class: usize, n: usize) -> Vec<f32> {
            (0..n).map(|_| self.value(class)).collect()
        }
    }

    pub(crate) const CLASSES: usize = 5;

    /// Bit for bit, except that two NaNs compare equal whatever their
    /// payloads (see the module comment).
    pub(crate) fn assert_same(got: &[f32], want: &[f32], what: &str) {
        assert_eq!(got.len(), want.len(), "{what}");
        for (i, (a, b)) in got.iter().zip(want).enumerate() {
            assert!(
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
                "{what}: element {i}: {a:e} ({:#010x}) vs {b:e} ({:#010x})",
                a.to_bits(),
                b.to_bits()
            );
        }
    }

    /// Runs `body` once for each setting of flush-to-zero (FZ) and
    /// default-NaN (DN) that AArch64 offers, and once elsewhere. The worker
    /// thread inherits the floating-point control register of the thread
    /// that starts it, and the vendor's threads may run with either bit set.
    /// Both bits govern scalar and vector arithmetic alike, so a kernel and
    /// its reference must agree under each.
    pub(crate) fn each_float_mode(mut body: impl FnMut(&str)) {
        #[cfg(target_arch = "aarch64")]
        {
            struct Restore(u64);
            impl Drop for Restore {
                fn drop(&mut self) {
                    // SAFETY: restores the control register the test began
                    // with.
                    unsafe { std::arch::asm!("msr fpcr, {}", in(reg) self.0) };
                }
            }
            let saved: u64;
            // SAFETY: reads the floating-point control register.
            unsafe { std::arch::asm!("mrs {}, fpcr", out(reg) saved) };
            let _restore = Restore(saved);
            for (name, bits) in [
                ("default", 0u64),
                ("FZ", 1 << 24),
                ("DN", 1 << 25),
                ("FZ+DN", 3 << 24),
            ] {
                let control = saved & !(3 << 24) | bits;
                let held: u64;
                // SAFETY: changes only the FZ and DN bits, for this thread,
                // until `_restore` drops; the code under test is compared
                // with its reference under the same setting.
                unsafe {
                    std::arch::asm!("msr fpcr, {}", in(reg) control);
                    std::arch::asm!("mrs {}, fpcr", out(reg) held);
                }
                assert_eq!(held, control, "the processor did not take {name}");
                body(name);
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        body("default");
    }

    const GUARD: usize = 9;
    const SENTINEL: f32 = 12345.678;

    /// Runs `f` on an output of `len` values between two guard bands and
    /// checks that the bands are untouched. The output starts as `fill`.
    fn guarded(len: usize, fill: &[f32], f: impl FnOnce(&mut [f32])) -> Vec<f32> {
        let mut buf = vec![SENTINEL; len + 2 * GUARD];
        buf[GUARD..GUARD + len].copy_from_slice(&fill[..len]);
        f(&mut buf[GUARD..GUARD + len]);
        assert!(
            buf[..GUARD]
                .iter()
                .chain(&buf[GUARD + len..])
                .all(|v| v.to_bits() == SENTINEL.to_bits()),
            "wrote outside the output"
        );
        buf[GUARD..GUARD + len].to_vec()
    }

    /// The row lengths the model uses and every short or odd one around the
    /// vector thresholds.
    fn lengths() -> impl Iterator<Item = usize> {
        (2..=41).chain([64, 96, 127, 128])
    }

    #[test]
    fn depthwise_matches_the_scalar_reference_bit_for_bit() {
        each_float_mode(|mode| {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
            for f in lengths() {
                for stride in [1, 2] {
                    for class in 0..CLASSES {
                        for c in [1, 3, 64] {
                            let w = rng.values(class.min(3), c * 3);
                            let x = rng.values(class, c * f);
                            let junk = rng.values(0, c * f);
                            let fo = f / stride;
                            let want = guarded(c * fo, &junk, |y| {
                                scalar::depthwise3(&w, &x, c, f, stride, y)
                            });
                            let got =
                                guarded(c * fo, &junk, |y| depthwise3(&w, &x, c, f, stride, y));
                            let what = format!("{mode}: f {f} stride {stride} class {class}");
                            assert_same(&got, &want, &what);
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn transposed_depthwise_matches_the_scalar_reference_bit_for_bit() {
        each_float_mode(|mode| {
            let mut rng = Rng(0x2545_f491_4f6c_dd1d);
            for f in (1..=41).chain([64, 127]) {
                for class in 0..CLASSES {
                    for c in [1, 3, 64] {
                        let w = rng.values(class.min(3), c * 3);
                        let x = rng.values(class, c * f);
                        let junk = rng.values(0, c * 2 * f);
                        let want =
                            guarded(c * 2 * f, &junk, |y| scalar::depthwise_t3(&w, &x, c, f, y));
                        let got = guarded(c * 2 * f, &junk, |y| depthwise_t3(&w, &x, c, f, y));
                        assert_same(&got, &want, &format!("{mode}: f {f} class {class}"));
                    }
                }
            }
        });
    }

    #[test]
    fn conv3x3_matches_the_scalar_reference_bit_for_bit() {
        each_float_mode(|mode| {
            let mut rng = Rng(0x0123_4567_89ab_cdef);
            for f in lengths() {
                for class in 0..CLASSES {
                    for (ch, groups) in [(1, 1), (6, 2), (8, 1), (64, 1), (64, 2)] {
                        let w = rng.values(class.min(3), ch * 9);
                        let x = rng.values(class, 3 * groups * f);
                        let junk = rng.values(0, ch * f);
                        let want =
                            guarded(ch * f, &junk, |y| scalar::conv3x3(&w, ch, &x, groups, f, y));
                        let got = guarded(ch * f, &junk, |y| conv3x3(&w, ch, &x, groups, f, y));
                        assert_same(
                            &got,
                            &want,
                            &format!("{mode}: f {f} class {class} ch {ch}/{groups}"),
                        );
                    }
                }
            }
        });
    }

    /// The accumulating row keeps what `y` held, signed zeros included.
    #[test]
    fn shift3_add_matches_the_scalar_reference_bit_for_bit() {
        each_float_mode(|mode| {
            let mut rng = Rng(0xfeed_face_cafe_beef);
            for n in lengths() {
                for class in 0..CLASSES {
                    for held in 0..CLASSES {
                        let k = rng.values(class.min(3), 3);
                        let x = rng.values(class, n);
                        let start = rng.values(held, n);
                        let want = guarded(n, &start, |y| {
                            scalar::shift3_add(k[0], k[1], k[2], &x, y);
                            scalar::shift3_add(k[2], k[0], k[1], &x, y);
                        });
                        let got = guarded(n, &start, |y| {
                            shift3_add(k[0], k[1], k[2], &x, y);
                            shift3_add(k[2], k[0], k[1], &x, y);
                        });
                        let what = format!("{mode}: n {n} class {class} held {held}");
                        assert_same(&got, &want, &what);
                    }
                }
            }
        });
    }

    /// Moves and single adds: every bit pattern must survive, NaN payloads
    /// included.
    #[test]
    fn transposes_match_the_scalar_reference_bit_for_bit() {
        let mut rng = Rng(0x1357_9bdf_0246_8ace);
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        for (rows, cols) in [
            (1, 1),
            (1, 7),
            (7, 1),
            (3, 5),
            (64, 32),
            (64, 8),
            (8, 64),
            (10, 64),
        ] {
            for class in 0..CLASSES {
                let x = rng.values(class, rows * cols);
                let junk = rng.values(0, rows * cols);
                let want = guarded(rows * cols, &junk, |y| scalar::transpose(&x, rows, cols, y));
                let got = guarded(rows * cols, &junk, |y| transpose(&x, rows, cols, y));
                assert_eq!(
                    bits(&got),
                    bits(&want),
                    "transpose {rows}x{cols} class {class}"
                );
                let start = rng.values(class.min(3), rows * cols);
                let want = guarded(rows * cols, &start, |y| {
                    scalar::add_transposed(&x, rows, cols, y)
                });
                let got = guarded(rows * cols, &start, |y| add_transposed(&x, rows, cols, y));
                assert_same(&got, &want, &format!("add {rows}x{cols} class {class}"));
            }
        }
    }

    /// Shapes with nothing to compute do nothing in either form: no
    /// channels, no outputs, no rows or no columns. The scalar loops simply
    /// do not run, and the faster forms must not panic where they did not.
    #[test]
    fn empty_shapes_do_nothing_as_in_the_scalar_reference() {
        let mut rng = Rng(0x0bad_5eed_0bad_5eed);
        let x = rng.values(0, 3 * 2 * 64);
        let w = rng.values(0, 64);
        let junk = rng.values(0, 64);
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        let same = |got: Vec<f32>, want: Vec<f32>, what: &str| {
            assert_eq!(bits(&got), bits(&want), "{what}");
            assert_eq!(bits(&got), bits(&junk), "{what}: untouched");
        };
        for f in [8, 64] {
            for stride in [1, 2] {
                same(
                    guarded(64, &junk, |y| depthwise3(&w, &x, 0, f, stride, y)),
                    guarded(64, &junk, |y| scalar::depthwise3(&w, &x, 0, f, stride, y)),
                    "depthwise3 without channels",
                );
            }
            same(
                guarded(64, &junk, |y| depthwise_t3(&w, &x, 0, f, y)),
                guarded(64, &junk, |y| scalar::depthwise_t3(&w, &x, 0, f, y)),
                "depthwise_t3 without channels",
            );
            for groups in [1, 2] {
                same(
                    guarded(64, &junk, |y| conv3x3(&w, 0, &x, groups, f, y)),
                    guarded(64, &junk, |y| scalar::conv3x3(&w, 0, &x, groups, f, y)),
                    "conv3x3 without outputs",
                );
            }
        }
        for (rows, cols) in [(0, 0), (0, 5), (5, 0)] {
            same(
                guarded(64, &junk, |y| transpose(&x, rows, cols, y)),
                guarded(64, &junk, |y| scalar::transpose(&x, rows, cols, y)),
                "transpose",
            );
            same(
                guarded(64, &junk, |y| add_transposed(&x, rows, cols, y)),
                guarded(64, &junk, |y| scalar::add_transposed(&x, rows, cols, y)),
                "add_transposed",
            );
        }
    }

    /// With the reference switch on, the dispatchers run the scalar forms.
    /// The two forms give the same values, so the values cannot show which
    /// one ran; conv3x3's traffic accounting can. The scalar form stores each
    /// output row three times and the NEON form once. Without this the
    /// whole-model comparisons in `tests.rs` could compare the NEON loops
    /// with themselves and never fail.
    #[test]
    fn the_reference_switch_selects_the_scalar_forms() {
        let mut rng = Rng(7);
        let (c, f) = (4, 32);
        let w = rng.values(0, c * 3);
        let x = rng.values(0, c * f);
        let mut want = vec![0.; c * f / 2];
        scalar::depthwise3(&w, &x, c, f, 2, &mut want);
        reference::set(true);
        let mut got = vec![0.; c * f / 2];
        depthwise3(&w, &x, c, f, 2, &mut got);
        reference::set(false);
        assert_same(&got, &want, "reference");

        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                reference::set(false);
                crate::traffic::disable();
            }
        }
        let _off = Off;
        let (ch, f) = (8, 64);
        let w = rng.values(0, ch * 9);
        let x = rng.values(0, 3 * f);
        let mut y = vec![0.; ch * f];
        let mut stores = |on: bool| {
            crate::traffic::enable();
            reference::set(on);
            conv3x3(&w, ch, &x, 1, f, &mut y);
            reference::set(false);
            let writes: Vec<u32> = crate::traffic::trace()
                .iter()
                .filter(|a| a.write)
                .map(|a| a.times)
                .collect();
            crate::traffic::disable();
            writes
        };
        assert_eq!(stores(true), [3], "the scalar form");
        let vector = cfg!(target_arch = "aarch64");
        assert_eq!(stores(false), [if vector { 1 } else { 3 }], "the fast form");
    }

    /// A buffer of f32 between two pages that cannot be read or written,
    /// placed against the page after it or against the page before it. A
    /// kernel that touches one element outside the buffer on that side
    /// faults, and the test binary dies with SIGSEGV, where a guard band of
    /// ordinary values would only catch a write. The vector loops load whole
    /// vectors through raw pointers, so a load past a slice's end is the
    /// mistake no other test here would see.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    pub(crate) struct Fenced {
        map: *mut u8,
        mapped: usize,
        data: *mut f32,
        len: usize,
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    mod sys {
        use std::ffi::c_void;
        // The C library is linked into every test binary already.
        unsafe extern "C" {
            pub(super) fn mmap(
                addr: *mut c_void,
                len: usize,
                prot: i32,
                flags: i32,
                fd: i32,
                offset: i64,
            ) -> *mut c_void;
            pub(super) fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
            pub(super) fn munmap(addr: *mut c_void, len: usize) -> i32;
            pub(super) fn sysconf(name: i32) -> i64;
        }
        pub(super) const SC_PAGESIZE: i32 = 30;
        pub(super) const PROT_NONE: i32 = 0;
        pub(super) const PROT_READ_WRITE: i32 = 3;
        pub(super) const MAP_PRIVATE_ANONYMOUS: i32 = 0x22;
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    impl Fenced {
        /// `values` copied into a fenced buffer that ends where the page
        /// after it begins (`at_end`), or begins where the page before it
        /// ends.
        pub(crate) fn new(values: &[f32], at_end: bool) -> Self {
            let len = values.len();
            // SAFETY: an anonymous private mapping of whole pages, of which
            // the first and the last are then made inaccessible; the data
            // lies in the pages between, which stay readable and writable.
            unsafe {
                let page = sys::sysconf(sys::SC_PAGESIZE) as usize;
                assert!(page.is_power_of_two() && page.is_multiple_of(4));
                let inner = (len * 4).next_multiple_of(page).max(page);
                let mapped = inner + 2 * page;
                let map = sys::mmap(
                    std::ptr::null_mut(),
                    mapped,
                    sys::PROT_READ_WRITE,
                    sys::MAP_PRIVATE_ANONYMOUS,
                    -1,
                    0,
                ) as *mut u8;
                assert!(!map.is_null() && map as isize != -1, "mmap");
                assert_eq!(sys::mprotect(map.cast(), page, sys::PROT_NONE), 0);
                assert_eq!(
                    sys::mprotect(map.add(page + inner).cast(), page, sys::PROT_NONE),
                    0
                );
                let offset = if at_end { inner - len * 4 } else { 0 };
                let data = map.add(page + offset) as *mut f32;
                std::ptr::copy_nonoverlapping(values.as_ptr(), data, len);
                Self {
                    map,
                    mapped,
                    data,
                    len,
                }
            }
        }
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    impl std::ops::Deref for Fenced {
        type Target = [f32];
        fn deref(&self) -> &[f32] {
            // SAFETY: `len` initialised f32 in the mapping, page-aligned or
            // a multiple of four bytes from a page boundary.
            unsafe { std::slice::from_raw_parts(self.data, self.len) }
        }
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    impl std::ops::DerefMut for Fenced {
        fn deref_mut(&mut self) -> &mut [f32] {
            // SAFETY: as `deref`, and uniquely borrowed.
            unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
        }
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    impl Drop for Fenced {
        fn drop(&mut self) {
            // SAFETY: the mapping made in `new`, no longer borrowed.
            unsafe { sys::munmap(self.map.cast(), self.mapped) };
        }
    }

    /// Every loop on weights, input and output that each end at an
    /// inaccessible page, and again with each beginning at one: no load or
    /// store leaves its slice, at the model's row lengths and at every short
    /// or odd one, and the results are still the scalar ones.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[test]
    fn no_loop_reads_or_writes_outside_its_slices() {
        let mut rng = Rng(0x5eed_fe1c_ed00_0001);
        for at_end in [true, false] {
            let fence = |v: &[f32]| Fenced::new(v, at_end);
            for f in lengths() {
                for c in [1, 3] {
                    let w = rng.values(0, c * 3);
                    let x = rng.values(1, c * f);
                    for stride in [1, 2] {
                        let fo = f / stride;
                        let mut want = vec![0.; c * fo];
                        scalar::depthwise3(&w, &x, c, f, stride, &mut want);
                        let mut got = fence(&rng.values(0, c * fo));
                        depthwise3(&fence(&w), &fence(&x), c, f, stride, &mut got);
                        assert_same(&got, &want, &format!("depthwise3 f {f} stride {stride}"));
                    }
                    let mut want = vec![0.; c * 2 * f];
                    scalar::depthwise_t3(&w, &x, c, f, &mut want);
                    let mut got = fence(&rng.values(0, c * 2 * f));
                    depthwise_t3(&fence(&w), &fence(&x), c, f, &mut got);
                    assert_same(&got, &want, &format!("depthwise_t3 f {f}"));
                }
                for (ch, groups) in [(1, 1), (6, 2)] {
                    let w = rng.values(0, ch * 9);
                    let x = rng.values(1, 3 * groups * f);
                    let mut want = vec![0.; ch * f];
                    scalar::conv3x3(&w, ch, &x, groups, f, &mut want);
                    let mut got = fence(&rng.values(0, ch * f));
                    conv3x3(&fence(&w), ch, &fence(&x), groups, f, &mut got);
                    assert_same(&got, &want, &format!("conv3x3 f {f} ch {ch}/{groups}"));
                }
                let k = rng.values(0, 3);
                let x = rng.values(1, f);
                let start = rng.values(1, f);
                let mut want = start.clone();
                scalar::shift3_add(k[0], k[1], k[2], &x, &mut want);
                let mut got = fence(&start);
                shift3_add(k[0], k[1], k[2], &fence(&x), &mut got);
                assert_same(&got, &want, &format!("shift3_add n {f}"));
            }
            for (rows, cols) in [(1, 1), (1, 7), (7, 1), (3, 5), (64, 32), (10, 64)] {
                let x = rng.values(1, rows * cols);
                let start = rng.values(1, rows * cols);
                let mut want = start.clone();
                scalar::transpose(&x, rows, cols, &mut want);
                let mut got = fence(&start);
                transpose(&fence(&x), rows, cols, &mut got);
                assert_same(&got, &want, &format!("transpose {rows}x{cols}"));
                let mut want = start.clone();
                scalar::add_transposed(&x, rows, cols, &mut want);
                let mut got = fence(&start);
                add_transposed(&fence(&x), rows, cols, &mut got);
                assert_same(&got, &want, &format!("add_transposed {rows}x{cols}"));
            }
        }
    }
}
