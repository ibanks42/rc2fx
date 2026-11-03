//! The mixer supplies the sum between the two DSP layers. This checks their
//! signal composition and branch isolation, not physical JACK/socket mapping.
use rc2fx_core::{Band, Target, TargetKind};
use rc2fx_dsp::{Dsp, Prepared};
use std::f64::consts::TAU;

const RATE: usize = 48_000;

fn target(id: &str, kind: TargetKind) -> Target {
    let mut target = Target::new(id, id, kind, 2);
    target.eq.enabled = true;
    target
}

fn process(target: &Target, input: &[Vec<f32>; 2]) -> [Vec<f32>; 2] {
    let prepared = Prepared::new(target, RATE as u32).unwrap();
    let mut dsp = Dsp::without_suppression(&prepared).unwrap();
    assert_eq!(dsp.latency_samples(), 0);
    let mut output = [vec![0.0; RATE], vec![0.0; RATE]];
    // Exercise the real mixer period, including a final partial block.
    for offset in (0..RATE).step_by(112) {
        let end = (offset + 112).min(RATE);
        let [left, right] = &mut output;
        dsp.process(
            &prepared,
            [&input[0][offset..end], &input[1][offset..end]],
            [&mut left[offset..end], &mut right[offset..end]],
        )
        .unwrap();
    }
    output
}

fn tone(frequency: f64) -> [Vec<f32>; 2] {
    let left = (0..RATE)
        .map(|i| (0.1 * (TAU * frequency * i as f64 / RATE as f64).sin()) as f32)
        .collect::<Vec<_>>();
    let right = left.iter().map(|sample| sample * 0.5).collect();
    [left, right]
}

fn amplitude(samples: &[f32], frequency: f64) -> f64 {
    // Half a second of settled signal, with whole cycles at both test tones.
    let mut real = 0.0;
    let mut imag = 0.0;
    for (index, sample) in samples.iter().enumerate().skip(RATE / 2) {
        let angle = TAU * frequency * index as f64 / RATE as f64;
        real += f64::from(*sample) * angle.cos();
        imag += f64::from(*sample) * angle.sin();
    }
    real.hypot(imag) * 4.0 / RATE as f64
}

#[test]
fn source_eqs_then_headphone_correction_leave_other_output_unchanged() {
    let mut source_a = target("usb-main", TargetKind::Playback);
    let mut band = Band::new("source-a");
    band.frequency = 500.0;
    band.gain = -6.0;
    source_a.eq.bands.push(band);
    let mut source_b = target("usb-secondary", TargetKind::Playback);
    let mut band = Band::new("source-b");
    band.frequency = 2000.0;
    band.gain = -3.0;
    source_b.eq.bands.push(band);
    let original_sources = [source_a.clone(), source_b.clone()];
    let a = process(&source_a, &tone(500.0));
    let b = process(&source_b, &tone(2000.0));
    let mix = std::array::from_fn(|channel| {
        a[channel]
            .iter()
            .zip(&b[channel])
            .map(|(a, b)| a + b)
            .collect::<Vec<_>>()
    });
    let saved_mix = mix.clone();

    let mut headphones = target("headphones-1", TargetKind::Output);
    headphones.eq.input_gain_db = -9.0;
    let phones = process(&headphones, &mix);
    let other_output = process(&target("send-chat", TargetKind::Output), &mix);
    assert_eq!(other_output, saved_mix);
    assert_eq!(mix, saved_mix);
    assert_eq!([source_a, source_b], original_sources);

    for channel in 0..2 {
        for (frequency, source_gain) in [(500.0, -6.0), (2000.0, -3.0)] {
            let input_amplitude = if channel == 0 { 0.1 } else { 0.05 };
            let expected_source = input_amplitude * 10.0_f64.powf(source_gain / 20.0);
            let expected_phones = expected_source * 10.0_f64.powf(-9.0 / 20.0);
            assert!((amplitude(&other_output[channel], frequency) - expected_source).abs() < 1e-6);
            assert!((amplitude(&phones[channel], frequency) - expected_phones).abs() < 1e-6);
        }
    }

    headphones.bypass = true;
    assert_eq!(process(&headphones, &mix), saved_mix);
}
