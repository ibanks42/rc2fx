//! AArch64 NEON chain: the two channels in the two lanes of one `f64x2`.
//!
//! A band's coefficients and state are stored as `[left, right]` pairs
//! (`Hot`), so each is one vector load, and the state one vector store. The
//! coefficients are the same bits in both lanes (checked by the caller); each
//! channel's state and peak live in its own lane. The arithmetic is the same
//! operation sequence as the scalar `step` — `vmul`/`vadd`/`vsub` only, never
//! `vfma`, and no reassociation — so results are bit-identical to the
//! per-channel path. With the `eq-fma` feature both fuse the same products
//! (`vfma`/`vfms` here, `f64::mul_add` there) and still agree bit for bit.
//! `vcalt`/`vbsl` flush a state below `1e-25` to `+0`: `vcalt` compares the
//! magnitudes in one instruction (FACGT), which is `abs(next) < 1e-25` exactly,
//! false for a NaN included. `vmaxnm` matches `f64::max` (maxNum) for the peak.
//!
//! The samples arrive interleaved, both channels of a sample side by side, so
//! each sample is one vector load and one vector store.
//!
//! NEON is part of the AArch64 baseline, so this module needs no
//! `target_feature` and no runtime detection. Every index used here is bounded
//! by the caller's eligibility checks (`order.len() <= MAX_BANDS`) and by the
//! length of the frame slice.
use super::*;
use core::arch::aarch64::*;

/// `step`'s zero-flush threshold, the same literal the scalar path uses.
const TINY: f64 = 1e-25;

/// One band's coefficients, the same in both lanes.
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
    fn new(c: &[[f64; 2]; 5]) -> Self {
        let [b0, b1, b2, a1, a2] = c.each_ref().map(load);
        Self { b0, b1, b2, a1, a2 }
    }
}

/// A stored `[left, right]` pair as a vector, lane 0 left.
#[inline(always)]
fn load(pair: &[f64; 2]) -> float64x2_t {
    // SAFETY: `pair` is two adjacent `f64`.
    unsafe { vld1q_f64(pair.as_ptr()) }
}

