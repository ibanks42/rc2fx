use super::*;

fn target(channels: u8, bypass: bool) -> Target {
    let mut target = Target::new("identity", "Identity", TargetKind::Playback, channels);
    target.bypass = bypass;
    target.eq.enabled = false;
    target.eq.input_gain_db = 12.0;
    target.eq.output_gain_db = -4.0;
    target.eq.bands = (0..MAX_BANDS)
        .map(|i| Band::new(format!("band-{i}")))
        .collect();
    target.gain.enabled = false;
    target.gain.db = -36.0;
    target.limiter.enabled = false;
    target
}

fn scalar_block(dsp: &mut Dsp, p: &Prepared, input: &[[f32; 112]; 2]) -> ([[f32; 112]; 2], Meter) {
    dsp.begin_block(p, 112);
    let mut output = [[0.0; 112]; 2];
    let mut meter = Meter::default();
    for n in 0..112 {
        let frame = dsp.frame(
            p,
            [
                input[0][n],
                if p.channels() == 2 { input[1][n] } else { 0.0 },
            ],
            &mut meter,
        );
        for ch in 0..p.channels() {
            output[ch][n] = frame[ch];
        }
    }
    (output, meter)
}

fn compare(
    actual: &mut Dsp,
    reference: &mut Dsp,
    p: &Prepared,
    input: &[[f32; 112]; 2],
    in_place: bool,
) {
    let (expected, expected_meter) = scalar_block(reference, p, input);
    let mut output = if in_place { *input } else { [[0.0; 112]; 2] };
    let [left, right] = &mut output;
    let meter = if in_place {
        actual
            .process_in_place(p, [left, if p.channels() == 2 { right } else { &mut [] }])
            .unwrap()
    } else {
        actual
            .process(
                p,
                [&input[0], if p.channels() == 2 { &input[1] } else { &[] }],
                [left, if p.channels() == 2 { right } else { &mut [] }],
            )
            .unwrap()
    };
    for ch in 0..p.channels() {
        assert_eq!(output[ch].map(f32::to_bits), expected[ch].map(f32::to_bits));
    }
    assert_eq!(meter, expected_meter);
    assert_eq!(
        meter.input_peak.to_bits(),
        expected_meter.input_peak.to_bits()
    );
    assert_eq!(
        meter.output_peak.to_bits(),
        expected_meter.output_peak.to_bits()
    );
    assert_eq!(
        meter.sum_squares.to_bits(),
        expected_meter.sum_squares.to_bits()
    );
    assert_eq!(
        [
            actual.gain,
            actual.wet,
            actual.processing_blend,
            actual.envelope,
            actual.limiter_gain
        ]
        .map(f64::to_bits),
        [
            reference.gain,
            reference.wet,
            reference.processing_blend,
            reference.envelope,
            reference.limiter_gain
        ]
        .map(f64::to_bits)
    );
    assert_eq!(
        (actual.eq_current, actual.eq_remaining),
        (reference.eq_current, reference.eq_remaining)
    );
}

#[test]
fn identity_matches_scalar_bits_and_exceptional_metering_for_both_buffer_forms() {
    let extremes = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        1.125,
        -1.125,
        16.0,
        -16.0,
        17.0,
        -17.0,
        f32::MAX,
        f32::MIN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
        f32::from_bits(1),
        -f32::from_bits(1),
        f32::from_bits(0x7f80_0001),
    ];
    for channels in [1, 2] {
        for bypass in [false, true] {
            for in_place in [false, true] {
                let prepared = Prepared::new(&target(channels, bypass), 48_000).unwrap();
                let mut actual = Dsp::without_suppression(&prepared).unwrap();
                let mut reference = Dsp::without_suppression(&prepared).unwrap();
                assert!(actual.steady_identity(&prepared));
                for block in 0..12 {
                    let input = std::array::from_fn(|ch| {
                        std::array::from_fn(|n| {
                            if block == 0 {
                                extremes[(n + ch * 7) % extremes.len()]
                            } else {
                                f32::from_bits(
                                    ((block * 112 + n + ch * 997) as u32)
                                        .wrapping_mul(2_654_435_761),
                                )
                            }
                        })
                    });
                    compare(&mut actual, &mut reference, &prepared, &input, in_place);
                }
            }
        }
    }
}

