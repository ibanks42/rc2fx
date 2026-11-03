//! AArch64 NEON chain: the two channels in the two lanes of one `f64x2`.
//!
//! The coefficients are shared between channels (checked bit-for-bit by the
//! caller), so one duplicated vector holds them for both lanes; each channel's
//! state and peak live in its own lane. The arithmetic is the same operation
//! sequence as the scalar `step` — `vmul`/`vadd`/`vsub` only, never `vfma`, and
//! no reassociation — so results are bit-identical to the per-channel path.
//! `vabs`/`vclt`/`vbsl` flush a state below `1e-25` to `+0`; `vmaxnm` matches
//! `f64::max` (maxNum) for the peak.
//!
//! NEON is part of the AArch64 baseline, so this module needs no
//! `target_feature` and no runtime detection. Every index used here is bounded
//! by the caller's eligibility checks (`n <= BLOCK`, `order.len() <= MAX_BANDS`).
use super::*;
use core::arch::aarch64::*;

/// `step`'s zero-flush threshold, the same literal the scalar path uses.
const TINY: f64 = 1e-25;

/// One band's coefficients, each duplicated into both lanes.
#[derive(Clone, Copy)]
struct Shared {
    b0: float64x2_t,
    b1: float64x2_t,
    b2: float64x2_t,
    a1: float64x2_t,
    a2: float64x2_t,
}

impl Shared {
    #[inline(always)]
    fn new(c: Coefficients) -> Self {
        // SAFETY: baseline NEON; no memory is touched.
        unsafe {
            Self {
                b0: vdupq_n_f64(c.b0),
                b1: vdupq_n_f64(c.b1),
                b2: vdupq_n_f64(c.b2),
                a1: vdupq_n_f64(c.a1),
                a2: vdupq_n_f64(c.a2),
            }
        }
    }
}

/// `[left, right]` from the two channels' scalars, lane 0 left.
#[inline(always)]
fn pack(left: f64, right: f64) -> float64x2_t {
    // SAFETY: baseline NEON; register operations only.
    unsafe { vcombine_f64(vdup_n_f64(left), vdup_n_f64(right)) }
}

#[inline(always)]
fn lane0(v: float64x2_t) -> f64 {
    // SAFETY: constant lane index.
    unsafe { vgetq_lane_f64::<0>(v) }
}

#[inline(always)]
fn lane1(v: float64x2_t) -> f64 {
    // SAFETY: constant lane index.
    unsafe { vgetq_lane_f64::<1>(v) }
}

/// The two channels' samples at `t`.
#[inline(always)]
fn input(x: &[[f64; BLOCK]; 2], t: usize) -> float64x2_t {
    pack(x[0][t], x[1][t])
}

/// Writes both lanes back to `x[0][t]` and `x[1][t]`.
#[inline(always)]
fn store(x: &mut [[f64; BLOCK]; 2], t: usize, v: float64x2_t) {
    x[0][t] = lane0(v);
    x[1][t] = lane1(v);
}

/// The scalar `step` on both lanes at once.
#[inline(always)]
fn step(
    c: &Shared,
    z1: &mut float64x2_t,
    z2: &mut float64x2_t,
    input: float64x2_t,
    peak: &mut float64x2_t,
) -> float64x2_t {
    // SAFETY: baseline NEON; register operations only. The expression order
    // matches the scalar `step` exactly, and no operation is fused.
    unsafe {
        let raw = vaddq_f64(vmulq_f64(c.b0, input), *z1);
        let next1 = vaddq_f64(vsubq_f64(vmulq_f64(c.b1, input), vmulq_f64(c.a1, raw)), *z2);
        let next2 = vsubq_f64(vmulq_f64(c.b2, input), vmulq_f64(c.a2, raw));
        let zero = vdupq_n_f64(0.0);
        let tiny = vdupq_n_f64(TINY);
        *z1 = vbslq_f64(vcltq_f64(vabsq_f64(next1), tiny), zero, next1);
        *z2 = vbslq_f64(vcltq_f64(vabsq_f64(next2), tiny), zero, next2);
        *peak = vmaxnmq_f64(*peak, vabsq_f64(raw));
        raw
    }
}

