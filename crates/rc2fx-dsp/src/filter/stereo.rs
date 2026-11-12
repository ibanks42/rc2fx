//! Experimental, exact-arithmetic stereo cascade scheduling.
//!
//! The eligibility and rollback here are architecture-independent; only the
//! steady chain differs: on AArch64 both channels ride in the two lanes of an
//! `f64x2` (the same coefficients in both lanes, explicit NEON ops, no
//! reassociation, and no FMA unless the `eq-fma` feature fuses the scalar
//! path too), elsewhere the scalar four-recurrence loop runs. The
//! scalar loop is the reference. Both reproduce `steady_two`'s per-sample
//! arithmetic exactly, so a refusal falls back to the per-channel path with no
//! change in audio.
use super::*;

#[cfg(target_arch = "aarch64")]
mod neon;

/// Try the steady, shared-coefficient stereo case. On failure, samples and
/// filter state are restored so the existing per-channel path can run.
pub(super) fn process(
    filters: &mut Filters,
    active: &[[bool; MAX_BANDS]; 2],
    counts: [usize; 2],
    x: &mut [[f64; BLOCK]; 2],
    n: usize,
    limit: f64,
) -> bool {
    if n > BLOCK || counts[0] != counts[1] || counts[0] > MAX_BANDS {
        return false;
    }
    let mut order = [0; MAX_BANDS];
    let mut count = 0;
    let (hot, smoothing) = (&filters.hot, &filters.smoothing);
    for i in 0..counts[0] {
        if active[0][i] != active[1][i] {
            return false;
        }
        if active[0][i] {
            if smoothing[0][i]
                || smoothing[1][i]
                || !same_bits(hot[i].coefficients(0), hot[i].coefficients(1))
            {
                return false;
            }
            order[count] = i;
            count += 1;
        }
    }
    if count < 2 {
        return false;
    }
    if n == 0 {
        return true;
    }
    let order = &order[..count];
    let mut original = [[[0.0; 2]; 2]; MAX_BANDS];
    for (saved, &i) in original.iter_mut().zip(order) {
        *saved = filters.hot[i].z;
    }
    #[cfg(not(target_arch = "aarch64"))]
    let valid = {
        let saved = *x;
        let valid = scalar_chain(filters, order, x, n, limit);
        if !valid {
            *x = saved;
        }
        valid
    };
    // The NEON chain works on a copy with the two channels side by side and
    // `x` takes the result only when every band was clean, so a refusal has
    // no samples to restore.
    #[cfg(target_arch = "aarch64")]
    let valid = {
        let [left, right] = x;
        let mut frames = [[0.0; 2]; BLOCK];
        let frames = &mut frames[..n];
        for (frame, (left, right)) in frames.iter_mut().zip(left.iter().zip(right.iter())) {
            *frame = [*left, *right];
        }
        // SAFETY: NEON is part of the AArch64 baseline; every lane and index
        // used by `neon::chain` stays inside the bands and `frames`, bounded
        // by the eligibility checks above, and no pointer outlives the call.
        let valid = unsafe { neon::chain(&mut filters.hot, order, frames, limit) };
        if valid {
            for (frame, (left, right)) in frames.iter().zip(left.iter_mut().zip(right)) {
                (*left, *right) = (frame[0], frame[1]);
            }
        }
        valid
    };
    if !valid {
        for (saved, &i) in original.iter().zip(order) {
            filters.hot[i].z = *saved;
        }
    }
    valid
}

