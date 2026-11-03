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
    pub fn try_stereo_cascade(
        filters: &mut [[Self; MAX_BANDS]; 2],
        active: &[[bool; MAX_BANDS]; 2],
        counts: [usize; 2],
        samples: &mut [[f64; BLOCK]; 2],
        n: usize,
        limit: f64,
    ) -> bool {
        stereo::process(filters, active, counts, samples, n, limit)
    }

    pub fn with_coefficients(c: Coefficients) -> Self {
        Self {
            c,
            ..Self::default()
        }
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
    /// Each active filter's `process_block` in turn, bit for bit. Steady
    /// filters run two at a time, the second one sample behind the first, so
    /// the in-order A53 overlaps two independent recurrences.
    pub fn cascade(
        filters: &mut [Self],
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
        if count > 1 && x.len() <= BLOCK && order.iter().all(|&i| !filters[i].smoothing) {
            let mut c = [ZERO; MAX_BANDS];
            let mut z = [[0.0; 2]; MAX_BANDS];
            let mut peak = [0.0; MAX_BANDS];
            for (k, &i) in order.iter().enumerate() {
                c[k] = filters[i].c;
                z[k] = [filters[i].z1, filters[i].z2];
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
                    (filters[i].z1, filters[i].z2) = (z[k][0], z[k][1]);
                }
                return;
            }
            x.copy_from_slice(&saved[..x.len()]);
        }
        for &i in order {
            filters[i].process_block(&targets[i], x, limit, m);
        }
    }
    pub fn process(&mut self, target: &Coefficients, x: f64, limit: f64, m: &mut Meter) -> f64 {
        if self.smoothing {
            self.c.b0 += COEFF_SMOOTH * (target.b0 - self.c.b0);
            self.c.b1 += COEFF_SMOOTH * (target.b1 - self.c.b1);
            self.c.b2 += COEFF_SMOOTH * (target.b2 - self.c.b2);
            self.c.a1 += COEFF_SMOOTH * (target.a1 - self.c.a1);
            self.c.a2 += COEFF_SMOOTH * (target.a2 - self.c.a2);
        }
        let raw = self.c.b0 * x + self.z1;
        let y = safe(raw, limit, m);
        if y != raw {
            self.z1 = 0.0;
            self.z2 = 0.0;
            return y;
        }
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
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
    let raw = c.b0 * input + z[0];
    let next1 = c.b1 * input - c.a1 * raw + z[1];
    let next2 = c.b2 * input - c.a2 * raw;
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