/// Writes both lanes back to a stored pair.
#[inline(always)]
fn save(pair: &mut [f64; 2], v: float64x2_t) {
    // SAFETY: `pair` is two adjacent `f64`.
    unsafe { vst1q_f64(pair.as_mut_ptr(), v) }
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

/// Both channels of one sample, left first.
pub(super) type Frame = [f64; 2];

/// The two channels' samples at `t`.
#[inline(always)]
fn input(x: &[Frame], t: usize) -> float64x2_t {
    // SAFETY: `x[t]` is two adjacent `f64`, and the index is checked.
    unsafe { vld1q_f64(x[t].as_ptr()) }
}

/// Writes both lanes back to sample `t`.
#[inline(always)]
fn store(x: &mut [Frame], t: usize, v: float64x2_t) {
    // SAFETY: `x[t]` is two adjacent `f64`, and the index is checked.
    unsafe { vst1q_f64(x[t].as_mut_ptr(), v) }
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
    // matches the scalar `step` exactly, and it fuses what `step` fuses:
    // nothing, or with `eq-fma` the same products.
    unsafe {
        let (raw, next1, next2);
        if cfg!(feature = "eq-fma") {
            raw = vfmaq_f64(*z1, c.b0, input);
            next1 = vfmsq_f64(vfmaq_f64(*z2, c.b1, input), c.a1, raw);
            next2 = vfmsq_f64(vmulq_f64(c.b2, input), c.a2, raw);
        } else {
            raw = vaddq_f64(vmulq_f64(c.b0, input), *z1);
            next1 = vaddq_f64(vsubq_f64(vmulq_f64(c.b1, input), vmulq_f64(c.a1, raw)), *z2);
            next2 = vsubq_f64(vmulq_f64(c.b2, input), vmulq_f64(c.a2, raw));
        }
        let zero = vdupq_n_f64(0.0);
        let tiny = vdupq_n_f64(TINY);
        // `vcaltq(a, b)` is `abs(a) < abs(b)`; `TINY` is positive.
        *z1 = vbslq_f64(vcaltq_f64(next1, tiny), zero, next1);
        *z2 = vbslq_f64(vcaltq_f64(next2, tiny), zero, next2);
        *peak = vmaxnmq_f64(*peak, vabsq_f64(raw));
        raw
    }
}

/// One band on both channels, samples written back in place (the odd tail).
fn tail(band: &mut Hot, x: &mut [Frame], limit: f64) -> bool {
    let c = Shared::new(&band.c);
    let [mut z1, mut z2] = band.z.each_ref().map(load);
    let mut peak = pack(0.0, 0.0);
    for t in 0..x.len() {
        let out = step(&c, &mut z1, &mut z2, input(x, t), &mut peak);
        store(x, t, out);
    }
    save(&mut band.z[0], z1);
    save(&mut band.z[1], z2);
    let (left, right) = (
        clean(lane0(z1), lane0(z2), lane0(peak), limit),
        clean(lane1(z1), lane1(z2), lane1(peak), limit),
    );
    left && right
}

/// Two bands on both channels: `steady_two`'s carry across the pair, with the
/// channels in lanes.
fn pair(
    bands: &mut [Hot; MAX_BANDS],
    first: usize,
    second: usize,
    x: &mut [Frame],
    limit: f64,
) -> bool {
    let n = x.len();
    let c0 = Shared::new(&bands[first].c);
    let c1 = Shared::new(&bands[second].c);
    let [mut z0, mut w0] = bands[first].z.each_ref().map(load);
    let [mut z1, mut w1] = bands[second].z.each_ref().map(load);
    let mut p0 = pack(0.0, 0.0);
    let mut p1 = pack(0.0, 0.0);
    let mut carry = step(&c0, &mut z0, &mut w0, input(x, 0), &mut p0);
    for t in 1..n {
        let out = step(&c1, &mut z1, &mut w1, carry, &mut p1);
        carry = step(&c0, &mut z0, &mut w0, input(x, t), &mut p0);
        store(x, t - 1, out);
    }
    store(x, n - 1, step(&c1, &mut z1, &mut w1, carry, &mut p1));
    save(&mut bands[first].z[0], z0);
    save(&mut bands[first].z[1], w0);
    save(&mut bands[second].z[0], z1);
    save(&mut bands[second].z[1], w1);
    clean(lane0(z0), lane0(w0), lane0(p0), limit)
        && clean(lane1(z0), lane1(w0), lane1(p0), limit)
        && clean(lane0(z1), lane0(w1), lane0(p1), limit)
        && clean(lane1(z1), lane1(w1), lane1(p1), limit)
}

/// The chain over `order` on the interleaved samples `x`, stopping at the first
/// unclean band like the scalar path; the caller restores the state when this
/// returns false.
///
/// # Safety
/// NEON intrinsics with baseline availability; `order` must be a subset of band
/// indices below `MAX_BANDS`, and `x` must not be empty. Both are guaranteed by
/// `process`'s eligibility checks.
pub(super) unsafe fn chain(
    bands: &mut [Hot; MAX_BANDS],
    order: &[usize],
    x: &mut [Frame],
    limit: f64,
) -> bool {
    let mut valid = true;
    for k in (0..order.len()).step_by(2) {
        valid = if k + 1 == order.len() {
            tail(&mut bands[order[k]], x, limit)
        } else {
            pair(bands, order[k], order[k + 1], x, limit)
        };
        if !valid {
            break;
        }
    }
    valid
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coefficients that hand the old second state to the first state and the
    /// input to the second state, so a test chooses both values the flush sees.
    const ROUTE: Coefficients = Coefficients {
        b0: 0.0,
        b1: 0.0,
        b2: 1.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn values() -> Vec<f64> {
        let below = f64::from_bits(TINY.to_bits() - 1);
        let above = f64::from_bits(TINY.to_bits() + 1);
        vec![
            TINY,
            below,
            above,
            -TINY,
            -below,
            -above,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            0.0,
            -0.0,
            1e-26,
            -1e-30,
            0.5,
            -3.0,
            1e300,
            f64::NAN,
            -f64::NAN,
            f64::from_bits(0x7ff0_0000_0000_0001),
            f64::INFINITY,
            f64::NEG_INFINITY,
        ]
    }

    /// Equal bits, or both NaN. When two different NaNs meet in one operation
    /// the result carries the payload of whichever operand the instruction
    /// was given first, and a compiler may order the operands of an addition
    /// or a multiplication either way. No caller reads a NaN's payload: any
    /// NaN leaves the state NaN, and `clean` then refuses the whole block.
    fn same(actual: f64, expected: f64) -> bool {
        actual.to_bits() == expected.to_bits() || (actual.is_nan() && expected.is_nan())
    }

    /// One NEON step with a different value in each lane, against the scalar
    /// `step` run once per lane.
    fn compare(c: Coefficients, first: [f64; 2], second: [f64; 2], sample: [f64; 2]) {
        let mut z1 = pack(first[0], first[1]);
        let mut z2 = pack(second[0], second[1]);
        let mut peak = pack(0.25, 0.25);
        let raw = step(
            &Shared::new(&c.as_array().map(|c| [c; 2])),
            &mut z1,
            &mut z2,
            pack(sample[0], sample[1]),
            &mut peak,
        );
        for (lane, get) in [lane0 as fn(float64x2_t) -> f64, lane1].iter().enumerate() {
            let mut z = [first[lane], second[lane]];
            let mut expected_peak = 0.25;
            let expected = crate::filter::step(&c, &mut z, sample[lane], &mut expected_peak);
            let context = format!(
                "lane {lane}: z {:e}/{:e} input {:e}",
                first[lane], second[lane], sample[lane]
            );
            assert!(same(get(raw), expected), "raw, {context}");
            assert!(same(get(z1), z[0]), "z1, {context}");
            assert!(same(get(z2), z[1]), "z2, {context}");
            assert!(same(get(peak), expected_peak), "peak, {context}");
            // What the caller decides from them is the same too.
            assert_eq!(
                clean(get(z1), get(z2), get(peak), 16.0),
                clean(z[0], z[1], expected_peak, 16.0),
                "clean, {context}"
            );
        }
    }

    #[test]
    fn the_flush_threshold_is_the_scalar_one_in_each_lane() {
        let values = values();
        // Every pair, so each special value also sits beside an ordinary one
        // in the other lane, in either lane.
        for &left in &values {
            for &right in &values {
                // The value reaches the first state through the second...
                compare(ROUTE, [0.0, 0.0], [left, right], [0.0, 0.0]);
                // ...and the second state as the input.
                compare(ROUTE, [0.0, 0.0], [0.0, 0.0], [left, right]);
                compare(ROUTE, [0.0, 0.0], [left, right], [right, left]);
            }
        }
        // The comparison is strict: exactly 1e-25 is kept, the next value
        // below it is flushed to positive zero, whatever its sign.
        let below = f64::from_bits(TINY.to_bits() - 1);
        let mut z1 = pack(0.0, 0.0);
        let mut z2 = pack(TINY, -below);
        let mut peak = pack(0.0, 0.0);
        step(
            &Shared::new(&ROUTE.as_array().map(|c| [c; 2])),
            &mut z1,
            &mut z2,
            pack(-TINY, below),
            &mut peak,
        );
        assert_eq!(lane0(z1).to_bits(), TINY.to_bits());
        assert_eq!(lane1(z1).to_bits(), 0.0_f64.to_bits());
        assert_eq!(lane0(z2).to_bits(), (-TINY).to_bits());
        assert_eq!(lane1(z2).to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn a_step_matches_the_scalar_one_for_ordinary_and_exceptional_values() {
        let c = Coefficients {
            b0: 0.7,
            b1: -0.2,
            b2: 0.1,
            a1: -0.3,
            a2: 0.15,
        };
        let values = values();
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut pick = |values: &[f64]| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            match seed % 3 {
                0 => values[(seed >> 8) as usize % values.len()],
                1 => ((seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 4e-25,
                _ => ((seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 4.0,
            }
        };
        for _ in 0..20_000 {
            let first = [pick(&values), pick(&values)];
            let second = [pick(&values), pick(&values)];
            let sample = [pick(&values), pick(&values)];
            compare(c, first, second, sample);
        }
    }
}