/// The scalar four-recurrence chain: one channel's two bands per pass, two
/// channels in turn. The reference implementation of `process`'s contract.
#[cfg(not(target_arch = "aarch64"))]
fn scalar_chain(
    filters: &mut Filters,
    order: &[usize],
    x: &mut [[f64; BLOCK]; 2],
    n: usize,
    limit: f64,
) -> bool {
    let mut valid = true;
    for k in (0..order.len()).step_by(2) {
        let i = order[k];
        let c0 = filters.hot[i].coefficients(0);
        if k + 1 == order.len() {
            for (ch, x) in x.iter_mut().enumerate() {
                let [z1, z2] = filters.hot[i].state(ch);
                let (z1, z2, peak) = steady(c0, z1, z2, &mut x[..n]);
                valid &= clean(z1, z2, peak, limit);
                filters.hot[i].set_state(ch, [z1, z2]);
            }
        } else {
            let j = order[k + 1];
            let c1 = filters.hot[j].coefficients(0);
            let mut l0 = filters.hot[i].state(0);
            let mut l1 = filters.hot[j].state(0);
            let mut r0 = filters.hot[i].state(1);
            let mut r1 = filters.hot[j].state(1);
            let (mut p0, mut p1, mut p2, mut p3) = (0., 0., 0., 0.);
            let mut left = step(&c0, &mut l0, x[0][0], &mut p0);
            let mut right = step(&c0, &mut r0, x[1][0], &mut p2);
            for t in 1..n {
                let out_l = step(&c1, &mut l1, left, &mut p1);
                let out_r = step(&c1, &mut r1, right, &mut p3);
                left = step(&c0, &mut l0, x[0][t], &mut p0);
                right = step(&c0, &mut r0, x[1][t], &mut p2);
                x[0][t - 1] = out_l;
                x[1][t - 1] = out_r;
            }
            x[0][n - 1] = step(&c1, &mut l1, left, &mut p1);
            x[1][n - 1] = step(&c1, &mut r1, right, &mut p3);
            valid &= clean(l0[0], l0[1], p0, limit)
                && clean(l1[0], l1[1], p1, limit)
                && clean(r0[0], r0[1], p2, limit)
                && clean(r1[0], r1[1], p3, limit);
            for (ch, a, b) in [(0, l0, l1), (1, r0, r1)] {
                filters.hot[i].set_state(ch, a);
                filters.hot[j].set_state(ch, b);
            }
        }
        if !valid {
            break;
        }
    }
    valid
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests keep their filters as plain `Filter`s, a channel at a time,
    // as the bank stored them before its steady data moved into `Hot`. Each
    // call below moves them into a bank and back out, whatever the outcome.
    type Plain = [[Filter; MAX_BANDS]; 2];
    fn bank(filters: &Plain) -> Filters {
        let mut bank = Filters::default();
        for (ch, filters) in filters.iter().enumerate() {
            for (i, filter) in filters.iter().enumerate() {
                bank.set(ch, i, *filter);
            }
        }
        bank
    }
    fn process(
        filters: &mut Plain,
        active: &[[bool; MAX_BANDS]; 2],
        counts: [usize; 2],
        x: &mut [[f64; BLOCK]; 2],
        n: usize,
        limit: f64,
    ) -> bool {
        let mut bank = bank(filters);
        let done = super::process(&mut bank, active, counts, x, n, limit);
        *filters = std::array::from_fn(|ch| std::array::from_fn(|i| bank.get(ch, i)));
        done
    }
    /// One channel's `Filters::cascade`, run as the right channel so that a
    /// mix-up of the two lanes shows.
    fn cascade(
        filters: &mut [Filter],
        active: &[bool],
        targets: &[Coefficients],
        x: &mut [f64],
        limit: f64,
        m: &mut Meter,
    ) {
        let mut bank = Filters::default();
        for (i, filter) in filters.iter().enumerate() {
            bank.set(1, i, *filter);
        }
        bank.cascade(1, active, targets, x, limit, m);
        for (i, filter) in filters.iter_mut().enumerate() {
            *filter = bank.get(1, i);
        }
    }
    #[test]
    fn ineligible_stereo_cases_leave_samples_and_state_untouched() {
        let c = Coefficients {
            b0: 0.7,
            b1: 0.2,
            b2: 0.1,
            a1: -0.3,
            a2: 0.15,
        };
        for kind in 0..4 {
            let mut filters = [[Filter::with_coefficients(c); MAX_BANDS]; 2];
            let mut active = [[true; MAX_BANDS]; 2];
            let mut counts = [10; 2];
            match kind {
                0 => active[1][2] = false,
                1 => filters[1][2].smoothing = true,
                2 => filters[1][2].c.b0 += 0.01,
                _ => counts[1] = 9,
            }
            let original = filters;
            let mut samples = [[0.125; BLOCK]; 2];
            assert!(!process(
                &mut filters,
                &active,
                counts,
                &mut samples,
                112,
                16.
            ));
            assert_eq!(samples, [[0.125; BLOCK]; 2]);
            for (filters, original) in filters.iter().zip(&original) {
                for (filter, original) in filters.iter().zip(original) {
                    assert_eq!(filter.z1.to_bits(), original.z1.to_bits());
                    assert_eq!(filter.z2.to_bits(), original.z2.to_bits());
                    assert!(same_bits(filter.c, original.c));
                }
            }
        }
    }

    #[test]
    #[ignore = "host timing only; supply RC2FX_EQ_BENCH_STATE"]
    fn benchmark_stereo_cascade() {
        use std::{hint::black_box, time::Instant};
        let path = std::env::var("RC2FX_EQ_BENCH_STATE").expect("state fixture path");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let target: rc2fx_core::Target = serde_json::from_value(
            value["config"]["targets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == "headphones-1")
                .unwrap()
                .clone(),
        )
        .unwrap();
        let count = target.eq.bands.len();
        let mut coefficients = [Coefficients::IDENTITY; MAX_BANDS];
        let mut active = [[false; MAX_BANDS]; 2];
        for (i, band) in target.eq.bands.iter().enumerate() {
            coefficients[i] = rc2fx_core::eq::coefficients(band).unwrap();
            active[0][i] = band.enabled;
            active[1][i] = band.enabled;
        }
        let initial =
            bank(&[std::array::from_fn(|i| Filter::with_coefficients(coefficients[i])); 2]);
        let input: [[f64; BLOCK]; 2] = std::array::from_fn(|ch| {
            std::array::from_fn(|t| ((t + ch * 13) as f64 * 0.13).sin() * 0.1)
        });
        for round in 0..6 {
            for interleaved in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let mut filters = initial;
                let mut meter = Meter::default();
                let started = Instant::now();
                for _ in 0..20_000 {
                    let mut samples = black_box(input);
                    if interleaved {
                        assert!(super::process(
                            &mut filters,
                            &active,
                            [count; 2],
                            &mut samples,
                            112,
                            16.
                        ));
                    } else {
                        for ch in 0..2 {
                            filters.cascade(
                                ch,
                                &active[ch][..count],
                                &coefficients[..count],
                                &mut samples[ch][..112],
                                16.,
                                &mut meter,
                            );
                        }
                    }
                    black_box(samples);
                }
                println!(
                    "round={round} interleaved={interleaved} ns_per_stereo_block={:.1}",
                    started.elapsed().as_nanos() as f64 / 20_000.
                );
            }
        }
    }

    #[test]
    fn interleaving_matches_existing_cascade_including_fallback_and_next_block() {
        let c = Coefficients {
            b0: 0.7,
            b1: 0.2,
            b2: 0.1,
            a1: -0.3,
            a2: 0.15,
        };
        let target: [Coefficients; MAX_BANDS] = std::array::from_fn(|i| Coefficients {
            b0: c.b0 + i as f64 * 0.003,
            a1: c.a1 + i as f64 * 0.002,
            ..c
        });
        for count in [2, 3, 10, MAX_BANDS] {
            for n in [0, 1, 17, 112, BLOCK] {
                for bad in 0..6 {
                    let mut a = [std::array::from_fn(|i| Filter::with_coefficients(target[i])); 2];
                    let mut b = a;
                    let mut active = [[false; MAX_BANDS]; 2];
                    for mask in &mut active {
                        mask[..count].fill(true);
                    }
                    for block in 0..3 {
                        let mut x = [[0.; BLOCK]; 2];
                        for (ch, samples) in x.iter_mut().enumerate() {
                            for (t, sample) in samples.iter_mut().enumerate().take(n) {
                                *sample = ((t + ch * 7 + block * 13) as f64 * 0.13).sin() * 0.4;
                            }
                        }
                        if bad != 0 && n > 0 && block == 1 {
                            x[1][n / 2] = match bad {
                                1 => f64::INFINITY,
                                2 => f64::from_bits(0x7ff8_0000_0000_0042),
                                3 => 1e100,
                                4 => 1e-30,
                                _ => -0.0,
                            };
                        }
                        let mut y = x;
                        let mut ma = Meter::default();
                        let mut mb = Meter::default();
                        for ch in 0..2 {
                            cascade(
                                &mut a[ch][..count],
                                &active[ch][..count],
                                &target[..count],
                                &mut x[ch][..n],
                                16.,
                                &mut ma,
                            );
                        }
                        if !process(&mut b, &active, [count; 2], &mut y, n, 16.) {
                            for ch in 0..2 {
                                cascade(
                                    &mut b[ch][..count],
                                    &active[ch][..count],
                                    &target[..count],
                                    &mut y[ch][..n],
                                    16.,
                                    &mut mb,
                                );
                            }
                        }
                        for (ch, ((want, got), (earlier, later))) in
                            x.iter().zip(&y).zip(a.iter().zip(&b)).enumerate()
                        {
                            for (t, (want, got)) in want.iter().zip(got).enumerate().take(n) {
                                assert_eq!(
                                    want.to_bits(),
                                    got.to_bits(),
                                    "count={count} n={n} block={block} ch={ch} t={t}"
                                );
                            }
                            for (earlier, later) in earlier.iter().zip(later).take(count) {
                                assert_eq!(earlier.z1.to_bits(), later.z1.to_bits());
                                assert_eq!(earlier.z2.to_bits(), later.z2.to_bits());
                            }
                        }
                        assert_eq!(ma.clipped, mb.clipped);
                    }
                }
            }
        }
    }

    // -- Hardening: refusal rollback, boundary shapes, and the public DSP path. --

    /// A stable band: mild gain, poles well inside the unit circle.
    fn mild(i: usize) -> Coefficients {
        Coefficients {
            b0: 0.6 + i as f64 * 0.01,
            b1: 0.2,
            b2: 0.1,
            a1: -0.3,
            a2: 0.15,
        }
    }
    /// A band that pushes `raw` far past a small limit, so `clean` refuses.
    fn huge(i: usize) -> Coefficients {
        Coefficients {
            b0: 1.0e6,
            ..mild(i)
        }
    }
    fn chain(
        count: usize,
        coefficient: impl Fn(usize) -> Coefficients,
    ) -> [[Filter; MAX_BANDS]; 2] {
        assert!(count <= MAX_BANDS);
        std::array::from_fn(|_| {
            std::array::from_fn(|i| {
                let mut filter = Filter::with_coefficients(coefficient(i));
                // Non-zero, per-band, per-channel state: a rollback that misses
                // one band or one channel is visible.
                filter.z1 = 0.25 * (i as f64 + 1.0);
                filter.z2 = -0.125 * (i as f64 + 1.0);
                filter
            })
        })
    }
    fn mask(count: usize) -> [[bool; MAX_BANDS]; 2] {
        std::array::from_fn(|_| std::array::from_fn(|i| i < count))
    }
    /// Every band's state as bits, both channels, in a comparable shape.
    fn state(filters: &[[Filter; MAX_BANDS]; 2]) -> Vec<(u64, u64)> {
        filters
            .iter()
            .flat_map(|filters| filters.iter().map(|f| (f.z1.to_bits(), f.z2.to_bits())))
            .collect()
    }
    fn bits(samples: &[[f64; BLOCK]; 2]) -> Vec<u64> {
        samples.iter().flatten().map(|x| x.to_bits()).collect()
    }
    /// Deterministic samples, with a sentinel beyond `n` so an overlong write
    /// is visible.
    fn samples(n: usize) -> [[f64; BLOCK]; 2] {
        std::array::from_fn(|ch| {
            std::array::from_fn(|t| {
                if t < n {
                    ((t + ch * 7) as f64 * 0.13).sin() * 0.4
                } else {
                    7.5
                }
            })
        })
    }
    /// A failure in a later pair must restore the samples and the state of the
    /// pairs that already ran, so the fallback then behaves exactly as if the
    /// attempt had never happened.
    #[test]
    fn a_late_pair_failure_rolls_back_earlier_pairs_on_both_channels() {
        let count = 4;
        let targets: [Coefficients; MAX_BANDS] =
            std::array::from_fn(|i| if i == 3 { huge(i) } else { mild(i) });
        let active = mask(count);
        let mut filters = chain(count, |i| targets[i]);
        let pristine = filters;
        let mut x = samples(112);
        let reference = x;
        let before_state = state(&filters);
        let before_samples = bits(&x);
        assert!(!process(
            &mut filters,
            &active,
            [count; 2],
            &mut x,
            112,
            16.
        ));
        assert_eq!(bits(&x), before_samples, "samples restored");
        assert_eq!(
            state(&filters),
            before_state,
            "state restored, including the pair that completed"
        );
        // The refusal leaves no trace: the fallback output and state are those
        // of a pipeline that never attempted the interleaved path.
        let mut reference_x = reference;
        let mut reference_filters = pristine;
        let mut reference_meter = Meter::default();
        for ch in 0..2 {
            cascade(
                &mut reference_filters[ch][..count],
                &active[ch][..count],
                &targets[..count],
                &mut reference_x[ch][..112],
                16.,
                &mut reference_meter,
            );
        }
        let mut refused_meter = Meter::default();
        for ch in 0..2 {
            cascade(
                &mut filters[ch][..count],
                &active[ch][..count],
                &targets[..count],
                &mut x[ch][..112],
                16.,
                &mut refused_meter,
            );
        }
        assert_eq!(bits(&x), bits(&reference_x));
        assert_eq!(state(&filters), state(&reference_filters));
        assert_eq!(refused_meter.clipped, reference_meter.clipped);
    }

    /// The same, when the failing band is the unpaired tail of an odd chain.
    #[test]
    fn a_tail_failure_rolls_back_the_whole_odd_chain() {
        for count in [3, 9] {
            let targets: [Coefficients; MAX_BANDS] =
                std::array::from_fn(|i| if i + 1 == count { huge(i) } else { mild(i) });
            let active = mask(count);
            let mut filters = chain(count, |i| targets[i]);
            let mut x = samples(112);
            let before_state = state(&filters);
            let before_samples = bits(&x);
            assert!(
                !process(&mut filters, &active, [count; 2], &mut x, 112, 16.),
                "count={count}"
            );
            assert_eq!(bits(&x), before_samples, "count={count}: samples");
            assert_eq!(state(&filters), before_state, "count={count}: state");
        }
    }

    /// Random chains, lengths, states and input, different in each channel,
    /// through this scheduling and through the per-channel cascade: samples,
    /// state and clip counts must match, block after block. States start on
    /// either side of the flush threshold in one channel and not the other,
    /// and now and then one channel alone carries a value that refuses the
    /// whole block.
    #[test]
    fn random_chains_match_the_per_channel_cascade_in_each_channel() {
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let unit = |value: u64| (value >> 11) as f64 / (1u64 << 53) as f64;
        let tiny = 1e-25_f64;
        let (mut scheduled, mut refused, mut flushed) = (0, 0, 0);
        for round in 0..2500 {
            let count = 2 + (next() % (MAX_BANDS as u64 - 1)) as usize;
            let targets: [Coefficients; MAX_BANDS] = std::array::from_fn(|_| {
                let a2 = -0.3 + 0.8 * unit(next());
                Coefficients {
                    b0: 0.5 + 0.6 * unit(next()),
                    b1: -0.4 + 0.8 * unit(next()),
                    b2: -0.2 + 0.4 * unit(next()),
                    a1: (1.0 + a2) * 0.9 * (2.0 * unit(next()) - 1.0),
                    a2,
                }
            });
            // The same bands on both channels, at least two of them.
            let mut on = [false; MAX_BANDS];
            for (i, on) in on.iter_mut().enumerate().take(count) {
                *on = i < 2 || !next().is_multiple_of(4);
            }
            let active = [on; 2];
            let mut a = [std::array::from_fn(|i| Filter::with_coefficients(targets[i])); 2];
            for filter in a.iter_mut().flatten().take(2 * MAX_BANDS) {
                let mut pick = || match next() % 9 {
                    0 => tiny,
                    1 => f64::from_bits(tiny.to_bits() - 1),
                    2 => -f64::from_bits(tiny.to_bits() + 1),
                    3 => -1e-26,
                    4 => f64::from_bits(1 + next() % 1000),
                    5 => -0.0,
                    6 => 0.0,
                    _ => unit(next()) - 0.5,
                };
                filter.z1 = pick();
                filter.z2 = pick();
            }
            let mut b = a;
            let limit = if next().is_multiple_of(4) {
                16.0
            } else {
                crate::EQ_INTERNAL_LIMIT
            };
            let quiet = next().is_multiple_of(3);
            for block in 0..3 {
                let n = match next() % 4 {
                    0 => 112,
                    1 => BLOCK,
                    _ => 1 + (next() % BLOCK as u64) as usize,
                };
                let mut x = [[7.5; BLOCK]; 2];
                for samples in &mut x {
                    for sample in samples.iter_mut().take(n) {
                        *sample = match next() % 60 {
                            0 => -0.0,
                            1 => 1e-30,
                            2 => f64::from_bits(next() % 5000),
                            _ if quiet => 0.0,
                            _ => unit(next()) - 0.5,
                        };
                    }
                }
                if next().is_multiple_of(7) {
                    let channel = (next() % 2) as usize;
                    x[channel][(next() % n as u64) as usize] = match next() % 4 {
                        0 => f64::INFINITY,
                        1 => f64::from_bits(0x7ff8_0000_0000_0042),
                        2 => 1e300,
                        _ => -1e160,
                    };
                }
                let mut y = x;
                let (mut ma, mut mb) = (Meter::default(), Meter::default());
                for ch in 0..2 {
                    cascade(
                        &mut a[ch][..count],
                        &active[ch][..count],
                        &targets[..count],
                        &mut x[ch][..n],
                        limit,
                        &mut ma,
                    );
                }
                if process(&mut b, &active, [count; 2], &mut y, n, limit) {
                    scheduled += 1;
                } else {
                    refused += 1;
                    for ch in 0..2 {
                        cascade(
                            &mut b[ch][..count],
                            &active[ch][..count],
                            &targets[..count],
                            &mut y[ch][..n],
                            limit,
                            &mut mb,
                        );
                    }
                }
                let context = format!("round {round} block {block} count {count} n {n}");
                assert_eq!(bits(&y), bits(&x), "samples, {context}");
                assert_eq!(state(&b), state(&a), "state, {context}");
                assert_eq!(mb.clipped, ma.clipped, "clipped, {context}");
                flushed += a
                    .iter()
                    .flatten()
                    .filter(|f| f.z1.to_bits() == 0 && f.z2.to_bits() == 0)
                    .count();
            }
        }
        assert!(
            scheduled > 2000 && refused > 500 && flushed > 1000,
            "scheduled {scheduled} refused {refused} flushed {flushed}"
        );
    }

    /// A failure only on the right channel must restore the right channel, not
    /// just the left one that ran first in the same iteration.
    #[test]
    fn a_right_channel_only_failure_restores_the_right_channel() {
        let count = 4;
        let targets: [Coefficients; MAX_BANDS] = std::array::from_fn(mild);
        let active = mask(count);
        let mut filters = chain(count, |i| targets[i]);
        let mut x = samples(112);
        x[1][5] = 1e6;
        let before_state = state(&filters);
        let before_samples = bits(&x);
        assert!(!process(
            &mut filters,
            &active,
            [count; 2],
            &mut x,
            112,
            16.
        ));
        assert_eq!(bits(&x), before_samples);
        let after = state(&filters);
        assert_eq!(after, before_state, "both channels restored");
        // The right channel alone is enough to make the call refuse: the same
        // samples with the right channel quiet complete.
        let mut quiet = samples(112);
        let mut quiet_filters = chain(count, |i| targets[i]);
        assert!(process(
            &mut quiet_filters,
            &active,
            [count; 2],
            &mut quiet,
            112,
            16.
        ));
    }

    /// Both paths flush a state below `1e-25` to exactly `+0`, which no
    /// normal-sized signal can reveal: `step`'s zeroing is exact, not gradual.
    #[test]
    fn a_sub_threshold_state_is_flushed_to_positive_zero() {
        for count in [2, 3] {
            let targets: [Coefficients; MAX_BANDS] = std::array::from_fn(mild);
            let active = mask(count);
            let mut filters = chain(count, |i| targets[i]);
            for (ch, filters) in filters.iter_mut().enumerate() {
                for filter in filters.iter_mut().take(count) {
                    let tiny = if ch == 0 { 1e-26 } else { -1e-26 };
                    filter.z1 = tiny;
                    filter.z2 = -tiny;
                }
            }
            let mut x = [[0.0; BLOCK]; 2];
            assert!(
                process(&mut filters, &active, [count; 2], &mut x, 4, 16.),
                "count={count}"
            );
            for filters in &filters {
                for filter in filters.iter().take(count) {
                    assert_eq!(
                        filter.z1.to_bits(),
                        0.0f64.to_bits(),
                        "count={count}: z1 not +0"
                    );
                    assert_eq!(
                        filter.z2.to_bits(),
                        0.0f64.to_bits(),
                        "count={count}: z2 not +0"
                    );
                }
            }
        }
    }

    /// `n` beyond a block is refused before anything is read or written.
    #[test]
    fn a_block_length_beyond_storage_is_refused_untouched() {
        let count = 2;
        let targets: [Coefficients; MAX_BANDS] = std::array::from_fn(mild);
        let active = mask(count);
        let mut filters = chain(count, |i| targets[i]);
        let mut x = samples(BLOCK);
        let before_state = state(&filters);
        let before_samples = bits(&x);
        assert!(!process(
            &mut filters,
            &active,
            [count; 2],
            &mut x,
            BLOCK + 1,
            16.
        ));
        assert_eq!(bits(&x), before_samples);
        assert_eq!(state(&filters), before_state);
    }

    /// A single active band has no pair and is left to the per-channel path.
    #[test]
    fn a_single_active_band_is_not_scheduled_here() {
        let active = mask(1);
        let targets: [Coefficients; MAX_BANDS] = std::array::from_fn(mild);
        let mut filters = chain(1, |i| targets[i]);
        let mut x = samples(112);
        let before = (bits(&x), state(&filters));
        assert!(!process(&mut filters, &active, [1; 2], &mut x, 112, 16.));
        assert_eq!((bits(&x), state(&filters)), before);
    }

    /// A successful partial block writes exactly `n` samples and leaves the
    /// rest of the block untouched.
    #[test]
    fn a_partial_block_writes_only_its_samples() {
        let count = 4;
        let targets: [Coefficients; MAX_BANDS] = std::array::from_fn(mild);
        let active = mask(count);
        let mut filters = chain(count, |i| targets[i]);
        let mut x = samples(17);
        let before = x;
        assert!(process(&mut filters, &active, [count; 2], &mut x, 17, 16.));
        for (ch, (before, after)) in before.iter().zip(&x).enumerate() {
            assert_eq!(before[17..], after[17..], "channel {ch} tail");
            assert_ne!(before[..17], after[..17], "channel {ch} processed");
        }
    }

    mod public_dsp {
        //! The stereo scheduling through the public `Dsp`, not just the filter.
        use crate::{Dsp, Prepared};
        use rc2fx_core::{Band, BandType, Target, TargetKind};

        fn band(i: usize) -> Band {
            Band {
                id: format!("band-{i}"),
                enabled: true,
                band_type: [BandType::Bell, BandType::LowShelf, BandType::HighShelf][i % 3],
                frequency: 40.0 * 1.6_f64.powi(i as i32),
                gain: if i.is_multiple_of(2) { 4.0 } else { -5.0 },
                q: 0.7 + 0.05 * i as f64,
                ..Band::new(format!("band-{i}"))
            }
        }
        /// Ten identical bands on both channels, or one channel's bands disabled so
        /// the stereo scheduling refuses and the per-channel path runs instead.
        fn target(right_enabled: bool, left_enabled: bool) -> Target {
            let mut target = Target::new("eq", "EQ", TargetKind::Playback, 2);
            target.bypass = false;
            target.eq.enabled = true;
            target.eq.input_gain_db = 0.0;
            target.eq.output_gain_db = 0.0;
            // Ten bands a side: the headphone fixture's count, and even, so every
            // band is paired.
            target.eq.bands = (0..10)
                .map(|i| {
                    let mut band = band(i);
                    band.enabled = left_enabled;
                    band
                })
                .collect();
            target.eq.right_bands = Some(
                (0..10)
                    .map(|i| {
                        let mut band = band(i);
                        band.enabled = right_enabled;
                        band
                    })
                    .collect(),
            );
            target.highpass.enabled = false;
            target.compressor.enabled = false;
            target.gain.enabled = false;
            target.limiter.enabled = false;
            target
        }
        fn block(seed: usize) -> [[f32; 112]; 2] {
            std::array::from_fn(|ch| {
                std::array::from_fn(|t| {
                    ((((t + seed * 17 + ch * 101) % 997) as f32 - 498.0) / 512.0).sin() * 0.5
                })
            })
        }
        fn run(prepared: &Prepared, dsp: &mut Dsp, seed: usize) -> [[f32; 112]; 2] {
            let input = block(seed);
            let mut output = [[0.0; 112]; 2];
            let [left, right] = &mut output;
            dsp.process(prepared, [&input[0], &input[1]], [left, right])
                .unwrap();
            output
        }
        #[test]
        fn the_left_channel_is_bit_exact_whether_or_not_the_stereo_path_runs() {
            let both = Prepared::new(&target(true, true), 48_000).unwrap();
            let right_off = Prepared::new(&target(false, true), 48_000).unwrap();
            let mut both_dsp = Dsp::without_suppression(&both).unwrap();
            let mut fallback_dsp = Dsp::without_suppression(&right_off).unwrap();
            let mut moved = false;
            for seed in 0..8 {
                let scheduled = run(&both, &mut both_dsp, seed);
                let fallback = run(&right_off, &mut fallback_dsp, seed);
                assert_eq!(
                    scheduled[0].map(f32::to_bits),
                    fallback[0].map(f32::to_bits),
                    "seed {seed}"
                );
                moved |= scheduled[0] != block(seed)[0];
            }
            assert!(
                moved,
                "the EQ must change the signal for this to be evidence"
            );
        }
        #[test]
        fn the_right_channel_is_bit_exact_whether_or_not_the_stereo_path_runs() {
            let both = Prepared::new(&target(true, true), 48_000).unwrap();
            let left_off = Prepared::new(&target(true, false), 48_000).unwrap();
            let mut both_dsp = Dsp::without_suppression(&both).unwrap();
            let mut fallback_dsp = Dsp::without_suppression(&left_off).unwrap();
            for seed in 0..8 {
                let scheduled = run(&both, &mut both_dsp, seed);
                let fallback = run(&left_off, &mut fallback_dsp, seed);
                assert_eq!(
                    scheduled[1].map(f32::to_bits),
                    fallback[1].map(f32::to_bits),
                    "seed {seed}"
                );
            }
        }
    }
}