/// One band on both channels, samples written back in place (the odd tail).
fn tail(
    filters: &mut [[Filter; MAX_BANDS]; 2],
    index: usize,
    x: &mut [[f64; BLOCK]; 2],
    n: usize,
    limit: f64,
) -> bool {
    let c = Shared::new(filters[0][index].c);
    let mut z1 = pack(filters[0][index].z1, filters[1][index].z1);
    let mut z2 = pack(filters[0][index].z2, filters[1][index].z2);
    let mut peak = pack(0.0, 0.0);
    for t in 0..n {
        let out = step(&c, &mut z1, &mut z2, input(x, t), &mut peak);
        store(x, t, out);
    }
    filters[0][index].z1 = lane0(z1);
    filters[0][index].z2 = lane0(z2);
    filters[1][index].z1 = lane1(z1);
    filters[1][index].z2 = lane1(z2);
    let (left, right) = (
        clean(lane0(z1), lane0(z2), lane0(peak), limit),
        clean(lane1(z1), lane1(z2), lane1(peak), limit),
    );
    left && right
}

/// Two bands on both channels: `steady_two`'s carry across the pair, with the
/// channels in lanes.
fn pair(
    filters: &mut [[Filter; MAX_BANDS]; 2],
    first: usize,
    second: usize,
    x: &mut [[f64; BLOCK]; 2],
    n: usize,
    limit: f64,
) -> bool {
    let c0 = Shared::new(filters[0][first].c);
    let c1 = Shared::new(filters[0][second].c);
    let mut z0 = pack(filters[0][first].z1, filters[1][first].z1);
    let mut w0 = pack(filters[0][first].z2, filters[1][first].z2);
    let mut z1 = pack(filters[0][second].z1, filters[1][second].z1);
    let mut w1 = pack(filters[0][second].z2, filters[1][second].z2);
    let mut p0 = pack(0.0, 0.0);
    let mut p1 = pack(0.0, 0.0);
    let mut carry = step(&c0, &mut z0, &mut w0, input(x, 0), &mut p0);
    for t in 1..n {
        let out = step(&c1, &mut z1, &mut w1, carry, &mut p1);
        carry = step(&c0, &mut z0, &mut w0, input(x, t), &mut p0);
        store(x, t - 1, out);
    }
    store(x, n - 1, step(&c1, &mut z1, &mut w1, carry, &mut p1));
    filters[0][first].z1 = lane0(z0);
    filters[0][first].z2 = lane0(w0);
    filters[1][first].z1 = lane1(z0);
    filters[1][first].z2 = lane1(w0);
    filters[0][second].z1 = lane0(z1);
    filters[0][second].z2 = lane0(w1);
    filters[1][second].z1 = lane1(z1);
    filters[1][second].z2 = lane1(w1);
    clean(lane0(z0), lane0(w0), lane0(p0), limit)
        && clean(lane1(z0), lane1(w0), lane1(p0), limit)
        && clean(lane0(z1), lane0(w1), lane0(p1), limit)
        && clean(lane1(z1), lane1(w1), lane1(p1), limit)
}

/// The chain over `order`, stopping at the first unclean band like the scalar
/// path; the caller restores samples and state when this returns false.
///
/// # Safety
/// NEON intrinsics with baseline availability; `order` must be a subset of band
/// indices below `MAX_BANDS`, and `n <= BLOCK`. Both are guaranteed by
/// `process`'s eligibility checks.
pub(super) unsafe fn chain(
    filters: &mut [[Filter; MAX_BANDS]; 2],
    order: &[usize],
    x: &mut [[f64; BLOCK]; 2],
    n: usize,
    limit: f64,
) -> bool {
    let mut valid = true;
    for k in (0..order.len()).step_by(2) {
        valid = if k + 1 == order.len() {
            tail(filters, order[k], x, n, limit)
        } else {
            pair(filters, order[k], order[k + 1], x, n, limit)
        };
        if !valid {
            break;
        }
    }
    valid
}
