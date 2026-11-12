use crate::{BLOCK, Meter};
use rc2fx_core::{MAX_BANDS, eq::Coefficients};
mod stereo;

const COEFF_SMOOTH: f64 = 0.00415799815489;
const ZERO: Coefficients = Coefficients {
    b0: 0.0,
    b1: 0.0,
    b2: 0.0,
    a1: 0.0,
    a2: 0.0,
};
fn same_bits(a: Coefficients, b: Coefficients) -> bool {
    a.as_array().map(f64::to_bits) == b.as_array().map(f64::to_bits)
}

#[derive(Clone, Copy)]
pub(crate) struct Filter {
    c: Coefficients,
    checkpoint: Coefficients,
    last_target: Coefficients,
    z1: f64,
    z2: f64,
    smoothing: bool,
}
impl Default for Filter {
    fn default() -> Self {
        Self {
            c: ZERO,
            checkpoint: ZERO,
            last_target: ZERO,
            z1: 0.0,
            z2: 0.0,
            smoothing: false,
        }
    }
}
impl Filter {
    pub fn with_coefficients(c: Coefficients) -> Self {
        Self {
            c,
            ..Self::default()
        }
    }
    /// The whole filter as bits, for comparing two processing paths.
    #[cfg(test)]
    pub fn bits(&self) -> Vec<u64> {
        [self.c, self.checkpoint, self.last_target]
            .iter()
            .flat_map(|c| c.as_array())
            .chain([self.z1, self.z2])
            .map(f64::to_bits)
            .chain([u64::from(self.smoothing)])
            .collect()
    }
    /// Whether `begin` found coefficients still on their way to the target.
    pub fn is_smoothing(&self) -> bool {
        self.smoothing
    }
    /// Whether the coefficients stopped moving short of `target`: the
    /// smoothing step became too small to change them.
    #[cfg(test)]
    pub fn stalled(&self, target: Coefficients) -> bool {
        !self.smoothing && !same_bits(self.c, target)
    }
    pub fn begin(&mut self, target: Coefficients) {
        self.smoothing = !same_bits(self.c, target)
            && (!same_bits(self.last_target, target) || !same_bits(self.checkpoint, self.c));
        self.checkpoint = self.c;
        self.last_target = target;
    }
    /// `process` over a block, bit for bit. A steady filter runs a loop with no
    /// branches; anything `process` would treat as exceptional leaves a peak
    /// above the limit or a non-finite state, and the block is then redone with
    /// `process` from its saved state.
    pub fn process_block(
        &mut self,
        target: &Coefficients,
        x: &mut [f64],
        limit: f64,
        m: &mut Meter,
    ) {
        if !self.smoothing {
            let mut saved = [0.0; BLOCK];
            saved[..x.len()].copy_from_slice(x);
            let (z1, z2, peak) = steady(self.c, self.z1, self.z2, x);
            if clean(z1, z2, peak, limit) {
                (self.z1, self.z2) = (z1, z2);
                return;
            }
            x.copy_from_slice(&saved[..x.len()]);
        }
        for sample in x {
            *sample = self.process(target, *sample, limit, m);
        }
    }
}
/// What a steady block reads and writes for one EQ band: `b0 b1 b2 a1 a2`,
/// then `z1 z2`, each a `[left, right]` pair, in the order the stereo kernel
/// walks them. 112 bytes, and 16-byte aligned so that no pair straddles a
/// cache line.
#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
struct Hot {
    c: [[f64; 2]; 5],
    z: [[f64; 2]; 2],
}
const _: () = assert!(size_of::<Hot>() == 112);
impl Hot {
    fn coefficients(&self, ch: usize) -> Coefficients {
        let [b0, b1, b2, a1, a2] = self.c.map(|pair| pair[ch]);
        Coefficients { b0, b1, b2, a1, a2 }
    }
    fn state(&self, ch: usize) -> [f64; 2] {
        self.z.map(|pair| pair[ch])
    }
    fn set_state(&mut self, ch: usize, z: [f64; 2]) {
        (self.z[0][ch], self.z[1][ch]) = (z[0], z[1]);
    }
}
/// The filters of an EQ bank, both channels. A steady block touches `hot`
/// and `smoothing` only; each filter's checkpoint and last target are in
/// `cold`. `get` and `set` move a whole `Filter` out and in for everything
/// that is not a steady block.
#[derive(Clone, Copy)]
pub(crate) struct Filters {
    hot: [Hot; MAX_BANDS],
    smoothing: [[bool; MAX_BANDS]; 2],
    cold: [[[Coefficients; 2]; MAX_BANDS]; 2],
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            hot: [Hot::default(); MAX_BANDS],
            smoothing: [[false; MAX_BANDS]; 2],
            cold: [[[ZERO; 2]; MAX_BANDS]; 2],
        }
    }
}
impl Filters {
    pub fn get(&self, ch: usize, i: usize) -> Filter {
        let [z1, z2] = self.hot[i].state(ch);
        let [checkpoint, last_target] = self.cold[ch][i];
        Filter {
            c: self.hot[i].coefficients(ch),
            checkpoint,
            last_target,
            z1,
            z2,
            smoothing: self.smoothing[ch][i],
        }
    }
    pub fn set(&mut self, ch: usize, i: usize, filter: Filter) {
        for (pair, value) in self.hot[i].c.iter_mut().zip(filter.c.as_array()) {
            pair[ch] = value;
        }
        self.hot[i].set_state(ch, [filter.z1, filter.z2]);
        self.cold[ch][i] = [filter.checkpoint, filter.last_target];
        self.smoothing[ch][i] = filter.smoothing;
    }
    /// Whether `begin` found the filter's coefficients still on their way.
    pub fn is_smoothing(&self, ch: usize, i: usize) -> bool {
        self.smoothing[ch][i]
    }
    pub fn try_stereo_cascade(
        &mut self,
        active: &[[bool; MAX_BANDS]; 2],
        counts: [usize; 2],
        samples: &mut [[f64; BLOCK]; 2],
        n: usize,
        limit: f64,
    ) -> bool {
        stereo::process(self, active, counts, samples, n, limit)
    }
    /// Asks the cache for everything a steady block will touch, all at once,
    /// so the misses overlap instead of arriving one by one in the loop: the
    /// lines of each active band among the first `count`, each line once, and
    /// the first two lines of both sample planes. Nothing on other targets.
    pub fn prefetch(&self, active: &[[bool; MAX_BANDS]; 2], count: usize, x: &[[f64; BLOCK]; 2]) {
        let mut next = 0;
        for (i, hot) in self.hot.iter().enumerate().take(count) {
            if active[0][i] || active[1][i] {
                let start = hot as *const Hot as usize;
                next = next.max(start & !63);
                while next < start + size_of::<Hot>() {
                    prefetch_line(next as *const f64);
                    next += 64;
                }
            }
        }
        for plane in x {
            prefetch_line(plane.as_ptr());
            prefetch_line(plane[8..].as_ptr());
        }
    }
    /// Each active filter of `ch` in turn, as `Filter::process_block` would
    /// run it, bit for bit. Steady filters run two at a time, the second one
    /// sample behind the first, so the in-order A53 overlaps two independent
    /// recurrences.
    pub fn cascade(
        &mut self,
        ch: usize,
        active: &[bool],
        targets: &[Coefficients],
        x: &mut [f64],
        limit: f64,
        m: &mut Meter,
    ) {
        let mut order = [0; MAX_BANDS];
        let mut count = 0;
        for (index, _) in active.iter().enumerate().filter(|(_, on)| **on) {
            order[count] = index;
            count += 1;
        }
        let order = &order[..count];
        if count > 1 && x.len() <= BLOCK && order.iter().all(|&i| !self.smoothing[ch][i]) {
            let mut c = [ZERO; MAX_BANDS];
            let mut z = [[0.0; 2]; MAX_BANDS];
            let mut peak = [0.0; MAX_BANDS];
            for (k, &i) in order.iter().enumerate() {
                c[k] = self.hot[i].coefficients(ch);
                z[k] = self.hot[i].state(ch);
            }
            let mut saved = [0.0; BLOCK];
            saved[..x.len()].copy_from_slice(x);
            for k in (0..count).step_by(2) {
                if k + 1 < count {
                    let [first, second] = &mut z[k..k + 2] else {
                        unreachable!()
                    };
                    let [p0, p1] = &mut peak[k..k + 2] else {
                        unreachable!()
                    };
                    steady_two([&c[k], &c[k + 1]], [first, second], [p0, p1], x);
                } else {
                    let (z1, z2, p) = steady(c[k], z[k][0], z[k][1], x);
                    (z[k], peak[k]) = ([z1, z2], p);
                }
            }
            if (0..count).all(|k| clean(z[k][0], z[k][1], peak[k], limit)) {
                for (k, &i) in order.iter().enumerate() {
                    self.hot[i].set_state(ch, z[k]);
                }
                return;
            }
            x.copy_from_slice(&saved[..x.len()]);
        }
        for &i in order {
            let mut filter = self.get(ch, i);
            filter.process_block(&targets[i], x, limit, m);
            self.set(ch, i, filter);
        }
    }
}
/// A hint to load the cache line at `p`; nothing where there is no such hint.
#[inline(always)]
fn prefetch_line(p: *const f64) {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a prefetch reads nothing and cannot fault, whatever `p` is.
    unsafe {
        core::arch::asm!(
            "prfm pldl1keep, [{}]",
            in(reg) p,
            options(nostack, preserves_flags, readonly)
        );
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = p;
}
impl Filter {
    pub fn process(&mut self, target: &Coefficients, x: f64, limit: f64, m: &mut Meter) -> f64 {
        if self.smoothing {
            self.c.b0 += COEFF_SMOOTH * (target.b0 - self.c.b0);
            self.c.b1 += COEFF_SMOOTH * (target.b1 - self.c.b1);
            self.c.b2 += COEFF_SMOOTH * (target.b2 - self.c.b2);
            self.c.a1 += COEFF_SMOOTH * (target.a1 - self.c.a1);
            self.c.a2 += COEFF_SMOOTH * (target.a2 - self.c.a2);
        }
        // With `eq-fma` every product of the recurrence that feeds a sum is
        // fused into it, here and in `step`, as the NEON kernel does: fewer
        // operations on the mixer, and last bits that differ from the default
        // build's. Without it this is the code that was always here.
        #[cfg(not(feature = "eq-fma"))]
        let raw = self.c.b0 * x + self.z1;
        #[cfg(feature = "eq-fma")]
        let raw = self.c.b0.mul_add(x, self.z1);
        let y = safe(raw, limit, m);
        if y != raw {
            self.z1 = 0.0;
            self.z2 = 0.0;
            return y;
        }
        #[cfg(not(feature = "eq-fma"))]
        {
            self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
            self.z2 = self.c.b2 * x - self.c.a2 * y;
        }
        #[cfg(feature = "eq-fma")]
        {
            self.z1 = (-self.c.a1).mul_add(y, self.c.b1.mul_add(x, self.z2));
            self.z2 = (-self.c.a2).mul_add(y, self.c.b2 * x);
        }
        if !self.z1.is_finite() || !self.z2.is_finite() {
            self.z1 = 0.0;
            self.z2 = 0.0;
            m.clipped += 1;
            return 0.0;
        }
        if self.z1.abs() < 1e-25 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-25 {
            self.z2 = 0.0;
        }
        y
    }
}
/// A steady block passed through `process` unchanged: every raw output was
/// within the limit and the state stayed finite. A NaN output also leaves the
/// state NaN for the rest of the block, and an infinite state makes the next
/// output infinite, so the peak and the final state cover every sample.
fn clean(z1: f64, z2: f64, peak: f64, limit: f64) -> bool {
    peak <= limit && z1.is_finite() && z2.is_finite()
}
/// The steady `process` recurrence, without its exceptional-value handling.
#[inline(always)]
fn step(c: &Coefficients, z: &mut [f64; 2], input: f64, peak: &mut f64) -> f64 {
    #[cfg(not(feature = "eq-fma"))]
    let (raw, next1, next2) = {
        let raw = c.b0 * input + z[0];
        let next1 = c.b1 * input - c.a1 * raw + z[1];
        let next2 = c.b2 * input - c.a2 * raw;
        (raw, next1, next2)
    };
    #[cfg(feature = "eq-fma")]
    let (raw, next1, next2) = {
        let raw = c.b0.mul_add(input, z[0]);
        let next1 = (-c.a1).mul_add(raw, c.b1.mul_add(input, z[1]));
        let next2 = (-c.a2).mul_add(raw, c.b2 * input);
        (raw, next1, next2)
    };
    z[0] = if next1.abs() < 1e-25 { 0.0 } else { next1 };
    z[1] = if next2.abs() < 1e-25 { 0.0 } else { next2 };
    *peak = peak.max(raw.abs());
    raw
}
fn steady(c: Coefficients, z1: f64, z2: f64, x: &mut [f64]) -> (f64, f64, f64) {
    let (mut z, mut peak) = ([z1, z2], 0.0);
    for sample in x {
        *sample = step(&c, &mut z, *sample, &mut peak);
    }
    (z[0], z[1], peak)
}
/// Two cascaded filters in one pass: the second filter takes sample t - 1
/// from the first while the first takes sample t.
fn steady_two(c: [&Coefficients; 2], z: [&mut [f64; 2]; 2], peak: [&mut f64; 2], x: &mut [f64]) {
    let Some(&first) = x.first() else {
        return;
    };
    let ([c0, c1], [z0, z1], [p0, p1]) = (c, z, peak);
    let (mut s0, mut s1, mut q0, mut q1) = (*z0, *z1, *p0, *p1);
    let mut carry = step(c0, &mut s0, first, &mut q0);
    for t in 1..x.len() {
        let out = step(c1, &mut s1, carry, &mut q1);
        carry = step(c0, &mut s0, x[t], &mut q0);
        x[t - 1] = out;
    }
    let last = x.len() - 1;
    x[last] = step(c1, &mut s1, carry, &mut q1);
    (*z0, *z1, *p0, *p1) = (s0, s1, q0, q1);
}
pub(crate) fn safe(x: f64, limit: f64, m: &mut Meter) -> f64 {
    if !x.is_finite() {
        m.clipped += 1;
        return 0.0;
    }
    if x > limit {
        m.clipped += 1;
        return limit;
    }
    if x < -limit {
        m.clipped += 1;
        return -limit;
    }
    x
}
