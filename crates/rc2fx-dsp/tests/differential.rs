//! Linux-only frozen scalar-oracle regression. No compiler, child process or C
//! runtime dependency; fixture provenance is in fixtures/scalar-linux-v1/README.md.
//! Not built with `eq-fma`, which changes the last bits the oracle pins.
#![cfg(all(target_os = "linux", not(feature = "eq-fma")))]
use rc2fx_core::{Band, BandMode, BandType, Target, TargetKind};
use rc2fx_dsp::{Dsp, Prepared};

fn band(i: usize, right: bool) -> Band {
    Band {
        id: format!("b{i}"),
        enabled: true,
        band_type: BandType::ALL[i % 7],
        mode: if i % 2 == 1 {
            BandMode::RlcBt
        } else {
            BandMode::ApoDr
        },
        solo: false,
        frequency: 75. + 401. * i as f64 + if right { 73. } else { 0. },
        gain: (if i % 2 == 1 { -1. } else { 1. }) * (1.5 + i as f64),
        q: 0.41 + i as f64 * 0.23,
    }
}
fn config(block: usize, channels: u8) -> Target {
    let mut t = Target::new("oracle", "Oracle", TargetKind::Output, channels);
    t.highpass.enabled = true;
    t.highpass.frequency = 80.;
    t.eq.enabled = true;
    t.compressor.enabled = true;
    t.gain.enabled = true;
    t.limiter.enabled = true;
    t.compressor.makeup = 2.;
    t.gain.db = -3.;
    t.eq.input_gain_db = -4.;
    t.eq.output_gain_db = 2.;
    t.eq.bands = (0..7).map(|i| band(i, false)).collect();
    if channels == 2 {
        t.eq.right_bands = Some((0..6).map(|i| band(i, true)).collect());
    }
    if block >= 20 {
        t.highpass.frequency = 140.;
        t.gain.db = -9.;
        t.eq.bands[1].frequency = 510.;
        t.eq.bands[1].gain = -7.;
        t.eq.input_gain_db = -2.;
    }
    if block >= 40 {
        t.eq.bands.swap(1, 5);
    }
    if block >= 41 {
        t.eq.bands[2].solo = true;
    }
    if block >= 42 {
        t.eq.bands.truncate(5);
    }
    if block >= 60 {
        t.eq.right_bands = None;
    }
    if (80..90).contains(&block) {
        t.eq.enabled = false;
    }
    if (100..120).contains(&block) {
        t.bypass = true;
    }
    if (130..140).contains(&block) {
        t.compressor.enabled = false;
        t.limiter.enabled = false;
    }
    if block >= 150 {
        t.eq.input_gain_db = -36.;
        t.eq.output_gain_db = -36.;
        t.eq.bands = (0..32)
            .map(|i| {
                let mut b = band(i, false);
                b.frequency = 30. + i as f64 * 700.;
                b.gain = if i % 2 == 1 { -12. } else { 12. };
                b
            })
            .collect();
    }
    t
}
fn sample(frame: u32, ch: usize) -> f32 {
    if frame.is_multiple_of(997) {
        return f32::NAN;
    }
    if frame.is_multiple_of(991) {
        return f32::INFINITY;
    }
    if frame.is_multiple_of(983) {
        return -20.;
    }
    let q = ((frame
        .wrapping_mul(1664525)
        .wrapping_add(1013904223)
        .wrapping_add(ch as u32 * 97)
        >> 8)
        & 65535) as i32
        - 32768;
    q as f32 / 65536.
}
fn take<const N: usize>(bytes: &[u8], offset: &mut usize) -> [u8; N] {
    let result = bytes[*offset..*offset + N].try_into().unwrap();
    *offset += N;
    result
}

#[test]
fn frozen_scalar_oracle_matches_ordered_effects_stereo_and_live_eq_handoffs() {
    const {
        assert!(
            cfg!(all(
                any(target_arch = "x86_64", target_arch = "aarch64"),
                target_endian = "little"
            )),
            "This frozen oracle is qualified for Linux x86_64 and aarch64 little-endian only. \
             Qualify this target against the pinned oracle before adding it; do not skip or relax exact audio checks."
        );
    }
    // Exact bytes are SHA256-pinned in the fixture manifest. This lightweight
    // noncryptographic fingerprint also rejects accidental fixture edits in
    // ordinary tests without adding a hashing/library dependency to the DSP.
    fn fingerprint(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
    }
    for (channels, reference, bytes, expected_fingerprint) in [
        (
            1,
            include_bytes!("fixtures/scalar-linux-v1/mono.bin").as_slice(),
            181760,
            0xb1924889f355f00f_u64,
        ),
        (
            2,
            include_bytes!("fixtures/scalar-linux-v1/stereo.bin").as_slice(),
            354880,
            0x9048292ed71d92dc_u64,
        ),
    ] {
        assert_eq!(reference.len(), bytes, "truncated/extended oracle fixture");
        assert_eq!(
            fingerprint(reference),
            expected_fingerprint,
            "changed oracle fixture"
        );
        let p = Prepared::new(&config(0, channels), 48000).unwrap();
        let mut dsp = Dsp::without_suppression(&p).unwrap();
        let mut frame = 0;
        let mut offset = 0;
        let mut audio_differences = 0;
        let mut maximum = 0_f64;
        for block in 0..180 {
            let p = Prepared::new(&config(block, channels), 48000).unwrap();
            let n = [112, 7, 480, 1, 240, 63, 1024, 13][block % 8];
            let input: [Vec<f32>; 2] = std::array::from_fn(|ch| {
                if ch < channels as usize {
                    (0..n).map(|i| sample(frame + i as u32, ch)).collect()
                } else {
                    Vec::new()
                }
            });
            let mut output = [vec![0.; n], vec![0.; if channels == 2 { n } else { 0 }]];
            let [left, right] = &mut output;
            let m = dsp
                .process(&p, [&input[0], &input[1]], [left, right])
                .unwrap();
            for plane in output.iter().take(channels as usize) {
                for &actual in plane {
                    let expected = f32::from_le_bytes(take(reference, &mut offset));
                    let error = (actual as f64 - expected as f64).abs();
                    maximum = maximum.max(error);
                    audio_differences += usize::from(actual.to_bits() != expected.to_bits());
                    assert_eq!(
                        actual.to_bits(),
                        expected.to_bits(),
                        "audio mismatch block{block}: {actual} != {expected} ({error})"
                    );
                }
            }
            for actual in [m.input_peak, m.output_peak, m.sum_squares, m.gain_reduction] {
                let expected = f64::from_le_bytes(take(reference, &mut offset));
                assert!(
                    (actual - expected).abs() <= 1e-10 * expected.abs().max(1.),
                    "meter mismatch block{block}: {actual} != {expected}"
                );
            }
            assert_eq!(m.samples, u64::from_le_bytes(take(reference, &mut offset)));
            assert_eq!(m.clipped, u64::from_le_bytes(take(reference, &mut offset)));
            frame += n as u32;
        }
        assert_eq!(offset, reference.len());
        assert_eq!(frame, 43280);
        eprintln!(
            "channels={channels} samples={frame} nonidenticalF32={audio_differences} maxError={maximum}\n"
        );
    }
}