#[test]
fn identity_preserves_later_gain_eq_bypass_and_disabled_eq_gain_transitions() {
    for channels in [1, 2] {
        let mut target = target(channels, false);
        let initial = Prepared::new(&target, 48_000).unwrap();
        let mut actual = Dsp::without_suppression(&initial).unwrap();
        let mut reference = Dsp::without_suppression(&initial).unwrap();
        for block in 0..180 {
            match block {
                4 => target.eq.input_gain_db = -12.0,
                12 => {
                    target.eq.enabled = true;
                    target.eq.bands[0].gain = 5.0;
                }
                18 => target.eq.enabled = false,
                24 => target.bypass = true,
                84 => target.bypass = false,
                94 => {
                    target.gain.enabled = true;
                    target.gain.db = -6.0;
                }
                104 => target.gain.enabled = false,
                114 => target.eq.enabled = true,
                124 => {
                    target.eq.enabled = false;
                    target.eq.output_gain_db = 12.0;
                }
                _ => {}
            }
            let prepared = Prepared::new(&target, 48_000).unwrap();
            let input = std::array::from_fn(|ch| {
                std::array::from_fn(|n| {
                    (((block * 112 + n + ch * 131) % 997) as f32 - 498.0) / 128.0
                })
            });
            compare(
                &mut actual,
                &mut reference,
                &prepared,
                &input,
                block % 2 == 0,
            );
        }
    }
}

#[test]
fn identity_rejects_nonidentity_stages_and_unfinished_smoothing() {
    let base = target(2, false);
    for mutate in [
        |t: &mut Target| t.highpass.enabled = true,
        |t: &mut Target| t.compressor.enabled = true,
        |t: &mut Target| t.limiter.enabled = true,
        |t: &mut Target| t.eq.enabled = true,
        |t: &mut Target| {
            t.gain.enabled = true;
            t.gain.db = -6.0;
        },
    ] {
        let mut target = base.clone();
        mutate(&mut target);
        let prepared = Prepared::new(&target, 48_000).unwrap();
        assert!(
            !Dsp::without_suppression(&prepared)
                .unwrap()
                .steady_identity(&prepared)
        );
    }
    let prepared = Prepared::new(&base, 48_000).unwrap();
    for mutate in [
        |d: &mut Dsp| d.gain = 0.5,
        |d: &mut Dsp| d.wet = 0.5,
        |d: &mut Dsp| d.processing_blend = 0.5,
        |d: &mut Dsp| d.eq_remaining = 1,
        |d: &mut Dsp| d.latency = 1,
    ] {
        let mut actual = Dsp::without_suppression(&prepared).unwrap();
        mutate(&mut actual);
        assert!(!actual.steady_identity(&prepared));
    }
    let mut changed = base.clone();
    changed.eq.input_gain_db = -12.0;
    let changed = Prepared::new(&changed, 48_000).unwrap();
    let mut actual = Dsp::without_suppression(&prepared).unwrap();
    actual.begin_block(&changed, 112);
    assert!(!actual.steady_identity(&changed));
}

#[test]
fn identity_preserves_future_filter_and_detector_histories() {
    for effect in [Effect::Highpass, Effect::Compressor, Effect::Limiter] {
        let mut target = target(2, false);
        target.compressor.threshold = -30.0;
        target.limiter.ceiling = -12.0;
        let initial = Prepared::new(&target, 48_000).unwrap();
        let mut actual = Dsp::without_suppression(&initial).unwrap();
        let mut reference = Dsp::without_suppression(&initial).unwrap();
        for block in 0..36 {
            if block == 4 || block == 24 {
                target.set_effect_enabled(effect, true);
            } else if block == 14 {
                target.set_effect_enabled(effect, false);
            }
            let prepared = Prepared::new(&target, 48_000).unwrap();
            let input = std::array::from_fn(|ch| {
                std::array::from_fn(|n| {
                    (((block * 112 + n + ch * 131) % 997) as f32 - 498.0) / 512.0
                })
            });
            compare(
                &mut actual,
                &mut reference,
                &prepared,
                &input,
                block % 2 == 0,
            );
        }
    }
}

#[test]
fn every_stage_matches_the_scalar_frame_across_block_lengths() {
    let band = |i: usize, right: bool| Band {
        id: format!("band-{i}"),
        enabled: i != 7,
        band_type: [BandType::Bell, BandType::LowShelf, BandType::HighShelf][i % 3],
        frequency: (30.0 * 1.3_f64.powi(i as i32)).min(18_000.0) + if right { 11.0 } else { 0.0 },
        gain: if i.is_multiple_of(2) { -9.0 } else { 6.0 },
        q: 0.4 + 0.2 * i as f64,
        ..Band::new(format!("band-{i}"))
    };
    for channels in [1, 2] {
        let mut target = target(channels, false);
        target.eq.enabled = true;
        target.eq.input_gain_db = 12.0;
        target.eq.bands = (0..MAX_BANDS).map(|i| band(i, false)).collect();
        if channels == 2 {
            target.eq.right_bands = Some((0..MAX_BANDS).map(|i| band(i, true)).collect());
        }
        target.highpass.enabled = true;
        target.compressor.enabled = true;
        target.compressor.threshold = -24.0;
        target.gain.enabled = true;
        target.gain.db = 3.0;
        target.limiter.enabled = true;
        target.limiter.ceiling = -3.0;
        let initial = Prepared::new(&target, 48_000).unwrap();
        let mut actual = Dsp::without_suppression(&initial).unwrap();
        let mut reference = Dsp::without_suppression(&initial).unwrap();
        for block in 0..40 {
            match block {
                // Same shape: coefficients smooth towards the new values.
                6 => target.eq.bands[3].gain = 2.0,
                // New shape: the banks crossfade over several blocks.
                14 => target.eq.bands[7].enabled = true,
                22 => target.bypass = true,
                30 => target.bypass = false,
                _ => {}
            }
            let prepared = Prepared::new(&target, 48_000).unwrap();
            let frames = [112, 1, 300, 128, 129][block % 5];
            let input: [Vec<f32>; 2] = std::array::from_fn(|ch| {
                (0..frames)
                    .map(|n| match (block * 977 + n * 13 + ch) % 211 {
                        0 => f32::NAN,
                        1 => 40.0,
                        _ => ((block * 300 + n) as f32 * (0.031 + ch as f32 * 0.007)).sin() * 0.9,
                    })
                    .collect()
            });
            reference.begin_block(&prepared, frames);
            let mut expected = [vec![0.0; frames], vec![0.0; frames]];
            let mut expected_meter = Meter::default();
            for n in 0..frames {
                let y = reference.frame(
                    &prepared,
                    [input[0][n], if channels == 2 { input[1][n] } else { 0.0 }],
                    &mut expected_meter,
                );
                for ch in 0..channels as usize {
                    expected[ch][n] = y[ch];
                }
            }
            let mut output = input.clone();
            let [left, right] = &mut output;
            let meter = actual
                .process_in_place(
                    &prepared,
                    [left, if channels == 2 { right } else { &mut [] }],
                )
                .unwrap();
            for ch in 0..channels as usize {
                let bits = |v: &Vec<f32>| v.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&output[ch]), bits(&expected[ch]), "block {block}");
            }
            assert_eq!(meter, expected_meter, "block {block}");
            assert_eq!(
                meter.sum_squares.to_bits(),
                expected_meter.sum_squares.to_bits()
            );
            assert_eq!(
                (actual.envelope.to_bits(), actual.limiter_gain.to_bits()),
                (
                    reference.envelope.to_bits(),
                    reference.limiter_gain.to_bits()
                )
            );
            assert_eq!(
                (actual.eq_current, actual.eq_remaining),
                (reference.eq_current, reference.eq_remaining)
            );
        }
    }
}
